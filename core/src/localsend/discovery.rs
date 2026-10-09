//! Multicast discovery (UDP 224.0.0.167:53317). The socket is shared so
//! another LocalSend app on the same Mac can run beside Pulse.

use super::net;
use super::proto::{self, DeviceInfo};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// IPv4 addresses of this Mac's network interfaces, loopback excluded.
pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut found = Vec::new();
    for (_, data) in networks.iter() {
        for network in data.ip_networks() {
            if let IpAddr::V4(ip) = network.addr
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && !(cfg!(windows) && ip.is_link_local())
                && !found.contains(&ip)
            {
                found.push(ip);
            }
        }
    }
    found
}

#[cfg(unix)]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain socket calls; the descriptor is closed on every error path
    // and otherwise handed to UdpSocket, which owns it from then on.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let one: libc::c_int = 1;
        for option in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
            let rc = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
            if rc != 0 {
                let error = io::Error::last_os_error();
                libc::close(fd);
                return Err(error);
            }
        }
        let mut address: libc::sockaddr_in = std::mem::zeroed();
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_port = port.to_be();
        address.sin_addr = libc::in_addr { s_addr: 0 };
        #[cfg(target_os = "macos")]
        {
            address.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        }
        let rc = libc::bind(
            fd,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        );
        if rc != 0 {
            let error = io::Error::last_os_error();
            libc::close(fd);
            return Err(error);
        }
        Ok(UdpSocket::from_raw_fd(fd))
    }
}

/// Winsock calls the standard library does not expose. Winsock itself is
/// already started by the first `std::net` socket (see `bind_shared`).
#[cfg(windows)]
mod winsock {
    use std::ffi::{c_char, c_int, c_void};

    pub type Socket = usize;
    pub const INVALID_SOCKET: Socket = !0;
    pub const AF_INET: c_int = 2;
    pub const SOCK_DGRAM: c_int = 2;
    pub const IPPROTO_UDP: c_int = 17;
    pub const IPPROTO_IP: c_int = 0;
    pub const SOL_SOCKET: c_int = 0xffff;
    pub const SO_REUSEADDR: c_int = 4;
    pub const IP_MULTICAST_IF: c_int = 9;

    #[repr(C)]
    pub struct SockAddrIn {
        pub family: u16,
        pub port: u16,
        pub addr: [u8; 4],
        pub zero: [u8; 8],
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        pub fn socket(af: c_int, kind: c_int, protocol: c_int) -> Socket;
        pub fn setsockopt(
            s: Socket,
            level: c_int,
            name: c_int,
            value: *const c_char,
            length: c_int,
        ) -> c_int;
        pub fn bind(s: Socket, name: *const c_void, length: c_int) -> c_int;
        pub fn closesocket(s: Socket) -> c_int;
    }
}

/// Windows lets several sockets share a UDP port only when each one asks with
/// SO_REUSEADDR before binding, which `UdpSocket::bind` cannot do.
#[cfg(windows)]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    use std::os::windows::io::FromRawSocket;
    use winsock::*;
    // A throwaway socket makes the standard library start Winsock (WSAStartup),
    // which the raw `socket` call below relies on.
    drop(UdpSocket::bind(("127.0.0.1", 0))?);
    // SAFETY: plain Winsock calls; the socket is closed on every error path and
    // otherwise handed to UdpSocket, which owns it from then on.
    unsafe {
        let s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
        if s == INVALID_SOCKET {
            return Err(io::Error::last_os_error());
        }
        let one: i32 = 1;
        if setsockopt(
            s,
            SOL_SOCKET,
            SO_REUSEADDR,
            (&one as *const i32).cast(),
            std::mem::size_of::<i32>() as i32,
        ) != 0
        {
            let error = io::Error::last_os_error();
            closesocket(s);
            return Err(error);
        }
        let address = SockAddrIn {
            family: AF_INET as u16,
            port: port.to_be(),
            addr: [0; 4],
            zero: [0; 8],
        };
        if bind(
            s,
            (&address as *const SockAddrIn).cast(),
            std::mem::size_of::<SockAddrIn>() as i32,
        ) != 0
        {
            let error = io::Error::last_os_error();
            closesocket(s);
            return Err(error);
        }
        Ok(UdpSocket::from_raw_socket(s as u64))
    }
}

#[cfg(not(any(unix, windows)))]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    UdpSocket::bind(("0.0.0.0", port))
}

