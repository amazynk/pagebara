//! [`HttpFetch`]: HTTPS downloads with ureq (rustls, Mozilla's root certificates).

use std::io::{self, Read, Write};

use crate::download::Fetch;

/// Downloads over HTTPS, following redirects.
#[derive(Clone, Copy, Debug, Default)]
pub struct HttpFetch;

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()> {
        let resp = ureq::get(url).call().map_err(io::Error::other)?;
        let mut body = resp.into_body().into_with_config().limit(64 << 20).reader();
        let mut buf = vec![0; 64 << 10];
        let mut done = 0u64;
        loop {
            let n = body.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done);
        }
    }
}
