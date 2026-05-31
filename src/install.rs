//! `ezbar install` — add ezbar to the user's sway config, idempotently and
//! without ever mutating an existing line.
//!
//! The decision + transform is a pure function ([`install_into_config`]) so it is
//! exhaustively unit-tested against real sway configs. The I/O shell ([`run`])
//! only locates the config, reads it, runs the pure function, and writes the
//! result back atomically (temp file + rename) with a backup — so a crash or a
//! bug can never leave a half-written or clobbered config.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Result of planning an install into a config body.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// ezbar already autostarts — the config is left exactly as-is.
    AlreadyPresent,
    /// ezbar was not present; carries the new config body to write (the original
    /// content is preserved verbatim as a prefix; we only append).
    Added(String),
}

/// Pure core: plan installing `exec_always <exe>` into a sway config `body`.
///
/// Idempotent: if any top-level `exec`/`exec_always` line already launches the
/// `ezbar` binary, returns [`Install::AlreadyPresent`] and changes nothing.
/// Otherwise returns [`Install::Added`] with the original body plus an appended
/// autostart line. It never edits or removes existing lines.
pub fn install_into_config(body: &str, exe: &str) -> Install {
    if body.lines().any(launches_ezbar_autostart) {
        return Install::AlreadyPresent;
    }
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("# ezbar status bar (added by `ezbar install`)\n");
    out.push_str("exec_always ");
    out.push_str(exe);
    out.push('\n');
    Install::Added(out)
}

/// True if `line` is a top-level `exec`/`exec_always` directive that launches the
/// `ezbar` binary. Precise on purpose:
/// - only the `exec`/`exec_always` directives count (a `bindsym … exec ezbar`
///   keybind is *not* autostart, so it does not block install);
/// - the launched program's basename must be exactly `ezbar` (so `ezbar-wrapper`,
///   `my-ezbar`, or `ezbar` inside an unrelated string do not false-match);
/// - a commented-out line (`# exec_always ezbar`) does not count.
fn launches_ezbar_autostart(line: &str) -> bool {
    let t = line.trim_start();
    let cmd = match t
        .strip_prefix("exec_always")
        .or_else(|| t.strip_prefix("exec"))
    {
        Some(rest) => rest,
        None => return false,
    };
    // The directive must be followed by whitespace, else it was a longer word
    // that merely starts with "exec" (e.g. a hypothetical `execfoo`).
    if !cmd.is_empty() && !cmd.starts_with(|c: char| c.is_whitespace()) {
        return false;
    }
    cmd.split_whitespace().any(|tok| {
        let tok = tok.trim_matches(['"', '\'']);
        let base = tok.rsplit('/').next().unwrap_or(tok);
        base == "ezbar"
    })
}

/// Locate the user's sway config: the first existing of `$XDG_CONFIG_HOME/sway/config`
/// then `~/.config/sway/config`; if neither exists, the preferred default path.
fn sway_config_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            candidates.push(PathBuf::from(xdg).join("sway/config"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            candidates.push(PathBuf::from(home).join(".config/sway/config"));
        }
    }
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

/// Write `contents` to `path` atomically: write a sibling temp file, then rename
/// over the target. A crash mid-write leaves the original config untouched.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad config path"))?;
    let tmp = path.with_file_name(format!(".{file_name}.ezbar-tmp"));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