/// Send multicast through one interface.
#[cfg(unix)]
fn use_interface(socket: &UdpSocket, ip: Ipv4Addr) {
    use std::os::fd::AsRawFd;
    let address = libc::in_addr {
        s_addr: u32::from(ip).to_be(),
    };
    // SAFETY: the option value is a live in_addr of the stated size.
    unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            (&address as *const libc::in_addr).cast(),
            std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
        );
    }
}

#[cfg(windows)]
fn use_interface(socket: &UdpSocket, ip: Ipv4Addr) {
    use std::os::windows::io::AsRawSocket;
    use winsock::{setsockopt, Socket, IPPROTO_IP, IP_MULTICAST_IF};
    let octets = ip.octets();
    // SAFETY: the option value is a live 4-byte in_addr (network byte order).
    unsafe {
        setsockopt(
            socket.as_raw_socket() as Socket,
            IPPROTO_IP,
            IP_MULTICAST_IF,
            octets.as_ptr().cast(),
            4,
        );
    }
}

#[cfg(not(any(unix, windows)))]
fn use_interface(_socket: &UdpSocket, _ip: Ipv4Addr) {}

/// Join the group on every interface that has an address. An interface that is
/// already joined answers with an error, which is ignored; returns whether any
/// join worked. Called again from time to time, because Wi-Fi comes and goes
/// (sleep, roaming) and a membership does not survive its interface.
pub fn rejoin(socket: &UdpSocket) -> bool {
    let mut joined = false;
    for ip in &local_ipv4s() {
        joined |= socket
            .join_multicast_v4(&proto::MULTICAST_GROUP, ip)
            .is_ok();
    }
    joined
}

