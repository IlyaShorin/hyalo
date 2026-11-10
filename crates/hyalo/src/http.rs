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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub host: &'static str,
    pub port: u16,
    /// The path and query: "/scripts/XML_daily.asp".
    pub path: &'static str,
    /// HTTPS, not plain HTTP.
    pub secure: bool,
}

/// How long resolving the name, connecting, sending and receiving may
/// each take, in milliseconds. Requests run off the UI thread; this only
/// bounds how long a dead network holds one up.
const TIMEOUT: i32 = 15_000;

/// The body `address` answers a GET with. The request names its user
/// agent `hyalo` and sends nothing else of the user's: no cookies, no
/// credentials. An error unless the answer is 200 OK.
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
        let host = HSTRING::from(address.host);
        let connection =
            Handle::opened(unsafe { WinHttpConnect(session.0, &host, address.port, 0) })?;
        let request = Handle::opened(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(address.path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags(address.secure),
            )
        })?;
        unsafe {
            WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
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
            w!("hyalo"),
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
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// A server on 127.0.0.1 that answers one request with `answer` and
    /// gives back the request it read. It answers at once what is not
    /// plain text, such as a TLS hello, which has no end of head to wait
    /// for.
    fn serve(answer: Vec<u8>) -> (u16, JoinHandle<String>) {
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

    fn answer(status: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=windows-1251\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    fn local(port: u16, secure: bool) -> Address {
        Address {
            host: "127.0.0.1",
            port,
            path: "/scripts/XML_daily.asp",
            secure,
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
        assert!(lines.contains(&"User-Agent: hyalo"), "{request}");
        assert!(!request.contains("Cookie"), "{request}");
        assert!(!request.contains("Authorization"), "{request}");
    }

    #[test]
    fn an_answer_other_than_ok_is_an_error() {
        let (port, server) = serve(answer("503 Service Unavailable", b"busy"));
        let error = get(&local(port, false)).expect_err("not OK");
        assert!(error.message().contains("503"), "{error}");
        server.join().expect("the server");
    }

    #[test]
    fn nobody_listening_is_an_error() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
            listener.local_addr().expect("an address").port()
        };
        assert!(get(&local(port, false)).is_err());
    }

    #[test]
    fn a_secure_address_speaks_tls_so_a_plain_server_cannot_answer() {
        let (port, _server) = serve(answer("200 OK", b"plain"));
        assert!(get(&local(port, true)).is_err());
    }
}
