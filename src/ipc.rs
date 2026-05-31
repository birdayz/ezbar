//! `ezbar msg …` IPC (RFC 0002): a tiny unix-socket so compositor keybinds drive
//! the running bar. The CLU side ([`send`]) connects and writes one line; the
//! daemon listens on the same socket and maps each line to a `Message`.

use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/ezbar.sock`, else `/tmp/ezbar.sock`.
pub fn socket_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    PathBuf::from(base).join("ezbar.sock")
}

/// CLI side: send a one-line command to the running bar and return.
pub fn send(cmd: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(socket_path())?;
    writeln!(s, "{cmd}")?;
    s.flush()
}
