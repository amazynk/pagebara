//! A tiny HTTP/1.1 server and client over `std::net`, for responders started by tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Answers `(method, path, body)` with `(content type, body)`, or `None` for a 404.
pub type Handler = dyn Fn(&str, &str, &[u8]) -> Option<(String, Vec<u8>)> + Send + Sync;

/// Serves `handler` on a free local port until the process ends; returns the base URL.
pub fn serve(handler: Arc<Handler>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    pagebara_thread::spawn("http-server", move || {
        for stream in listener.incoming().flatten() {
            let h = handler.clone();
            pagebara_thread::spawn("http-answer", move || {
                let _ = answer(stream, &*h);
            });
        }
    });
    url
}

fn answer(stream: TcpStream, handler: &Handler) -> std::io::Result<()> {
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut len = 0;
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    let mut w = stream;
    match handler(&method, &path, &body) {
        Some((ct, data)) => {
            write!(
                w,
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            )?;
            w.write_all(&data)?;
        }
        None => write!(
            w,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?,
    }
    w.flush()
}

/// `http://host:port/path` → (`host:port`, `/path`).
fn split(url: &str) -> Result<(&str, &str), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("not an http URL: {url}"))?;
    Ok(match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    })
}

fn request(url: &str, method: &str, ct: Option<&str>, body: &[u8]) -> Result<Vec<u8>, String> {
    let (host, path) = split(url)?;
    let mut s = TcpStream::connect(host).map_err(|e| e.to_string())?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(ct) = ct {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(body).map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).map_err(|e| e.to_string())?;
    let end = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no HTTP header")?;
    let head = String::from_utf8_lossy(&resp[..end]).to_string();
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(head.lines().next().unwrap_or_default().to_string());
    }
    Ok(resp[end + 4..].to_vec())
}

pub fn post(url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    request(url, "POST", Some(content_type), body)
}

pub fn get(url: &str) -> Result<Vec<u8>, String> {
    request(url, "GET", None, &[])
}
