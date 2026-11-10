//! HTTP GET through WinHTTP: the system's proxy settings and TLS, with
//! nothing added to the build.
//!
//! The `windows` crate marks WinHTTP calls `unsafe`. The invariant they
//! rely on: each handle passed in is open, made by the call before, and
//! stays open until its `Handle` drops after its last use.

use std::ffi::c_void;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

/// Where a GET goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    pub port: u16,
    /// The path and query: "/scripts/XML_daily.asp".
    pub path: String,
    /// HTTPS, not plain HTTP.
    pub secure: bool,
}

impl Address {
    /// `path` on `host`, over HTTPS at its port, 443.
    pub fn https(host: &str, path: &str) -> Address {
        Address {
            host: host.to_string(),
            port: 443,
            path: path.to_string(),
            secure: true,
        }
    }

    /// The address `url` names: `https://` or `http://`, a host of
    /// letters, digits, dots and dashes, perhaps a port, then the path
    /// and query, `/` when there is none. None for any other URL: one
    /// with a user, an IPv6 host or a scheme WinHTTP is not asked for.
    pub fn parse(url: &str) -> Option<Address> {
        let (secure, rest) = match url.split_once("://")? {
            ("https", rest) => (true, rest),
            ("http", rest) => (false, rest),
            _ => return None,
        };
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let (host, port) = host_and_port(authority, secure)?;
        Some(Address {
            host,
            port,
            path: if path.is_empty() { "/" } else { path }.to_string(),
            secure,
        })
    }
}

/// The host and port `authority` names, the port the scheme's own when
/// it names none; none when the host is not plain letters, digits, dots
/// and dashes.
fn host_and_port(authority: &str, secure: bool) -> Option<(String, u16)> {
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, digits(port)?.parse().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-';
    let named = !host.is_empty() && host.bytes().all(plain);
    named.then(|| (host.to_string(), port))
}

/// `text`, when it is digits only: Rust would read `+443` as a number.
fn digits(text: &str) -> Option<&str> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    digits.then_some(text)
}

/// What every request names as its user agent: hyalo and its version.
const AGENT: &str = concat!("hyalo/", env!("CARGO_PKG_VERSION"));

/// How long resolving the name, connecting, sending and receiving may
/// each take, in milliseconds. Requests run off the UI thread; this only
/// bounds how long a dead network holds one up.
const TIMEOUT: i32 = 15_000;

/// The body `address` answers a GET with, following redirects. The
/// request names its user agent `hyalo/<version>` and sends nothing else
/// of the user's: no cookies, no credentials. An error unless the answer
/// is 200 OK.
pub fn get(address: &Address) -> Result<Vec<u8>> {
    let exchange = Exchange::sent(address)?;
    match status(&exchange.request)? {
        200 => body(&exchange.request),
        other => Err(Error::new(E_FAIL, format!("the server answered {other}"))),
    }
}

/// A session, a connection in it and a request on it; they close in
/// that order reversed, the request first.
struct Exchange {
    request: Handle,
    _connection: Handle,
    _session: Handle,
}

impl Exchange {
    /// A GET to `address`, sent, its answer read as far as the headers.
    fn sent(address: &Address) -> Result<Self> {
        let session = session()?;
        let host = HSTRING::from(&address.host);
        let connection =
            Handle::opened(unsafe { WinHttpConnect(session.0, &host, address.port, 0) })?;
        let request = Handle::opened(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(&address.path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags(address.secure),
            )
        })?;
        // A GET sends no body, and a request without callbacks needs no
        // context for them.
        let (no_body, no_context) = (u32::default(), usize::default());
        unsafe {
            WinHttpSendRequest(request.0, None, None, no_body, no_body, no_context)?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;
        }
        Ok(Self {
            request,
            _connection: connection,
            _session: session,
        })
    }
}

/// A session that finds its proxy as the system does, with timeouts.
fn session() -> Result<Handle> {
    let session = Handle::opened(unsafe {
        WinHttpOpen(
            &HSTRING::from(AGENT),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    })?;
    unsafe { WinHttpSetTimeouts(session.0, TIMEOUT, TIMEOUT, TIMEOUT, TIMEOUT)? };
    Ok(session)
}

/// The request flags for a plain or a secure address.
fn flags(secure: bool) -> WINHTTP_OPEN_REQUEST_FLAGS {
    match secure {
        true => WINHTTP_FLAG_SECURE,
        false => WINHTTP_OPEN_REQUEST_FLAGS::default(),
    }
}

/// The answer's status code: 200, 404.
fn status(request: &Handle) -> Result<u32> {
    let mut status = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `status` is a live u32 and `size` is its size.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )?;
    }
    Ok(status)
}

/// The whole body, read until WinHTTP has no more.
fn body(request: &Handle) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let mut read = 0_u32;
        // SAFETY: `chunk` is a live buffer of the length given.
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )?;
        }
        if read == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&chunk[..read as usize]);
    }
}

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut c_void);

