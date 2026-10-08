//! Transport for the LocalSend protocol: a self-signed TLS identity, rustls
//! streams, and the small slice of HTTP/1.1 the protocol needs. Every
//! connection carries one request and is closed after the response.

use super::proto;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn other<E: std::fmt::Display>(error: E) -> io::Error {
    io::Error::other(error.to_string())
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

// ---- identity --------------------------------------------------------------

/// This device's certificate. The fingerprint the protocol asks for is the
/// SHA-256 of the certificate (DER), as lowercase hex.
#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

impl Identity {
    pub fn generate() -> io::Result<Identity> {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localsend".to_string()]).map_err(other)?;
        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.key_pair.serialize_der();
        Ok(Identity {
            fingerprint: sha256_hex(&cert_der),
            cert_der,
            key_der,
        })
    }

    /// The identity saved in `directory`, or a new one saved there, so the
    /// fingerprint other devices remember survives restarts.
    pub fn load_or_create(directory: &Path) -> io::Result<Identity> {
        let cert_path = directory.join("cert.der");
        let key_path = directory.join("key.der");
        if let (Ok(cert_der), Ok(key_der)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            let identity = Identity {
                fingerprint: sha256_hex(&cert_der),
                cert_der,
                key_der,
            };
            if server_config(&identity).is_ok() {
                return Ok(identity);
            }
        }
        let identity = Identity::generate()?;
        std::fs::create_dir_all(directory)?;
        std::fs::write(&cert_path, &identity.cert_der)?;
        write_private(&key_path, &identity.key_der)?;
        Ok(identity)
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

pub fn server_config(identity: &Identity) -> io::Result<Arc<ServerConfig>> {
    let certificate = CertificateDer::from(identity.cert_der.clone());
    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(identity.key_der.clone()));
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(other)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .map_err(other)?;
    Ok(Arc::new(config))
}

/// LocalSend devices use self-signed certificates and do not validate a chain.
/// Like them, this accepts any certificate, but when the peer's announced
/// fingerprint is a SHA-256 it must match the certificate shown, so a different
/// device answering on a remembered address is refused.
#[derive(Debug)]
struct PinVerifier {
    expected: Option<String>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.expected
            && sha256_hex(end_entity.as_ref()) != *expected
        {
            return Err(rustls::Error::General(
                "the device's certificate does not match its fingerprint".into(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn client_config(fingerprint: &str) -> io::Result<Arc<ClientConfig>> {
    let provider = provider();
    let expected = {
        let f = fingerprint.to_ascii_lowercase();
        (f.len() == 64 && f.bytes().all(|b| b.is_ascii_hexdigit())).then_some(f)
    };
    let verifier = PinVerifier {
        expected,
        algorithms: provider.signature_verification_algorithms,
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

// ---- streams ---------------------------------------------------------------

pub enum Wire {
    Plain(TcpStream),
    Server(Box<StreamOwned<ServerConnection, TcpStream>>),
    Client(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Server(s) => s.read(buf),
            Wire::Client(s) => s.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Server(s) => s.write(buf),
            Wire::Client(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Server(s) => s.flush(),
            Wire::Client(s) => s.flush(),
        }
    }
}

impl Wire {
    pub fn tcp(&self) -> &TcpStream {
        match self {
            Wire::Plain(s) => s,
            Wire::Server(s) => &s.sock,
            Wire::Client(s) => &s.sock,
        }
    }

    /// Say goodbye (TLS close_notify) and close the socket.
    pub fn finish(&mut self) {
        match self {
            Wire::Server(s) => s.conn.send_close_notify(),
            Wire::Client(s) => s.conn.send_close_notify(),
            Wire::Plain(_) => {}
        }
        let _ = self.flush();
        let _ = self.tcp().shutdown(Shutdown::Both);
    }
}

/// Wrap an accepted connection in TLS (server side).
pub fn accept_tls(config: &Arc<ServerConfig>, socket: TcpStream) -> io::Result<Wire> {
    let connection = ServerConnection::new(config.clone()).map_err(other)?;
    Ok(Wire::Server(Box::new(StreamOwned::new(connection, socket))))
}

/// Connect to a peer. HTTPS peers are pinned to their announced fingerprint.
pub fn connect(
    ip: IpAddr,
    port: u16,
    https: bool,
    fingerprint: &str,
    read_timeout: Duration,
) -> io::Result<Wire> {
    connect_within(ip, port, https, fingerprint, Duration::from_secs(5), read_timeout)
}

/// `connect` with its own limit on how long the TCP connection may take.
pub fn connect_within(
    ip: IpAddr,
    port: u16,
    https: bool,
    fingerprint: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> io::Result<Wire> {
    let socket = TcpStream::connect_timeout(&SocketAddr::new(ip, port), connect_timeout)?;
    socket.set_read_timeout(Some(read_timeout))?;
    socket.set_write_timeout(Some(Duration::from_secs(30)))?;
    let _ = socket.set_nodelay(true);
    if !https {
        return Ok(Wire::Plain(socket));
    }
    let config = client_config(fingerprint)?;
    let name = ServerName::IpAddress(ip.into());
    let connection = ClientConnection::new(config, name).map_err(other)?;
    Ok(Wire::Client(Box::new(StreamOwned::new(connection, socket))))
}

// ---- HTTP: requests (server side) -----------------------------------------

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn read_line_limited<R: BufRead>(reader: &mut R, limit: u64) -> io::Result<String> {
    let mut line = String::new();
    let read = reader.by_ref().take(limit).read_line(&mut line)?;
    if read == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    if !line.ends_with('\n') {
        return Err(bad("line too long"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_headers<R: BufRead>(reader: &mut R) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    loop {
        let line = read_line_limited(reader, 16 * 1024)?;
        if line.is_empty() {
            return Ok(headers);
        }
        if headers.len() >= 100 {
            return Err(bad("too many headers"));
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
}

pub fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Request> {
    let line = read_line_limited(reader, 16 * 1024)?;
    let mut parts = line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| bad("empty request"))?
        .to_string();
    let target = parts.next().ok_or_else(|| bad("no request target"))?;
    let (path, query_text) = target.split_once('?').unwrap_or((target, ""));
    let query = query_text
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (proto::url_decode(k), proto::url_decode(v))
        })
        .collect();
    let headers = read_headers(reader)?;
    Ok(Request {
        method,
        path: path.to_string(),
        query,
        headers,
    })
}

/// Read a message body (Content-Length or chunked) and hand it to `sink` in
/// pieces. More than `max` bytes is an error.
pub fn read_body<R: BufRead>(
    reader: &mut R,
    headers: &[(String, String)],
    max: u64,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<u64> {
    let find = |name: &str| header_of(headers, name);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let chunked =
        find("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            let size_line = read_line_limited(reader, 1024)?;
            let size_text = size_line.split(';').next().unwrap_or("").trim();
            let mut remaining =
                u64::from_str_radix(size_text, 16).map_err(|_| bad("bad chunk size"))?;
            if remaining == 0 {
                while !read_line_limited(reader, 16 * 1024)?.is_empty() {}
                return Ok(total);
            }
            while remaining > 0 {
                let want = remaining.min(buffer.len() as u64) as usize;
                let n = reader.read(&mut buffer[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "body cut short",
                    ));
                }
                total += n as u64;
                if total > max {
                    return Err(bad("body too large"));
                }
                sink(&buffer[..n])?;
                remaining -= n as u64;
            }
            read_line_limited(reader, 16)?; // the CRLF after the chunk
        }
    }
    let length = match find("content-length") {
        Some(v) => v.parse::<u64>().map_err(|_| bad("bad content length"))?,
        None => return Ok(0),
    };
    if length > max {
        return Err(bad("body too large"));
    }
    let mut remaining = length;
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let n = reader.read(&mut buffer[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "body cut short",
            ));
        }
        total += n as u64;
        sink(&buffer[..n])?;
        remaining -= n as u64;
    }
    Ok(total)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    }
}

pub fn write_response<W: Write>(
    writer: &mut W,
    status: u16,
    json: Option<&[u8]>,
) -> io::Result<()> {
    let body = json.unwrap_or(&[]);
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nConnection: close\r\n",
        reason(status)
    );
    if json.is_some() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

// ---- HTTP: requests (client side) -----------------------------------------

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Send one request on `wire` and read the whole response. `write_body` writes
/// exactly `length` bytes.
pub fn call(
    wire: &mut Wire,
    method: &str,
    host: &str,
    target: &str,
    content_type: Option<&str>,
    length: u64,
    write_body: &mut dyn FnMut(&mut Wire) -> io::Result<()>,
) -> io::Result<Reply> {
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Pulse\r\nConnection: close\r\nContent-Length: {length}\r\n"
    );
    if let Some(kind) = content_type {
        head.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    head.push_str("\r\n");
    wire.write_all(head.as_bytes())?;
    write_body(wire)?;
    wire.flush()?;

    let mut reader = BufReader::new(&mut *wire);
    let status_line = read_line_limited(&mut reader, 16 * 1024)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("bad status line"))?;
    let headers = read_headers(&mut reader)?;
    let mut body = Vec::new();
    let has_length = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    const CAP: u64 = 8 * 1024 * 1024;
    if has_length || chunked {
        read_body(&mut reader, &headers, CAP, &mut |c| {
            body.extend_from_slice(c);
            Ok(())
        })?;
    } else {
        // No length: the body runs until the peer closes. A close without a TLS
        // goodbye is a normal end here.
        let mut limited = (&mut reader).take(CAP);
        match limited.read_to_end(&mut body) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Reply { status, body })
}

/// One small request with an optional JSON body, on a fresh connection.
#[allow(clippy::too_many_arguments)]
pub fn request_json(
    ip: IpAddr,
    port: u16,
    https: bool,
    fingerprint: &str,
    method: &str,
    target: &str,
    json: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Reply> {
    let mut wire = connect(ip, port, https, fingerprint, timeout)?;
    let payload = json.unwrap_or(&[]);
    let host = format!("{ip}:{port}");
    let result = call(
        &mut wire,
        method,
        &host,
        target,
        json.map(|_| "application/json"),
        payload.len() as u64,
        &mut |w| w.write_all(payload),
    );
    wire.finish();
    result
}
