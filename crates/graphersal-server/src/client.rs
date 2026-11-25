//! A minimal HTTP/1.1 client for the console's admin requests to a running server on this
//! machine (std `TcpStream`, no dependency): one request per connection (`Connection: close`),
//! `Host` = the server's address (its `Host` check accepts it), no `Origin`, the operator
//! token of the run (`server.run`) as `Authorization: Bearer` when given, a JSON body with
//! `Content-Length`, timeouts on connect, read and write. Answers `Content-Length`, chunked and
//! close-delimited bodies. [`download`] streams a `200` answer's body into a writer (an export).

use std::io::{BufRead, BufReader, Read, Write};
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

/// The `Authorization` header line for the bearer `token` (empty without one).
fn authorization(token: Option<&str>) -> String {
    token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default()
}

/// Sends `method path` (with `body` as JSON, `token` as the bearer token) to the server at
/// `address` and answers the status and the JSON body (`Value::Null` for an empty body).
pub(crate) fn request(
    address: SocketAddr,
    token: Option<&str>,
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
         application/json\r\nUser-Agent: graphersal-server-console\r\n{}",
        authorization(token)
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

/// What [`download`] received.
pub(crate) struct Downloaded {
    /// The status.
    pub(crate) status: u16,
    /// The headers, in order.
    pub(crate) headers: Vec<(String, String)>,
    /// The body bytes written into the writer (`200`).
    pub(crate) bytes: u64,
    /// The JSON body of an error answer (`Value::Null` for `200` or a body that is not JSON).
    pub(crate) error: Value,
}

impl Downloaded {
    /// The value of the header `name` (case-insensitive).
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Sends `GET path` (with `token` as the bearer token) to the server at `address` and streams
/// a `200` answer's body into `out` (`Content-Length`, chunked or close-delimited); an error
/// answer's JSON body is read instead.
pub(crate) fn download(
    address: SocketAddr,
    token: Option<&str>,
    path: &str,
    out: &mut dyn Write,
) -> Result<Downloaded, ServerError> {
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
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nUser-Agent: \
         graphersal-server-console\r\n{}Content-Length: 0\r\n\r\n",
        authorization(token)
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|err| failed(format!("cannot send the request: {err}")))?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| failed(format!("cannot read the answer: {err}")))?;
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| failed("the answer has no status".to_string()))?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|err| failed(format!("cannot read the answer: {err}")))?;
        let line = line.trim_end();
        if n == 0 || line.is_empty() {
            break;
        }
        if let Some((field, value)) = line.split_once(':') {
            headers.push((field.trim().to_string(), value.trim().to_string()));
        }
    }
    let mut answer = Downloaded {
        status,
        headers,
        bytes: 0,
        error: Value::Null,
    };
    let chunked = answer
        .header("Transfer-Encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let length = answer
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok());
    if status != 200 {
        let mut body = Vec::new();
        (&mut reader)
            .take(MAX_ANSWER_BYTES)
            .read_to_end(&mut body)
            .map_err(|err| failed(format!("cannot read the answer: {err}")))?;
        let body = if chunked {
            dechunk(&body).unwrap_or_default()
        } else {
            body
        };
        answer.error = serde_json::from_slice(&body).unwrap_or(Value::Null);
        return Ok(answer);
    }
    let copied = if chunked {
        copy_chunked(&mut reader, out)
    } else if let Some(length) = length {
        std::io::copy(&mut (&mut reader).take(length), out).and_then(|n| {
            if n == length {
                Ok(n)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("the answer ended after {n} of {length} bytes"),
                ))
            }
        })
    } else {
        std::io::copy(&mut reader, out)
    };
    answer.bytes = copied.map_err(|err| failed(format!("cannot read the download: {err}")))?;
    Ok(answer)
}

/// Copies a chunked body from `reader` into `out`; answers the bytes copied.
fn copy_chunked(reader: &mut dyn BufRead, out: &mut dyn Write) -> std::io::Result<u64> {
    let broken =
        |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let mut total = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(broken("the chunked answer ended early"));
        }
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_text, 16).map_err(|_| broken("a broken chunk size"))?;
        if size == 0 {
            return Ok(total);
        }
        let n = std::io::copy(&mut (&mut *reader).take(size), out)?;
        if n != size {
            return Err(broken("a truncated chunk"));
        }
        total += n;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end)?;
    }
}

/// The status and the body of a raw HTTP/1.1 answer.
pub(crate) fn parse_answer(answer: &[u8]) -> Result<(u16, Vec<u8>), String> {
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

    #[test]
    fn chunked_downloads_are_decoded() {
        let mut body: &[u8] = b"3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        let mut out = Vec::new();
        assert_eq!(copy_chunked(&mut body, &mut out).unwrap(), 5);
        assert_eq!(out, b"abcde");
        let mut cut: &[u8] = b"3\r\nab";
        assert!(copy_chunked(&mut cut, &mut Vec::new()).is_err());
    }
}
