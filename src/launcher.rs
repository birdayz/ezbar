//! Independent guards for the restarter and the process that owns bar surfaces.
//! A renderer keeps its own guard if its supervisor dies; foreground launches
//! take the same renderer guard rather than bypassing singleton protection.

use std::io;

#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::unix::net::{SocketAddr, UnixListener};

#[cfg(target_os = "linux")]
pub(crate) type Guard = UnixListener;
#[cfg(not(target_os = "linux"))]
pub(crate) type Guard = ();

/// Keep the original supervisor address so an existing launcher still excludes
/// another one after an upgrade. Renderers have a separate, canonical address.
pub(crate) fn supervisor_guard() -> io::Result<Option<Guard>> {
    #[cfg(target_os = "linux")]
    {
        let name = format!(
            "ezbar.{}.{}",
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(),
            std::env::var("WAYLAND_DISPLAY").unwrap_or_default()
        );
        bind_guard(name.as_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Some(()))
    }
}

/// Hold this guard for all of `run_bar()`, including foreground/debug launches.
/// Only an occupied address returns `None`; other startup errors are preserved.
pub(crate) fn renderer_guard() -> io::Result<Option<Guard>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        // /proc reports the process's uid without FFI or trusting an exported UID.
        let uid = std::fs::metadata("/proc/self")?.uid();
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| format!("/run/user/{uid}").into());
        let display = std::env::var_os("WAYLAND_DISPLAY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "wayland-0".into());
        let name = renderer_name(&runtime, &display, uid)?;
        bind_guard(name.as_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Some(()))
    }
}

#[cfg(target_os = "linux")]
fn bind_guard(name: &[u8]) -> io::Result<Option<UnixListener>> {
    let address = SocketAddr::from_abstract_name(name)?;
    match UnixListener::bind_addr(&address) {
        Ok(guard) => Ok(Some(guard)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn renderer_name(
    runtime: &std::path::Path,
    display: &std::path::Path,
    uid: u32,
) -> io::Result<String> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, PathBuf};

    let socket = if display.is_absolute() {
        display.to_path_buf()
    } else {
        runtime.join(display)
    };
    if !socket.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "XDG_RUNTIME_DIR must be absolute for a relative WAYLAND_DISPLAY",
        ));
    }
    // Resolve aliases when the socket exists. A missing socket can still be
    // named deterministically (e.g. while the compositor is starting).
    let socket = std::fs::canonicalize(&socket).unwrap_or_else(|_| {
        let mut normalized = PathBuf::new();
        for component in socket.components() {
            match component {
                Component::ParentDir => {
                    normalized.pop();
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        normalized
    });
    // Stable FNV-1a/128 keeps arbitrary/non-UTF8 paths below Linux's 107-byte
    // abstract-name limit without a dependency or version-dependent std hash.
    let mut hash = 0x6c62272e07bb014262b821756295c58du128;
    for byte in socket.as_os_str().as_bytes() {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
    }
    Ok(format!("ezbar.renderer.{uid}.{hash:032x}"))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn renderer_session_normalizes_absolute_relative_and_dot_paths() {
        let runtime = Path::new("/unlikely-ezbar-test-runtime/1000");
        let relative = renderer_name(runtime, Path::new("wayland-0"), 1000).unwrap();
        assert_eq!(
            relative,
            renderer_name(runtime, &runtime.join("wayland-0"), 1000).unwrap()
        );
        assert_eq!(
            relative,
            renderer_name(runtime, Path::new("./sub/../wayland-0"), 1000).unwrap()
        );
        assert_eq!(
            relative,
            renderer_name(
                Path::new("/other-runtime"),
                &runtime.join("wayland-0"),
                1000
            )
            .unwrap()
        );
    }

    #[test]
    fn renderer_session_separates_users_runtimes_and_displays() {
        let runtime = Path::new("/unlikely-ezbar-test-runtime/1000");
        let first = renderer_name(runtime, Path::new("wayland-0"), 1000).unwrap();
        assert_ne!(
            first,
            renderer_name(runtime, Path::new("wayland-0"), 1001).unwrap()
        );
        assert_ne!(
            first,
            renderer_name(runtime, Path::new("wayland-1"), 1000).unwrap()
        );
        assert_ne!(
            first,
            renderer_name(Path::new("/other-runtime"), Path::new("wayland-0"), 1000).unwrap()
        );
    }

    #[test]
    fn renderer_session_supports_long_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let mut bytes = vec![b'/'];
        bytes.extend(vec![b'x'; 200]);
        bytes.push(0xff);
        let name = renderer_name(
            Path::new(&OsString::from_vec(bytes)),
            Path::new("wayland-0"),
            u32::MAX,
        )
        .unwrap();
        assert!(name.len() <= 107);
        SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    }

    #[test]
    fn invalid_address_is_not_reported_as_contention() {
        assert!(bind_guard(&[b'x'; 108]).is_err());
        assert!(renderer_name(Path::new("relative"), Path::new("wayland-0"), 1000).is_err());
    }
}
