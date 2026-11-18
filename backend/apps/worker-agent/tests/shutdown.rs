//! The real binary must stop cleanly on SIGTERM, which is what container
//! runtimes and Kubernetes send. Waiting for Ctrl+C alone made a PID-1
//! process ignore it until SIGKILL.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn healthy(addr: &str) -> bool {
    let Ok(mut s) = TcpStream::connect(addr) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut body = String::new();
    let _ = s.read_to_string(&mut body);
    body.starts_with("HTTP/1.1 200")
}

#[test]
fn sigterm_stops_the_agent_gracefully() {
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_worker-agent"))
        .env_clear()
        .env("WORKER_BIND", &addr)
        .env("WORKER_PUBLIC_URL", format!("http://{addr}"))
        .env("INFERENCE_MODE", "mock")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    while !healthy(&addr) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("agent exited before becoming healthy: {status}");
        }
        assert!(Instant::now() < deadline, "agent never became healthy");
        std::thread::sleep(Duration::from_millis(100));
    }

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "expected a clean exit, got {status}");
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("agent ignored SIGTERM for 10 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
