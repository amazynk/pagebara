//! Runs a command as a child process with a wall-clock timeout and an address-space cap,
//! so a crash, hang or runaway allocation ends that child and nothing else.

use std::io::{self, Read};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub timeout: Duration,
    /// Virtual memory cap in MiB (`ulimit -v`); 0 for none.
    pub memory_mb: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    Timeout,
}

#[derive(Debug)]
pub struct Outcome {
    pub exit: Exit,
    pub stdout: String,
    /// The last [`STDERR_TAIL`] bytes.
    pub stderr: String,
    pub seconds: f64,
}

pub const STDERR_TAIL: usize = 16 << 10;
const STDOUT_MAX: usize = 4 << 20;

impl Outcome {
    /// Whether the allocator gave up (a Rust abort on a failed allocation).
    pub fn out_of_memory(&self) -> bool {
        self.stderr.contains("memory allocation of") || self.stderr.contains("capacity overflow")
    }
}

/// Runs `program args...` under `limits`, capturing its output.
pub fn run(program: &str, args: &[&str], limits: Limits) -> io::Result<Outcome> {
    let mut cmd = if limits.memory_mb > 0 {
        let mut c = Command::new("sh");
        c.args(["-c", "ulimit -v \"$1\" && shift && exec \"$@\"", "sh"])
            .arg((limits.memory_mb * 1024).to_string())
            .arg(program);
        c
    } else {
        Command::new(program)
    };
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Fewer malloc arenas keep the address space close to what is really used.
        .env("MALLOC_ARENA_MAX", "2")
        .env("RAYON_NUM_THREADS", "1")
        .env("RUST_BACKTRACE", "0");
    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let out = drain(child.stdout.take(), STDOUT_MAX, false);
    let err = drain(child.stderr.take(), STDERR_TAIL, true);
    let exit = loop {
        if let Some(st) = child.try_wait()? {
            break match (st.code(), st.signal()) {
                (Some(c), _) => Exit::Code(c),
                (None, Some(s)) => Exit::Signal(s),
                _ => Exit::Code(-1),
            };
        }
        if start.elapsed() > limits.timeout {
            let _ = child.kill();
            let _ = child.wait();
            break Exit::Timeout;
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(Outcome {
        exit,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        seconds: start.elapsed().as_secs_f64(),
    })
}

/// Reads a pipe to its end on a thread, keeping the first (or, with `tail`, the last)
/// `max` bytes.
fn drain(
    pipe: Option<impl Read + Send + 'static>,
    max: usize,
    tail: bool,
) -> thread::JoinHandle<String> {
    pagebara_thread::spawn("drain", move || {
        let Some(mut pipe) = pipe else {
            return String::new();
        };
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > max {
                if tail {
                    kept.drain(..kept.len() - max);
                } else {
                    kept.truncate(max);
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// Runs `jobs` closures over `items` on that many threads, in any order.
pub fn parallel<T: Sync, R: Send>(
    items: &[T],
    jobs: usize,
    f: impl Fn(&T) -> R + Sync,
    mut done: impl FnMut(usize, R),
) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel();
    thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            let tx = tx.clone();
            let (next, f) = (&next, &f);
            pagebara_thread::spawn_scoped(s, "check", move || {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    if tx.send((i, f(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (i, r) in rx {
            done(i, r);
        }
    });
}
