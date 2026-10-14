#![cfg(target_os = "linux")]

// Also runnable without the application's dependency graph:
// rustc --edition=2021 --test tests/launcher.rs -o target/launcher-tests
#[path = "../src/launcher.rs"]
mod launcher;

use std::io::{BufRead, Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

struct Session {
    runtime: String,
}

impl Session {
    fn new() -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self {
            runtime: format!(
                "/tmp/ezbar-test-{}-{time:x}-{}",
                std::process::id(),
                NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
            ),
        }
    }

    fn command(&self, role: &str) -> Command {
        let mut command = helper_command(role);
        command
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env_remove("EZBAR_TEST_CONTROL")
            .env_remove("EZBAR_TEST_SPAWN_RENDERER");
        command
    }
}

fn helper_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "launcher_process_helper",
            "--nocapture",
        ])
        .env("EZBAR_TEST_ROLE", role);
    command
}

struct Worker {
    child: Child,
    lines: Receiver<String>,
}

impl Worker {
    fn spawn(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if let Some(message) = line.strip_prefix("launcher-test:") {
                    if tx.send(message.to_string()).is_err() {
                        break;
                    }
                }
            }
        });
        Self { child, lines }
    }

    fn status(&self) -> String {
        self.lines
            .recv_timeout(TIMEOUT)
            .expect("helper did not become ready")
    }

    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.wait();
    }

    fn wait(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let deadline = Instant::now() + TIMEOUT;
        while matches!(self.child.try_wait(), Ok(None)) {
            if Instant::now() >= deadline {
                eprintln!(
                    "launcher helper {} did not exit after kill",
                    self.child.id()
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn report(message: &str) {
    println!("launcher-test:{message}");
    std::io::stdout().flush().unwrap();
}

/// Subprocess fixture: never starts the GUI or uses the desktop session's names.
#[test]
#[ignore]
fn launcher_process_helper() {
    let Ok(role) = std::env::var("EZBAR_TEST_ROLE") else {
        return;
    };
    let guard = match role.as_str() {
        "supervisor" => launcher::supervisor_guard(),
        "renderer" => launcher::renderer_guard(),
        _ => panic!("unknown test role"),
    }
    .unwrap();
    let Some(guard) = guard else {
        report(&format!("busy:{role}"));
        return;
    };
    let mut control = std::env::var("EZBAR_TEST_CONTROL").ok().and_then(|name| {
        if role == "renderer" {
            let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
            Some(UnixStream::connect_addr(&address).unwrap())
        } else {
            None
        }
    });
    let _renderer = if std::env::var_os("EZBAR_TEST_SPAWN_RENDERER").is_some() {
        Some(
            helper_command("renderer")
                .env_remove("EZBAR_TEST_SPAWN_RENDERER")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    } else {
        None
    };
    report(&format!("held:{role}"));
    let mut byte = [0];
    if let Some(stream) = control.as_mut() {
        let _ = stream.read(&mut byte);
    } else {
        let _ = std::io::stdin().read(&mut byte);
    }
    drop(guard);
    report(&format!("released:{role}"));
}

#[test]
fn supervisor_address_remains_compatible() {
    let session = Session::new();
    let name = format!("ezbar.{}.wayland-0", session.runtime);
    let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let _old_supervisor = UnixListener::bind_addr(&address).unwrap();
    let mut worker = Worker::spawn(session.command("supervisor"));
    assert_eq!(worker.status(), "busy:supervisor");
    worker.wait();
}

#[test]
fn renderer_excludes_foreground_relaunch_and_releases_on_crash() {
    let session = Session::new();
    let mut first = Worker::spawn(session.command("renderer"));
    assert_eq!(first.status(), "held:renderer");
    let mut duplicate = Worker::spawn(session.command("renderer"));
    assert_eq!(duplicate.status(), "busy:renderer");
    duplicate.wait();
    first.kill();
    let replacement = Worker::spawn(session.command("renderer"));
    assert_eq!(replacement.status(), "held:renderer");
}

#[test]
fn orphan_renderer_still_excludes_relaunch_after_supervisor_sigkill() {
    let session = Session::new();
    let control_name = format!("{}.control", session.runtime);
    let address = SocketAddr::from_abstract_name(control_name.as_bytes()).unwrap();
    let control = UnixListener::bind_addr(&address).unwrap();
    control.set_nonblocking(true).unwrap();
    let mut command = session.command("supervisor");
    command
        .env("EZBAR_TEST_CONTROL", &control_name)
        .env("EZBAR_TEST_SPAWN_RENDERER", "1");
    let mut supervisor = Worker::spawn(command);
    let mut statuses = vec![supervisor.status(), supervisor.status()];
    statuses.sort();
    assert_eq!(statuses, ["held:renderer", "held:supervisor"]);
    let deadline = Instant::now() + TIMEOUT;
    let connection = loop {
        match control.accept() {
            Ok((connection, _)) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "orphan control did not connect");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("orphan control failed: {error}"),
        }
    };
    supervisor.kill(); // Child::kill is SIGKILL on Unix, not graceful cleanup.
    let replacement_supervisor = Worker::spawn(session.command("supervisor"));
    assert_eq!(replacement_supervisor.status(), "held:supervisor");
    let mut duplicate_renderer = Worker::spawn(session.command("renderer"));
    assert_eq!(duplicate_renderer.status(), "busy:renderer");
    duplicate_renderer.wait();
    drop(connection); // Stops the orphan even if the test subsequently fails.
    assert_eq!(supervisor.status(), "released:renderer");
    let replacement_renderer = Worker::spawn(session.command("renderer"));
    assert_eq!(replacement_renderer.status(), "held:renderer");
}

#[test]
fn parallel_launches_have_one_owner_per_role() {
    for role in ["supervisor", "renderer"] {
        let session = Session::new();
        let mut workers: Vec<_> = (0..8)
            .map(|_| Worker::spawn(session.command(role)))
            .collect();
        let mut held = 0;
        for worker in &mut workers {
            match worker.status().as_str() {
                status if status == format!("held:{role}") => held += 1,
                status if status == format!("busy:{role}") => worker.wait(),
                status => panic!("unexpected helper status: {status}"),
            }
        }
        assert_eq!(held, 1);
    }
}

#[test]
fn missing_runtime_uses_uid_scoped_default() {
    use std::os::unix::fs::MetadataExt;
    let session = Session::new();
    let display = session.runtime.rsplit('/').next().unwrap();
    let mut absent = session.command("renderer");
    absent
        .env_remove("XDG_RUNTIME_DIR")
        .env("WAYLAND_DISPLAY", display);
    let owner = Worker::spawn(absent);
    assert_eq!(owner.status(), "held:renderer");
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut explicit = session.command("renderer");
    explicit
        .env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"))
        .env("WAYLAND_DISPLAY", display);
    let mut duplicate = Worker::spawn(explicit);
    assert_eq!(duplicate.status(), "busy:renderer");
    duplicate.wait();
}

#[test]
fn sessions_are_independent_and_display_aliases_share_renderer_guard() {
    let first = Session::new();
    let second = Session::new();
    let owner = Worker::spawn(first.command("renderer"));
    assert_eq!(owner.status(), "held:renderer");
    let independent = Worker::spawn(second.command("renderer"));
    assert_eq!(independent.status(), "held:renderer");
    let mut absolute = first.command("renderer");
    absolute.env("WAYLAND_DISPLAY", format!("{}/wayland-0", first.runtime));
    let mut alias = Worker::spawn(absolute);
    assert_eq!(alias.status(), "busy:renderer");
    alias.wait();
    let mut unset = first.command("renderer");
    unset.env_remove("WAYLAND_DISPLAY");
    let mut default_display = Worker::spawn(unset);
    assert_eq!(default_display.status(), "busy:renderer");
    default_display.wait();
}