impl Handle {
    /// `raw` as a handle; the thread's last error when it is null, as
    /// the call that made it failed.
    fn opened(raw: *mut c_void) -> Result<Self> {
        match raw.is_null() {
            true => Err(Error::from_thread()),
            false => Ok(Self(raw)),
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// A server on 127.0.0.1 that answers one request with `answer` and
    /// gives back the request it read. It answers at once what is not
    /// plain text, such as a TLS hello, which has no end of head to wait
    /// for.
    pub(crate) fn serve(answer: Vec<u8>) -> (u16, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a request");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while request.is_ascii() && !request.windows(4).any(|end| end == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).expect("read the request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let _ = stream.write_all(&answer);
            String::from_utf8_lossy(&request).into_owned()
        });
        (port, server)
    }

    pub(crate) fn answer(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=windows-1251\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    fn local(port: u16, secure: bool) -> Address {
        address("127.0.0.1", port, "/scripts/XML_daily.asp", secure)
    }

    fn address(host: &str, port: u16, path: &str, secure: bool) -> Address {
        Address {
            host: host.to_string(),
            port,
            path: path.to_string(),
            secure,
        }
    }

    #[test]
    fn an_address_is_read_from_a_url() {
        let exe = "https://github.com/IlyaShorin/hyalo/releases/download/v0.0.2/hyalo.exe";
        let path = "/IlyaShorin/hyalo/releases/download/v0.0.2/hyalo.exe";
        assert_eq!(
            Address::parse(exe),
            Some(address("github.com", 443, path, true))
        );
        assert_eq!(
            Address::parse("http://127.0.0.1:8080/x?y=1"),
            Some(address("127.0.0.1", 8080, "/x?y=1", false))
        );
        assert_eq!(
            Address::parse("http://localhost"),
            Some(address("localhost", 80, "/", false))
        );
        assert_eq!(
            Address::parse("https://api.github.com:8443"),
            Some(address("api.github.com", 8443, "/", true))
        );
    }

    #[test]
    fn a_url_that_names_no_plain_host_is_no_address() {
        for wrong in [
            "",
            "github.com/x",
            "ftp://github.com/x",
            "HTTPS://github.com/x",
            "https://",
            "https:///x",
            "https://:443/x",
            "https://github.com:/x",
            "https://github.com:+443/x",
            "https://github.com:65536/x",
            "https://github.com:https/x",
            "https://user@github.com/x",
            "https://[::1]/x",
            "https://github.com?x",
            "https://git hub.com/x",
        ] {
            assert_eq!(Address::parse(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn a_get_brings_back_the_body_of_an_ok_answer() {
        let body: Vec<u8> = (0..200_000_u32).map(|n| (n % 251) as u8).collect();
        let (port, server) = serve(answer("200 OK", &body));
        assert_eq!(get(&local(port, false)).expect("the body"), body);
        let request = server.join().expect("the server");
        let lines: Vec<&str> = request.split("\r\n").collect();
        assert_eq!(lines[0], "GET /scripts/XML_daily.asp HTTP/1.1");
        let agent = format!("User-Agent: hyalo/{}", env!("CARGO_PKG_VERSION"));
        assert!(lines.contains(&agent.as_str()), "{request}");
        assert!(!request.contains("Cookie"), "{request}");
        assert!(!request.contains("Authorization"), "{request}");
    }

    #[test]
    fn a_get_follows_a_redirect_as_github_sends_downloads_elsewhere() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let elsewhere = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/elsewhere\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let answers = [elsewhere.into_bytes(), answer("200 OK", b"the exe")];
        let server = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().expect("a request");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).expect("read the request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                let _ = stream.write_all(&answer);
                let request = String::from_utf8_lossy(&request).into_owned();
                asked.push(request.lines().next().unwrap_or_default().to_string());
            }
            asked
        });
        let first = address("127.0.0.1", port, "/download/hyalo.exe", false);
        assert_eq!(get(&first).expect("the body"), b"the exe");
        let asked = server.join().expect("the server");
        assert_eq!(
            asked,
            [
                "GET /download/hyalo.exe HTTP/1.1",
                "GET /elsewhere HTTP/1.1"
            ]
        );
    }

    #[test]
    fn an_answer_other_than_ok_is_an_error() {
        let (port, server) = serve(answer("503 Service Unavailable", b"busy"));
        let error = get(&local(port, false)).expect_err("not OK");
        assert!(error.message().contains("503"), "{error}");
        server.join().expect("the server");
    }

    #[test]
    fn a_server_that_hangs_up_without_answering_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = listener.local_addr().expect("an address").port();
        let server = std::thread::spawn(move || drop(listener.accept().expect("a request")));
        assert!(get(&local(port, false)).is_err());
        server.join().expect("the server");
    }

    #[test]
    fn an_address_without_a_host_is_an_error() {
        let nowhere = Address {
            host: String::new(),
            ..local(80, false)
        };
        assert!(get(&nowhere).is_err());
    }

    #[test]
    fn a_secure_address_speaks_tls_so_a_plain_server_cannot_answer() {
        let (port, _server) = serve(answer("200 OK", b"plain"));
        assert!(get(&local(port, true)).is_err());
    }
}
