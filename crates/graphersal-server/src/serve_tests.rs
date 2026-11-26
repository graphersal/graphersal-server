//! Tests of the listener's connection handling on a real socket (port 0): the connection cap
//! (`503` with `Retry-After`), the header and idle timeouts, and h2c with prior knowledge.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use serde_json::Value;

use super::*;
use crate::registry::tests::server;
use crate::test_support::TempDir;

/// A server on `127.0.0.1:0` with `options` (the listen address set here).
fn serve(temp: &TempDir, options: ServeOptions) -> Running {
    let dir = server(temp);
    start(
        dir.root(),
        options.with_listen("127.0.0.1:0".parse().unwrap()),
    )
    .unwrap()
}

/// `GET path` on `stream` (HTTP/1.1, keep-alive) with the right `Host`.
fn send_get(stream: &mut TcpStream, running: &Running, path: &str) {
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
        running.address()
    )
    .unwrap();
}

/// Everything `stream` sends until it closes (a read timeout fails the test).
fn read_to_close(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut out = Vec::new();
    stream.read_to_end(&mut out).unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

/// How long until the server closes `stream` (EOF or a reset), with nothing sent before.
fn time_to_close(stream: &mut TcpStream) -> Duration {
    let begun = Instant::now();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut buffer = [0u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return begun.elapsed(),
            Ok(_) => continue,
            Err(err)
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                panic!("the server did not close the connection in 20 s")
            }
            Err(_) => return begun.elapsed(),
        }
    }
}

#[test]
fn a_connection_over_the_cap_is_answered_503_with_retry_after() {
    let temp = TempDir::new("serve-cap");
    let running = serve(&temp, ServeOptions::new().with_max_connections(1));
    assert_eq!(running.connection_limits().max_connections, 1);
    // The one place: a connection that has not sent anything yet.
    let holder = TcpStream::connect(running.address()).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let mut refused = TcpStream::connect(running.address()).unwrap();
    send_get(&mut refused, &running, "/api/info");
    let answer = read_to_close(&mut refused);
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(answer.contains("\r\nRetry-After: 1\r\n"), "{answer}");
    assert!(answer.contains("\r\nConnection: close\r\n"), "{answer}");
    let body: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("connection limit (1 open"),
        "{body}"
    );
    assert!(body["help"].as_str().unwrap().contains("max_connections"));

    // The place frees when the holder goes: the next connection is served.
    drop(holder);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut stream = TcpStream::connect(running.address()).unwrap();
        write!(
            stream,
            "GET /api/info HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            running.address()
        )
        .unwrap();
        let answer = read_to_close(&mut stream);
        if answer.starts_with("HTTP/1.1 200") {
            break;
        }
        assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
        assert!(Instant::now() < deadline, "the place was not freed");
        std::thread::sleep(Duration::from_millis(50));
    }
    running.stop().unwrap();
}

#[test]
fn a_slow_or_silent_client_is_disconnected_after_the_header_timeout() {
    if crate::test_support::skip_on_ci("timing") {
        return;
    }
    let temp = TempDir::new("serve-header-timeout");
    let running = serve(
        &temp,
        ServeOptions::new()
            .with_header_timeout_secs(1)
            .with_idle_timeout_secs(30),
    );
    // Half a request head, then nothing.
    let mut slow = TcpStream::connect(running.address()).unwrap();
    slow.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .unwrap();
    let waited = time_to_close(&mut slow);
    assert!(
        waited >= Duration::from_millis(800) && waited < Duration::from_secs(5),
        "{waited:?}"
    );
    // Nothing at all.
    let mut silent = TcpStream::connect(running.address()).unwrap();
    let waited = time_to_close(&mut silent);
    assert!(
        waited >= Duration::from_millis(800) && waited < Duration::from_secs(5),
        "{waited:?}"
    );
    running.stop().unwrap();
}

#[test]
fn an_idle_kept_alive_connection_is_closed_after_the_idle_timeout() {
    if crate::test_support::skip_on_ci("timing") {
        return;
    }
    let temp = TempDir::new("serve-idle-timeout");
    let running = serve(
        &temp,
        ServeOptions::new()
            .with_header_timeout_secs(30)
            .with_idle_timeout_secs(1),
    );
    let mut stream = TcpStream::connect(running.address()).unwrap();
    send_get(&mut stream, &running, "/healthz");
    // The answer (kept alive: no EOF after it), then the idle close.
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut buffer = [0u8; 4096];
    let read = stream.read(&mut buffer).unwrap();
    let answer = String::from_utf8_lossy(&buffer[..read]).into_owned();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    let waited = time_to_close(&mut stream);
    assert!(
        waited >= Duration::from_millis(500) && waited < Duration::from_secs(5),
        "{waited:?}"
    );
    running.stop().unwrap();
}

