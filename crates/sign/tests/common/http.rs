//! A tiny HTTP/1.1 server and client over `std::net`, for responders started by tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long the client waits to connect, and for each read and write: a responder that
/// stops answering fails its test instead of leaving it waiting.
const PATIENCE: Duration = Duration::from_secs(30);

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
    let mut body = vec![0; content_length(&mut r)?.unwrap_or(0)];
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

/// Reads the header lines up to the empty one; returns the `Content-Length`, if stated.
fn content_length(r: &mut impl BufRead) -> std::io::Result<Option<usize>> {
    let mut len = None;
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            return Ok(len);
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
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
    let err = |e: std::io::Error| format!("{method} {url}: {e}");
    let addr = host
        .to_socket_addrs()
        .map_err(err)?
        .next()
        .ok_or_else(|| format!("no address for {host}"))?;
    let mut s = TcpStream::connect_timeout(&addr, PATIENCE).map_err(err)?;
    s.set_read_timeout(Some(PATIENCE)).map_err(err)?;
    s.set_write_timeout(Some(PATIENCE)).map_err(err)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(ct) = ct {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).map_err(err)?;
    s.write_all(body).map_err(err)?;
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status).map_err(err)?;
    if status.is_empty() {
        return Err("no HTTP header".into());
    }
    // The stated length, when there is one: a server may keep the connection open.
    let len = content_length(&mut r).map_err(err)?;
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return Err(status.trim_end().to_string());
    }
    let mut resp = Vec::new();
    match len {
        Some(n) => {
            resp.resize(n, 0);
            r.read_exact(&mut resp).map_err(err)?;
        }
        None => {
            r.read_to_end(&mut resp).map_err(err)?;
        }
    }
    Ok(resp)
}

pub fn post(url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    request(url, "POST", Some(content_type), body)
}

pub fn get(url: &str) -> Result<Vec<u8>, String> {
    request(url, "GET", None, &[])
}
