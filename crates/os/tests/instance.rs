//! The single-instance channel, between threads and between processes: forwarding, the
//! race of many launches at once, and a socket left behind by an instance that died.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pagebara_os::{Instance, InstanceKey, OpenFile, OpenRequest};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pagebara-inst-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A request for `path`, which starts at the root. A request is only taken with absolute
/// paths, and on Windows that needs a drive.
fn request(path: &str, page: Option<usize>) -> OpenRequest {
    let drive = if cfg!(windows) { "C:" } else { "" };
    OpenRequest {
        files: vec![OpenFile {
            path: PathBuf::from(format!("{drive}{path}")),
            page,
            search: None,
        }],
    }
}

/// This test binary, running one test as a child process.
fn child(test: &str, dir: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", test, "--ignored", "--nocapture"])
        .env("PAGEBARA_INSTANCE_DIR", dir);
    c
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn second_launch_forwards_and_new_window_runs_alone() {
    let dir = temp("threads");
    let key = InstanceKey::in_dir(&dir, "instance");
    let Instance::Primary(server) = Instance::start(&key, None) else {
        panic!("the first launch is the instance");
    };
    let (tx, rx) = mpsc::channel();
    let _reg = server.serve(move |r| tx.send(r).unwrap());
    let req = request("/docs/a.pdf", Some(2));
    assert!(matches!(
        Instance::start(&key, Some(&req)),
        Instance::Forwarded
    ));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), req);
    // An empty request (raise the window) also goes through.
    let empty = OpenRequest::default();
    assert!(matches!(
        Instance::start(&key, Some(&empty)),
        Instance::Forwarded
    ));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), empty);
    // `--new-window`: nothing forwarded.
    assert!(matches!(Instance::start(&key, None), Instance::Alone));
    assert!(rx.try_recv().is_err());
    drop(_reg);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn many_launches_at_once_make_one_instance() {
    let dir = temp("race");
    let key = InstanceKey::in_dir(&dir, "instance");
    let (tx, rx) = mpsc::channel();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let (key, tx) = (key.clone(), tx.clone());
            pagebara_thread::spawn(format!("test-race-{i}"), move || {
                match Instance::start(&key, Some(&request(&format!("/d/{i}.pdf"), None))) {
                    Instance::Primary(s) => {
                        // Keep serving until the test ends.
                        std::mem::forget(s.serve(move |r| tx.send(r).unwrap()));
                        "primary"
                    }
                    Instance::Forwarded => "forwarded",
                    Instance::Alone => "alone",
                }
            })
        })
        .collect();
    let mut roles: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    roles.sort();
    assert_eq!(
        roles.iter().filter(|r| **r == "primary").count(),
        1,
        "{roles:?}"
    );
    assert_eq!(
        roles.iter().filter(|r| **r == "forwarded").count(),
        7,
        "{roles:?}"
    );
    for _ in 0..7 {
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_dead_instances_socket_is_replaced() {
    let dir = temp("stale");
    let key = InstanceKey::in_dir(&dir, "instance");
    // An instance that died without cleaning up: its socket file stays, its lock goes.
    let status = child("child_dies_holding_the_instance", &dir)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        key.socket_path().exists(),
        "the dead instance left its socket"
    );
    let req = request("/docs/b.pdf", None);
    let Instance::Primary(server) = Instance::start(&key, Some(&req)) else {
        panic!("a dead instance's socket must not block the next launch");
    };
    let (tx, rx) = mpsc::channel();
    let _reg = server.serve(move |r| tx.send(r).unwrap());
    assert!(matches!(
        Instance::start(&key, Some(&req)),
        Instance::Forwarded
    ));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), req);
    drop(_reg);
    assert!(
        !key.socket_path().exists(),
        "the registration removes its socket"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn forwards_to_another_process() {
    let dir = temp("procs");
    let key = InstanceKey::in_dir(&dir, "instance");
    let received = dir.join("received.json");
    let mut first = child("child_instance_records_one_request", &dir)
        .spawn()
        .unwrap();
    wait_for("the child instance is ready", || dir.join("ready").exists());
    let req = request("/docs/c.pdf", Some(4));
    assert!(matches!(
        Instance::start(&key, Some(&req)),
        Instance::Forwarded
    ));
    assert!(first.wait().unwrap().success());
    let line = std::fs::read_to_string(&received).unwrap();
    assert_eq!(OpenRequest::from_line(&line), Some(req));
    std::fs::remove_dir_all(dir).unwrap();
}

/// Child process: becomes the instance and exits at once, leaving its socket.
#[test]
#[ignore = "run by a_dead_instances_socket_is_replaced"]
fn child_dies_holding_the_instance() {
    let Some(dir) = std::env::var_os("PAGEBARA_INSTANCE_DIR") else {
        return;
    };
    let key = InstanceKey::in_dir(PathBuf::from(dir), "instance");
    let Instance::Primary(server) = Instance::start(&key, None) else {
        std::process::exit(3);
    };
    std::mem::forget(server);
    std::process::exit(0);
}

/// Child process: the instance; writes the first request it gets, then exits.
#[test]
#[ignore = "run by forwards_to_another_process"]
fn child_instance_records_one_request() {
    let Some(dir) = std::env::var_os("PAGEBARA_INSTANCE_DIR").map(PathBuf::from) else {
        return;
    };
    let key = InstanceKey::in_dir(&dir, "instance");
    let Instance::Primary(server) = Instance::start(&key, None) else {
        std::process::exit(3);
    };
    let (tx, rx) = mpsc::channel();
    let _reg = server.serve(move |r| tx.send(r).unwrap());
    std::fs::write(dir.join("ready"), b"").unwrap();
    let r = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    std::fs::write(dir.join("received.json"), r.to_line().unwrap()).unwrap();
}