#[test]
fn a_running_request_is_not_cut_by_the_idle_or_header_timeout() {
    let temp = TempDir::new("serve-slow-body");
    let running = serve(
        &temp,
        ServeOptions::new()
            .with_header_timeout_secs(1)
            .with_idle_timeout_secs(1),
    );
    // A JSON body sent slowly over more than both timeouts.
    let body = br#"{"name": "slow"}"#;
    let mut stream = TcpStream::connect(running.address()).unwrap();
    write!(
        stream,
        "POST /api/admin/databases HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        running.address(),
        crate::test_support::admin_header(running.address()),
        body.len()
    )
    .unwrap();
    for chunk in body.chunks(4) {
        std::thread::sleep(Duration::from_millis(600));
        stream.write_all(chunk).unwrap();
    }
    let answer = read_to_close(&mut stream);
    assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
    running.stop().unwrap();
}

/// The status and JSON body of `GET`/`POST path` over one h2c (prior knowledge) connection.
fn h2_request(running: &Running, method: &str, path: &str, body: &[u8]) -> (u16, Version, Value) {
    let address = running.address();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let body = body.to_vec();
    runtime.block_on(async move {
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut sender, conn) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await
                .unwrap();
        tokio::spawn(conn);
        let mut request = hyper::Request::builder()
            .method(method)
            .uri(format!("http://{address}{path}"))
            .header("content-type", "application/json");
        if let Some(token) = crate::test_support::operator_token(address) {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request.body(Outgoing::full(body)).unwrap();
        let response = sender.send_request(request).await.unwrap();
        let status = response.status().as_u16();
        let version = response.version();
        let mut incoming: Incoming = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) =
            std::future::poll_fn(|cx| Pin::new(&mut incoming).poll_frame(cx)).await
        {
            if let Ok(data) = frame.unwrap().into_data() {
                bytes.extend_from_slice(&data);
            }
        }
        (status, version, serde_json::from_slice(&bytes).unwrap())
    })
}

#[test]
fn h2c_with_prior_knowledge_is_served() {
    let temp = TempDir::new("serve-h2c");
    let running = serve(&temp, ServeOptions::new());
    // `:authority` passes the Host check.
    let (status, version, info) = h2_request(&running, "GET", "/api/info", b"");
    assert_eq!((status, version), (200, Version::HTTP_2), "{info}");
    assert!(info["protocol"].is_string(), "{info}");
    // A body is read (a JSON admin request).
    let (status, _, entry) = h2_request(
        &running,
        "POST",
        "/api/admin/databases",
        br#"{"name": "over-h2"}"#,
    );
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["name"], "over-h2");
    assert!(running.registry().get("over-h2").is_ok());
    // HTTP/1.1 on the same listener is unchanged.
    let mut stream = TcpStream::connect(running.address()).unwrap();
    write!(
        stream,
        "GET /healthz HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        running.address()
    )
    .unwrap();
    assert!(read_to_close(&mut stream).starts_with("HTTP/1.1 200"));
    running.stop().unwrap();
}

#[test]
fn zero_limits_are_refused() {
    let temp = TempDir::new("serve-zero-limits");
    let dir = server(&temp);
    for options in [
        ServeOptions::new().with_max_connections(0),
        ServeOptions::new().with_header_timeout_secs(0),
        ServeOptions::new().with_idle_timeout_secs(0),
    ] {
        let err = start(
            dir.root(),
            options.with_listen("127.0.0.1:0".parse().unwrap()),
        )
        .err()
        .unwrap();
        assert!(err.to_string().contains("at least 1"), "{err}");
    }
}