/// I/O shell for `ezbar install`. Returns a human message on success.
pub fn run() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .map_err(|e| format!("cannot determine ezbar's own path: {e}"))?;

    let path =
        sway_config_path().ok_or("cannot locate a sway config ($HOME / $XDG_CONFIG_HOME unset)")?;

    if !path.exists() {
        return Err(format!(
            "no sway config at {}\n\nadd this line to your sway config yourself:\n    exec_always {exe}",
            path.display()
        ));
    }

    let body = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;

    match install_into_config(&body, &exe) {
        Install::AlreadyPresent => Ok(format!(
            "ezbar already autostarts in {} — nothing to do",
            path.display()
        )),
        Install::Added(new_body) => {
            // Best-effort backup, then atomic replace.
            let backup = path.with_file_name(format!(
                "{}.ezbar.bak",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("config")
            ));
            let _ = fs::copy(&path, &backup);
            write_atomic(&path, &new_body)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
            Ok(format!(
                "added `exec_always {exe}` to {}\n(backed up the old config to {})\n\nreload sway to start it:\n    swaymsg reload",
                path.display(),
                backup.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic sway config exercising the tricky bits: another bar (waybar),
    // a keybind that launches ezbar (NOT autostart), comments, and includes.
    const SAMPLE: &str = "\
set $mod Mod4
set $term foot

output * bg ~/wall.png fill

bindsym $mod+Return exec $term
bindsym $mod+d exec wofi
bindsym $mod+b exec ezbar

exec_always waybar
exec dunst

include /etc/sway/config.d/*
";

    fn added(body: &str, exe: &str) -> String {
        match install_into_config(body, exe) {
            Install::Added(s) => s,
            Install::AlreadyPresent => panic!("expected Added, got AlreadyPresent"),
        }
    }
    fn is_present(body: &str, exe: &str) -> bool {
        matches!(install_into_config(body, exe), Install::AlreadyPresent)
    }

    // ---- adding ----

    #[test]
    fn adds_to_empty_config() {
        let out = added("", "/usr/bin/ezbar");
        assert!(out.contains("exec_always /usr/bin/ezbar\n"));
        // re-detecting our own line proves a well-formed directive
        assert!(launches_ezbar_autostart("exec_always /usr/bin/ezbar"));
    }

    #[test]
    fn adds_to_realistic_config_and_preserves_everything() {
        let out = added(SAMPLE, "/usr/bin/ezbar");
        // original content is a verbatim prefix — nothing existing was touched
        assert!(
            out.starts_with(SAMPLE),
            "original config must be preserved verbatim"
        );
        // other bar untouched, our line appended
        assert!(out.contains("exec_always waybar\n"));
        assert!(out.contains("exec_always /usr/bin/ezbar\n"));
        // exactly one ezbar autostart line now
        let count = out.lines().filter(|l| launches_ezbar_autostart(l)).count();
        assert_eq!(count, 1, "exactly one ezbar autostart line");
    }

    #[test]
    fn keybind_launch_is_not_autostart_so_install_still_adds() {
        // SAMPLE has `bindsym $mod+b exec ezbar` — a keybind, not autostart.
        assert!(!is_present(SAMPLE, "ezbar"));
        let out = added(SAMPLE, "ezbar");
        assert!(out.contains("\nexec_always ezbar\n"));
    }

    #[test]
    fn appends_newline_when_body_lacks_trailing_one() {
        let out = added("set $mod Mod4", "/usr/bin/ezbar");
        assert!(out.starts_with("set $mod Mod4\n"));
        assert!(out.ends_with("exec_always /usr/bin/ezbar\n"));
    }

    // ---- idempotency ----

    #[test]
    fn idempotent_second_run_is_noop() {
        let once = added(SAMPLE, "/usr/bin/ezbar");
        assert!(
            is_present(&once, "/usr/bin/ezbar"),
            "second install must be a no-op"
        );
        // and installing with a *different* exe path is still a no-op (already there)
        assert!(is_present(&once, "ezbar"));
    }

    // ---- detection: positives ----

    #[test]
    fn detects_existing_autostart_variants() {
        for line in [
            "exec_always ezbar",
            "exec ezbar",
            "exec_always /usr/bin/ezbar",
            "exec_always /usr/local/bin/ezbar",
            "exec_always ezbar --some-flag",
            "\texec_always ezbar",   // tab-indented
            "   exec ezbar",         // space-indented
            "exec_always \"ezbar\"", // quoted
        ] {
            assert!(launches_ezbar_autostart(line), "should detect: {line:?}");
            assert!(is_present(&format!("set $mod Mod4\n{line}\n"), "ezbar"));
        }
    }

    // ---- detection: negatives (must NOT false-match → must still install) ----

    #[test]
    fn does_not_match_lookalikes() {
        for line in [
            "# exec_always ezbar",       // commented out
            "exec_always ezbar-wrapper", // different binary
            "exec_always my-ezbar",      // different binary
            "exec_always waybar",        // another bar
            "bindsym $mod+b exec ezbar", // keybind, not autostart
            "set $foo ezbar",            // not an exec directive
            "exec_alwaysx ezbar",        // not a real directive
            "# launch ezbar at startup", // prose comment
            "exec ezbard",               // ezbard != ezbar
        ] {
            assert!(
                !launches_ezbar_autostart(line),
                "should NOT match: {line:?}"
            );
        }
    }

    #[test]
    fn install_adds_even_when_lookalikes_present() {
        let cfg = "exec_always ezbar-wrapper\n# exec_always ezbar\nbindsym $mod+b exec ezbar\n";
        assert!(!is_present(cfg, "ezbar"));
        let out = added(cfg, "ezbar");
        assert!(out.starts_with(cfg), "lookalike lines preserved verbatim");
        assert!(out.trim_end().ends_with("exec_always ezbar"));
    }

    #[test]
    fn added_line_round_trips_for_path_and_bare_name() {
        for exe in [
            "ezbar",
            "/usr/bin/ezbar",
            "/home/me/.cargo/target/release/ezbar",
        ] {
            let out = added("", exe);
            assert!(
                is_present(&out, exe),
                "freshly-added {exe} must be detected"
            );
        }
    }
}