/// A socket joined to the group on every interface.
pub fn listen(port: u16) -> io::Result<UdpSocket> {
    let socket = bind_shared(port)?;
    let joined = rejoin(&socket);
    if !joined {
        socket.join_multicast_v4(&proto::MULTICAST_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    }
    let _ = socket.set_multicast_loop_v4(true);
    Ok(socket)
}

/// Announce (or answer, with `announce: false`) to the group, out of every
/// interface. Returns whether any send worked. A refusal with "no route to
/// host" is what macOS gives when Local Network access is denied.
pub fn announce(me: &DeviceInfo, port: u16, announce: bool) -> Result<(), io::Error> {
    let mut info = me.clone();
    info.announce = Some(announce);
    let payload = serde_json::to_vec(&info).map_err(io::Error::other)?;
    let target = SocketAddr::V4(SocketAddrV4::new(proto::MULTICAST_GROUP, port));
    let interfaces = local_ipv4s();
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    let _ = socket.set_multicast_ttl_v4(1);
    if interfaces.is_empty() {
        return socket.send_to(&payload, target).map(|_| ());
    }
    let mut result: Result<(), io::Error> = Err(io::Error::other("no interface"));
    for ip in interfaces {
        use_interface(&socket, ip);
        match socket.send_to(&payload, target) {
            Ok(_) => result = Ok(()),
            Err(e) if result.is_err() => result = Err(e),
            Err(_) => {}
        }
    }
    result
}

/// A device heard on the network, with the address it spoke from.
pub struct Heard {
    pub info: DeviceInfo,
    pub ip: IpAddr,
}

pub fn parse(packet: &[u8], from: SocketAddr) -> Option<Heard> {
    let info: DeviceInfo = serde_json::from_slice(packet).ok()?;
    if info.alias.is_empty() {
        return None;
    }
    Some(Heard {
        info,
        ip: from.ip(),
    })
}

/// Announce, then listen for `window`, collecting every device that answers
/// or announces itself. For the command line, which runs no server.
pub fn scan(me: &DeviceInfo, window: Duration) -> io::Result<Vec<Heard>> {
    let socket = listen(proto::PORT)?;
    socket.set_read_timeout(Some(Duration::from_millis(150)))?;
    let started = Instant::now();
    let mut heard: Vec<Heard> = Vec::new();
    let mut announced: u64 = 0;
    let mut buffer = vec![0u8; 64 * 1024];
    while started.elapsed() < window {
        if announced < 3 && started.elapsed() >= Duration::from_millis(announced * 500) {
            let _ = announce(me, proto::PORT, true);
            announced += 1;
        }
        match socket.recv_from(&mut buffer) {
            Ok((n, from)) => {
                if let Some(device) = parse(&buffer[..n], from)
                    && device.info.fingerprint != me.fingerprint
                    && !heard
                        .iter()
                        .any(|h| h.info.fingerprint == device.info.fingerprint)
                {
                    heard.push(device);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(heard)
}

/// Interfaces that never carry a LAN: loopback, VPN and point-to-point tunnels,
/// Apple's peer-to-peer links.
fn skips_interface(name: &str) -> bool {
    if cfg!(windows) {
        // Windows names adapters for people ("Ethernet", "Wi-Fi", "vEthernet (WSL)").
        // Virtual switches of WSL and Hyper-V's Default Switch, tunnels and VPN
        // adapters are never the LAN.
        let name = name.to_ascii_lowercase();
        return [
            "loopback",
            "pseudo",
            "isatap",
            "teredo",
            "6to4",
            "wsl",
            "default switch",
            "bluetooth",
            "wireguard",
            "tailscale",
            "tap-windows",
            "openvpn",
            "vpn",
        ]
        .iter()
        .any(|word| name.contains(word));
    }
    ["lo", "utun", "ppp", "ipsec", "gif", "stf", "awdl", "llw"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Every host worth probing: the private networks this Mac is on, narrowed to
/// at most a /24 around its own address, without this Mac's own addresses.
pub fn sweep_targets() -> Vec<Ipv4Addr> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let own = local_ipv4s();
    let mut targets: Vec<Ipv4Addr> = Vec::new();
    for (name, data) in networks.iter() {
        if skips_interface(name) {
            continue;
        }
        for network in data.ip_networks() {
            let IpAddr::V4(ip) = network.addr else {
                continue;
            };
            if !ip.is_private() || network.prefix >= 31 {
                continue;
            }
            let prefix = u32::from(network.prefix).max(24);
            let base = u32::from(ip) & (u32::MAX << (32 - prefix));
            for offset in 1..(1u32 << (32 - prefix)) - 1 {
                let host = Ipv4Addr::from(base + offset);
                if !own.contains(&host) && !targets.contains(&host) {
                    targets.push(host);
                }
            }
        }
    }
    targets
}

/// One request to a LocalSend port, answered with the device's own info.
fn exchange(
    ip: Ipv4Addr,
    https: bool,
    method: &str,
    endpoint: &str,
    body: Option<&[u8]>,
) -> Option<DeviceInfo> {
    let mut wire = net::connect_within(
        IpAddr::V4(ip),
        proto::PORT,
        https,
        "",
        Duration::from_secs(1),
        Duration::from_secs(2),
    )
    .ok()?;
    let payload = body.unwrap_or(&[]);
    let reply = net::call(
        &mut wire,
        method,
        &format!("{ip}:{}", proto::PORT),
        &format!("{}{endpoint}", proto::API),
        body.map(|_| "application/json"),
        payload.len() as u64,
        &mut |w| w.write_all(payload),
    );
    wire.finish();
    let reply = reply.ok().filter(|r| r.status == 200)?;
    let mut info: DeviceInfo = serde_json::from_slice(&reply.body).ok()?;
    if info.alias.is_empty() || info.fingerprint.is_empty() {
        return None;
    }
    info.port = proto::PORT;
    info.protocol = if https { "https" } else { "http" }.to_string();
    Some(info)
}

/// Ask one address whether a LocalSend device lives there, the way the
/// LocalSend app's HTTP discovery does: register with it (so it lists this Mac
/// too) and fall back to reading its info. https first, then http.
pub fn probe(me: &DeviceInfo, ip: Ipv4Addr) -> Option<DeviceInfo> {
    let address = SocketAddr::new(IpAddr::V4(ip), proto::PORT);
    TcpStream::connect_timeout(&address, Duration::from_secs(1)).ok()?;
    let mut mine = me.clone();
    mine.announce = None;
    let body = serde_json::to_vec(&mine).ok()?;
    for https in [true, false] {
        let found = exchange(ip, https, "POST", "/register", Some(&body))
            .or_else(|| exchange(ip, https, "GET", "/info", None));
        if found.is_some() {
            return found;
        }
    }
    None
}

/// Probe every host of the local networks from a bounded pool of threads,
/// handing each responder to `found`. Ends early when `stop` is set.
pub fn sweep(me: &DeviceInfo, stop: &AtomicBool, found: &(dyn Fn(Heard) + Sync)) {
    let targets = sweep_targets();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..targets.len().min(48) {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&ip) = targets.get(index) else {
                        break;
                    };
                    if let Some(info) = probe(me, ip) {
                        found(Heard {
                            info,
                            ip: IpAddr::V4(ip),
                        });
                    }
                }
            });
        }
    });
}
