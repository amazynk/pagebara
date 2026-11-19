//! One running instance per user (and per `--profile`): a second launch hands its files to
//! the first over a local socket (a Unix domain socket in `XDG_RUNTIME_DIR`, or a named
//! pipe on Windows) and exits.
//!
//! Race-safe by a lock file: whoever holds the lock is the instance and listens; the lock
//! goes with the process, so a crashed instance leaves no owner and its socket file is
//! replaced by the next one (stale-socket recovery). A launch that finds the lock taken
//! connects, retrying while the owner starts up; if nobody answers it runs alone.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::request::OpenRequest;

/// Longest request accepted (a few thousand paths).
const MAX_REQUEST: u64 = 1 << 20;
/// How long a second launch keeps trying to reach the first.
const CONNECT_FOR: Duration = Duration::from_secs(4);
/// How long it waits for the first to confirm.
const ANSWER_WITHIN: Duration = Duration::from_secs(4);

/// Where an instance's lock and socket live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceKey {
    dir: PathBuf,
    name: String,
}

impl InstanceKey {
    /// The per-user key; with a profile directory, one instance per profile.
    pub fn new(profile: Option<&Path>) -> Option<InstanceKey> {
        let name = match profile {
            Some(p) => {
                let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
                format!("instance-{:016x}", fnv1a(abs.to_string_lossy().as_bytes()))
            }
            None => "instance".to_owned(),
        };
        Some(InstanceKey {
            dir: runtime_dir()?,
            name,
        })
    }

    /// A key in `dir` (tests).
    pub fn in_dir(dir: impl Into<PathBuf>, name: &str) -> InstanceKey {
        InstanceKey {
            dir: dir.into(),
            name: name.to_owned(),
        }
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join(format!("{}.lock", self.name))
    }

    /// The Unix socket's path.
    pub fn socket_path(&self) -> PathBuf {
        self.dir.join(format!("{}.sock", self.name))
    }

    /// The Windows pipe's name: per user, per key.
    pub fn pipe_name(&self) -> String {
        let user = std::env::var("USERNAME").unwrap_or_default();
        let dir = self.dir.to_string_lossy();
        format!(
            "{}-{}-{:016x}",
            pagebara_identity::APP_ID,
            self.name,
            fnv1a(format!("{user}\0{dir}").as_bytes())
        )
    }
}

/// This process's role.
#[derive(Debug)]
pub enum Instance {
    /// The first instance: holds the lock; [`Server::serve`] takes the requests.
    Primary(Server),
    /// The request went to the running instance: exit.
    Forwarded,
    /// Run alone (no per-user directory, or the running instance did not answer).
    Alone,
}

impl Instance {
    /// Becomes the instance, or hands `request` to the running one. With `request` `None`
    /// (`--new-window` and the like) nothing is forwarded: the process runs alone when an
    /// instance is already running.
    pub fn start(key: &InstanceKey, request: Option<&OpenRequest>) -> Instance {
        let lock = match take_lock(key) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                return match request {
                    Some(r) => match forward(key, r) {
                        Ok(()) => Instance::Forwarded,
                        Err(e) => {
                            log::warn!("the running instance did not answer ({e}); opening here");
                            Instance::Alone
                        }
                    },
                    None => Instance::Alone,
                };
            }
            Err(e) => {
                log::warn!("single instance: {e}");
                return Instance::Alone;
            }
        };
        match sys::bind(key) {
            Ok(listener) => Instance::Primary(Server {
                key: key.clone(),
                _lock: lock,
                listener,
            }),
            Err(e) => {
                log::warn!("single instance: cannot listen: {e}");
                Instance::Alone
            }
        }
    }
}

/// The first instance's end of the channel.
#[derive(Debug)]
pub struct Server {
    key: InstanceKey,
    _lock: File,
    listener: sys::Listener,
}

/// Keeps the instance registered; dropping it lets the next launch become the instance
/// (before restarting after an update).
#[derive(Debug)]
pub struct Registration {
    key: InstanceKey,
    _lock: File,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if cfg!(unix) {
            let _ = std::fs::remove_file(self.key.socket_path());
        }
    }
}

impl Server {
    /// Answers requests on a background thread, calling `handler` for each.
    pub fn serve(self, handler: impl Fn(OpenRequest) + Send + 'static) -> Registration {
        let Server {
            key,
            _lock,
            listener,
        } = self;
        pagebara_thread::spawn("rc-accept", move || {
            for conn in sys::incoming(&listener) {
                match conn.and_then(|c| answer(c, &handler)) {
                    Ok(()) => {}
                    Err(e) => log::warn!("single instance: a request failed: {e}"),
                }
            }
        });
        Registration { key, _lock }
    }
}

