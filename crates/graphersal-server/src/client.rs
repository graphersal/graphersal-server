//! A minimal HTTP/1.1 client for the console's admin requests to a running server on this
//! machine (std `TcpStream`, no dependency): one request per connection (`Connection: close`),
//! `Host` = the server's address (its `Host` check accepts it), no `Origin`, a JSON body with
//! `Content-Length`, timeouts on connect, read and write. Answers `Content-Length`, chunked and
//! close-delimited bodies.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

use crate::error::ServerError;

/// How long the client waits for the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the client waits for an answer (an unload waits up to 30 s for running requests;
/// a create or clone of a large database copies it).
const READ_TIMEOUT: Duration = Duration::from_secs(600);
/// The largest answer read (the listing of many databases fits easily).
const MAX_ANSWER_BYTES: u64 = 64 << 20;

/// Sends `method path` (with `body` as JSON) to the server at `address` and answers the
/// status and the JSON body (`Value::Null` for an empty body).
pub(crate) fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), ServerError> {
    let failed = |message: String| ServerError::Connect {
        address: address.to_string(),
        message,
    };
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .map_err(|err| failed(err.to_string()))?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(CONNECT_TIMEOUT)))
        .map_err(|err| failed(err.to_string()))?;
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nAccept: \
         application/json\r\nUser-Agent: graphersal-server-console\r\n"
    );
    if body.is_some() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(payload.as_bytes()))
        .and_then(|()| stream.flush())
        .map_err(|err| failed(format!("cannot send the request: {err}")))?;
    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER_BYTES)
        .read_to_end(&mut answer)
        .map_err(|err| failed(format!("cannot read the answer: {err}")))?;
    let (status, body) = parse_answer(&answer).map_err(failed)?;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok((status, Value::Null));
    }
    let json = serde_json::from_slice(&body)
        .map_err(|err| failed(format!("the answer is not JSON ({err})")))?;
    Ok((status, json))
}

/// The status and the body of a raw HTTP/1.1 answer.
fn parse_answer(answer: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the answer has no header end")?;
    let head = String::from_utf8_lossy(&answer[..split]);
    let rest = &answer[split + 4..];
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("the answer has no status")?;
    let header = |name: &str| {
        head.lines().skip(1).find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    };
    let chunked =
        header("Transfer-Encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        return dechunk(rest).map(|body| (status, body));
    }
    match header("Content-Length").and_then(|v| v.parse::<usize>().ok()) {
        Some(len) if len <= rest.len() => Ok((status, rest[..len].to_vec())),
        Some(_) => Err("the answer is shorter than its Content-Length".to_string()),
        None => Ok((status, rest.to_vec())),
    }
}

/// A chunked body decoded.
fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("a broken chunk")?;
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| "a broken chunk size")?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        if rest.len() < size + 2 {
            return Err("a truncated chunk".to_string());
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_parsed() {
        let plain = b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}extra";
        assert_eq!(parse_answer(plain).unwrap(), (201, b"{}".to_vec()));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4\r\n\":1}\r\n0\r\n\r\n";
        assert_eq!(parse_answer(chunked).unwrap(), (200, b"{\"a\":1}".to_vec()));
        let closed = b"HTTP/1.1 404 Not Found\r\n\r\n{\"error\":\"x\"}";
        assert_eq!(parse_answer(closed).unwrap().0, 404);
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
    }
}