#[test]
fn an_h2c_connection_over_the_cap_is_answered_503_too() {
    let temp = TempDir::new("serve-cap-h2");
    let running = serve(&temp, ServeOptions::new().with_max_connections(1));
    let holder = TcpStream::connect(running.address()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (status, version, body) = h2_request(&running, "GET", "/api/info", b"");
    assert_eq!((status, version), (503, Version::HTTP_2), "{body}");
    assert!(body["help"].as_str().unwrap().contains("max_connections"));
    drop(holder);
    running.stop().unwrap();
}

// SEC M1: slow clients without credentials cannot hold the connection cap, and the probes
// answer through it.

/// Sends `head` on a new connection, then trickles one byte of the declared body every 200 ms
/// (each read of the server gets a byte well within its 120 s idle timeout) until the server
/// answers and closes: (the answer, how long it took). A 20 s read timeout stops a hung test.
fn trickle(running: &Running, head: &str) -> (String, Duration) {
    let mut stream = TcpStream::connect(running.address()).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    let begun = Instant::now();
    let writer = stream.try_clone().unwrap();
    let trickling = std::thread::spawn(move || {
        let mut writer = writer;
        for _ in 0..200 {
            if writer.write_all(b" ").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let answer = read_to_close(&mut stream);
    let took = begun.elapsed();
    drop(stream);
    let _ = trickling.join();
    (answer, took)
}

#[test]
fn a_trickled_login_body_is_cut_at_the_deadline() {
    let temp = TempDir::new("serve-sec-login-trickle");
    let deadline = Duration::from_secs(1);
    let running = serve(&temp, ServeOptions::new().with_body_deadline(deadline));
    let (answer, took) = trickle(
        &running,
        &format!(
            "POST /api/login HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: 1000\r\n\r\n{{",
            running.address()
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("too slowly"), "{answer}");
    assert!(took >= deadline - Duration::from_millis(100), "{took:?}");
    running.stop().unwrap();
}

#[test]
fn a_trickled_body_without_credentials_is_cut_and_its_drain_too() {
    let temp = TempDir::new("serve-sec-anonymous-trickle");
    let running = serve(
        &temp,
        ServeOptions::new().with_body_deadline(Duration::from_secs(1)),
    );
    // Anonymous: refused (401) before its body is read; the drain of the short rest is
    // bounded too, so the trickle does not hold the connection.
    let (answer, _) = trickle(
        &running,
        &format!(
            "POST /api/admin/databases HTTP/1.1\r\nHost: {}\r\n\
             Content-Type: application/json\r\nContent-Length: 1000\r\n\r\n{{",
            running.address()
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    // Wrong credentials: the same (refused before the body, the drain bounded).
    let (answer, _) = trickle(
        &running,
        &format!(
            "POST /api/admin/databases HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer nope\r\n\
             Content-Type: application/json\r\nContent-Length: 1000\r\n\r\n{{",
            running.address()
        ),
    );
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    // An authenticated body slower than the deadline is still read whole.
    let body = br#"{"name": "slow"}"#;
    let mut stream = TcpStream::connect(running.address()).unwrap();
    write!(
        stream,
        "POST /api/admin/databases HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        running.address(),
        crate::test_support::admin_header(running.address()),
        body.len()
    )
    .unwrap();
    for chunk in body.chunks(4) {
        std::thread::sleep(Duration::from_millis(400));
        stream.write_all(chunk).unwrap();
    }
    let answer = read_to_close(&mut stream);
    assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
    running.stop().unwrap();
}

#[test]
fn a_login_body_over_4_kib_is_refused_unread() {
    let temp = TempDir::new("serve-sec-login-cap");
    let running = serve(&temp, ServeOptions::new());
    let token = "x".repeat(5000);
    let body = format!(r#"{{"token": "{token}"}}"#);
    // Declared: 413 at once.
    let mut stream = TcpStream::connect(running.address()).unwrap();
    write!(
        stream,
        "POST /api/login HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        running.address(),
        body.len()
    )
    .unwrap();
    let answer = read_to_close(&mut stream);
    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
    // Chunked (undeclared): 413 after 4 KiB.
    let mut stream = TcpStream::connect(running.address()).unwrap();
    write!(
        stream,
        "POST /api/login HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
        running.address(),
        body.len()
    )
    .unwrap();
    let answer = read_to_close(&mut stream);
    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
    running.stop().unwrap();
}

#[test]
fn the_probes_answer_also_over_the_connection_cap() {
    let temp = TempDir::new("serve-sec-probes");
    let running = serve(&temp, ServeOptions::new().with_max_connections(1));
    let holder = TcpStream::connect(running.address()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for (path, expected) in [("/healthz", "\"ok\""), ("/readyz", "\"ready\"")] {
        let mut stream = TcpStream::connect(running.address()).unwrap();
        send_get(&mut stream, &running, path);
        let answer = read_to_close(&mut stream);
        assert!(answer.starts_with("HTTP/1.1 200"), "{path}: {answer}");
        assert!(answer.contains(expected), "{path}: {answer}");
        assert!(answer.contains("no-store"), "{path}: {answer}");
    }
    let (status, version, body) = h2_request(&running, "GET", "/healthz", b"");
    assert_eq!((status, version), (200, Version::HTTP_2), "{body}");
    // Every other request still gets the 503.
    let mut stream = TcpStream::connect(running.address()).unwrap();
    send_get(&mut stream, &running, "/api/info");
    assert!(read_to_close(&mut stream).starts_with("HTTP/1.1 503"));
    drop(holder);
    assert_eq!(blocking_threads(256), 288);
    assert_eq!(blocking_threads(1), 33);
    assert_eq!(blocking_threads(100_000), 512);
    running.stop().unwrap();
}
