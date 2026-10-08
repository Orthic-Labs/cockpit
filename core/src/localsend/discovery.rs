//! Multicast discovery (UDP 224.0.0.167:53317). The socket is shared so
//! another LocalSend app on the same Mac can run beside Pulse.

use super::proto::{self, DeviceInfo};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

/// IPv4 addresses of this Mac's network interfaces, loopback excluded.
pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut found = Vec::new();
    for data in networks.values() {
        for network in data.ip_networks() {
            if let IpAddr::V4(ip) = network.addr
                && !ip.is_loopback()
                && !ip.is_unspecified()
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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
fn use_interface(_socket: &UdpSocket, _ip: Ipv4Addr) {}

/// A socket joined to the group on every interface.
pub fn listen(port: u16) -> io::Result<UdpSocket> {
    let socket = bind_shared(port)?;
    let interfaces = local_ipv4s();
    let mut joined = false;
    for ip in &interfaces {
        joined |= socket
            .join_multicast_v4(&proto::MULTICAST_GROUP, ip)
            .is_ok();
    }
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