fn answer(mut conn: sys::Stream, handler: &impl Fn(OpenRequest)) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new((&mut conn).take(MAX_REQUEST)).read_line(&mut line)?;
    match OpenRequest::from_line(&line) {
        Some(req) => {
            conn.write_all(b"ok\n")?;
            handler(req);
            Ok(())
        }
        None => {
            conn.write_all(b"error\n")?;
            Err(io::Error::new(io::ErrorKind::InvalidData, "bad request"))
        }
    }
}

/// The lock, or `None` when another process holds it.
fn take_lock(key: &InstanceKey) -> io::Result<Option<File>> {
    create_private_dir(&key.dir)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(key.lock_path())?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Sends `req` to the running instance, retrying while it starts listening.
fn forward(key: &InstanceKey, req: &OpenRequest) -> io::Result<()> {
    let line = req
        .to_line()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    let start = Instant::now();
    let mut conn = loop {
        match sys::connect(key) {
            Ok(c) => break c,
            Err(e) if start.elapsed() > CONNECT_FOR => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    // A hung instance must not hang this launch: the exchange runs on a thread.
    let (tx, rx) = mpsc::channel();
    pagebara_thread::spawn("rc-client", move || {
        let _ = tx.send(exchange(&mut conn, &line));
    });
    rx.recv_timeout(ANSWER_WITHIN)
        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "no answer")))
}

fn exchange(conn: &mut sys::Stream, line: &str) -> io::Result<()> {
    conn.write_all(line.as_bytes())?;
    conn.flush()?;
    let mut answer = String::new();
    BufReader::new(conn.take(64)).read_line(&mut answer)?;
    if answer.trim() == "ok" {
        Ok(())
    } else {
        Err(io::Error::other(format!("refused: {}", answer.trim())))
    }
}

/// The per-user directory for the lock and socket: `$XDG_RUNTIME_DIR/<app id>` (in a
/// Flatpak, `$XDG_RUNTIME_DIR/app/<id>`, shared by its instances), else the cache folder.
fn runtime_dir() -> Option<PathBuf> {
    let id = pagebara_identity::APP_ID;
    if cfg!(all(unix, not(target_os = "macos")))
        && let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())
    {
        let run = PathBuf::from(run);
        return Some(match std::env::var("FLATPAK_ID") {
            Ok(flatpak) => run.join("app").join(flatpak),
            Err(_) => run.join(id),
        });
    }
    // macOS: the per-user temporary folder ($TMPDIR) is short enough for a socket path.
    if cfg!(target_os = "macos") {
        return Some(std::env::temp_dir().join(id));
    }
    pagebara_identity::cache_dir().map(|d| d.join("instance"))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// A stable hash (the key must match between builds).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(unix)]
mod sys {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::Duration;

    use super::InstanceKey;

    pub type Listener = UnixListener;
    pub type Stream = UnixStream;

    /// Listens, replacing a socket file left by an instance that died (the caller holds
    /// the lock, so no live instance owns it).
    pub fn bind(key: &InstanceKey) -> io::Result<Listener> {
        let path = key.socket_path();
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        UnixListener::bind(path)
    }

    pub fn incoming(l: &Listener) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        l.incoming().map(|c| {
            let c = c?;
            c.set_read_timeout(Some(Duration::from_secs(2)))?;
            c.set_write_timeout(Some(Duration::from_secs(2)))?;
            Ok(c)
        })
    }

    pub fn connect(key: &InstanceKey) -> io::Result<Stream> {
        UnixStream::connect(key.socket_path())
    }
}

#[cfg(windows)]
mod sys {
    use std::io;

    use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};

    use super::InstanceKey;

    pub type Listener = interprocess::local_socket::Listener;
    pub type Stream = interprocess::local_socket::Stream;

    pub fn bind(key: &InstanceKey) -> io::Result<Listener> {
        let name = key.pipe_name().to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new().name(name).create_sync()
    }

    pub fn incoming(l: &Listener) -> impl Iterator<Item = io::Result<Stream>> + '_ {
        l.incoming()
    }

    pub fn connect(key: &InstanceKey) -> io::Result<Stream> {
        let name = key.pipe_name().to_ns_name::<GenericNamespaced>()?;
        Stream::connect(name)
    }
}
