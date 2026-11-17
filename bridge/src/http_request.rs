use std::io;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

const MAX_HEADER_BYTES: usize = 8192;
const MAX_BODY_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestError {
    BadRequest,
    LengthRequired,
    HeaderTooLarge,
    BodyTooLarge,
    UnsupportedEncoding,
}

impl RequestError {
    pub(crate) fn response(self) -> &'static str {
        match self {
            Self::BadRequest => "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Self::LengthRequired => "HTTP/1.1 411 Length Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Self::HeaderTooLarge => "HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Self::BodyTooLarge => "HTTP/1.1 413 Content Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Self::UnsupportedEncoding => "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        }
    }
}

fn request_length(header: &[u8]) -> Result<usize, RequestError> {
    let header = std::str::from_utf8(header).map_err(|_| RequestError::BadRequest)?;
    let mut lines = header.strip_suffix("\r\n\r\n").ok_or(RequestError::BadRequest)?.split("\r\n");
    let mut request = lines.next().ok_or(RequestError::BadRequest)?.split(' ');
    let method = request.next().ok_or(RequestError::BadRequest)?;
    let target = request.next().ok_or(RequestError::BadRequest)?;
    let version = request.next().ok_or(RequestError::BadRequest)?;
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || !target.starts_with('/') || target.bytes().any(|byte| byte.is_ascii_control())
        || !matches!(version, "HTTP/1.1" | "HTTP/1.0") || request.next().is_some()
    {
        return Err(RequestError::BadRequest);
    }
    let mut length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(RequestError::BadRequest)?;
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.bytes().any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(RequestError::BadRequest);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(RequestError::UnsupportedEncoding);
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim_matches([' ', '\t']);
            if length.is_some() || value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(RequestError::BadRequest);
            }
            let value: usize = value.parse().map_err(|_| RequestError::BodyTooLarge)?;
            if value > MAX_BODY_BYTES {
                return Err(RequestError::BodyTooLarge);
            }
            length = Some(value);
        }
    }
    if matches!(method, "POST" | "PUT" | "PATCH") && length.is_none() {
        return Err(RequestError::LengthRequired);
    }
    Ok(length.unwrap_or(0))
}

pub(crate) async fn read(stream: &mut TcpStream) -> io::Result<Result<Option<String>, RequestError>> {
    let mut bytes = Vec::with_capacity(4096);
    let mut length = None;
    loop {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(if bytes.is_empty() { Ok(None) } else { Err(RequestError::BadRequest) });
        }
        bytes.extend_from_slice(&chunk[..count]);
        if length.is_none() {
            if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let end = at + 4;
                if end > MAX_HEADER_BYTES {
                    return Ok(Err(RequestError::HeaderTooLarge));
                }
                length = match request_length(&bytes[..end]) {
                    Ok(body) => Some(end + body),
                    Err(error) => return Ok(Err(error)),
                };
            } else if bytes.len() > MAX_HEADER_BYTES {
                return Ok(Err(RequestError::HeaderTooLarge));
            }
        }
        if let Some(length) = length {
            if bytes.len() > length {
                return Ok(Err(RequestError::BadRequest));
            }
            if bytes.len() == length {
                return Ok(std::str::from_utf8(&bytes).map(|request| Some(request.to_string())).map_err(|_| RequestError::BadRequest));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_is_required_for_post_and_preserves_body_boundary() {
        assert_eq!(request_length(b"POST /api/faucet/claim HTTP/1.1\r\nContent-Length: 97\r\n\r\n"), Ok(97));
        assert_eq!(request_length(b"POST /api/faucet/claim HTTP/1.1\r\n\r\n"), Err(RequestError::LengthRequired));
        assert_eq!(request_length(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\n\r\n"), Ok(0));
    }

    #[test]
    fn ambiguous_or_oversized_framing_is_rejected() {
        for header in [
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: +1\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 1, 1\r\n\r\n",
            "POST / HTTP/1.1\r\n Content-Length: 1\r\n\r\n",
        ] {
            assert_eq!(request_length(header.as_bytes()), Err(RequestError::BadRequest), "{header:?}");
        }
        assert_eq!(request_length(b"POST / HTTP/1.1\r\nContent-Length: 65537\r\n\r\n"), Err(RequestError::BodyTooLarge));
        assert_eq!(request_length(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"), Err(RequestError::UnsupportedEncoding));
    }

    #[tokio::test]
    async fn fragmented_header_and_body_are_read_to_the_declared_length() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read(&mut stream).await.unwrap().unwrap().unwrap()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        for piece in [b"POST / HTTP/1.1\r\nContent-".as_slice(), b"Length: 2\r\n\r\n", b"{", b"}"] {
            client.write_all(piece).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(server.await.unwrap().ends_with("\r\n\r\n{}"));
    }
}
