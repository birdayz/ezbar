//! Pure logic for the ezbar `claude` plugin, factored out so it can be **unit-tested on the
//! host**. The plugin crate (`wasm/claude`) is `cdylib`-only and links a wasm-only SDK, so its
//! own functions can't run under `cargo test`; this crate has no I/O and no SDK, builds for both
//! the host and `wasm32-wasip2`, and the plugin depends on it for the fiddly, bug-prone bits —
//! the `/proc/<pid>/stat` parse, Claude's project-dir encoding, the rate-limit projection, etc.

use std::collections::{HashMap, HashSet};

/// Severity for a thresholded value; the renderer maps it to a theme token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Urgent,
}

/// Colour level for a rate-limit by percent **used**: green with headroom, amber past 70%,
/// red past 85% — it reddens as you approach the cap.
pub fn usage_level(used: f64) -> Level {
    if used > 85.0 {
        Level::Urgent
    } else if used > 70.0 {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// Colour level for a waiting agent by how long it's been quiet: amber, then red past
/// `red_secs`. Only ever Warn/Urgent — it's called only for agents already flagged waiting.
pub fn idle_level(secs: i64, red_secs: i64) -> Level {
    if secs >= red_secs {
        Level::Urgent
    } else {
        Level::Warn
    }
}

/// Fields lifted from a `/proc/<pid>/stat` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    /// `utime + stime` in clock ticks.
    pub cpu: u64,
}

/// Parse a `/proc/<pid>/stat` line. The format is `pid (comm) state ppid … utime stime …`, and
/// `comm` can itself contain spaces and parentheses — so we split on the FIRST `(` and the LAST
/// `)` (matching the kernel), then index the space-separated tail: `[0]=state, [1]=ppid,
/// [11]=utime, [12]=stime`. `None` if there's no parenthesised comm.
pub fn parse_stat(raw: &str) -> Option<Stat> {
    let open = raw.find('(')?;
    let close = raw.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = raw.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = raw[close + 1..].split_whitespace().collect();
    let field = |i: usize| rest.get(i).copied().unwrap_or("");
    let state = field(0).chars().next().unwrap_or('?');
    let ppid: i32 = field(1).parse().unwrap_or(0);
    let utime: u64 = field(11).parse().unwrap_or(0);
    let stime: u64 = field(12).parse().unwrap_or(0);
    Some(Stat {
        comm,
        state,
        ppid,
        cpu: utime + stime,
    })
}

/// Claude stores a session transcript under `~/.claude/projects/<dir>`, where `<dir>` is the cwd
/// with **every non-alphanumeric char replaced by `-`** (so `/`, `.`, `_`, spaces all collapse;
/// existing `-` survives). E.g. `…/esp-iot/.claude-worktrees` ⇒ `…-esp-iot--claude-worktrees`.
pub fn encode_project(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Pull `PWD` out of a NUL-separated `/proc/<pid>/environ` blob (the cwd fallback when the
/// cap-std sandbox refuses to `readlink` the `/proc/<pid>/cwd` magic symlink).
pub fn pwd_from_environ(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .find_map(|kv| kv.strip_prefix("PWD=").map(|s| s.to_string()))
}

/// Compact idle time: `45s` / `8m` / `2h`.
pub fn idle_str(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Human duration with a `now` floor: `now` / `8m` / `2h` / `3d`.
pub fn human_dur(secs: i64) -> String {
    if secs <= 0 {
        "now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The last non-empty path segment (basename), or the whole string if there is none.
pub fn base(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

/// Labels for a list of cwds (same order out): the basename, or `parent/basename` when two cwds
/// share a basename — so "which agent is which" stays answerable across worktrees / monorepo
/// subdirs / two terminals in one repo.
pub fn disambiguate_labels(cwds: &[String]) -> Vec<String> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for c in cwds {
        *counts.entry(base(c)).or_default() += 1;
    }
    cwds.iter()
        .map(|c| {
            let b = base(c);
            if counts.get(b).copied().unwrap_or(0) > 1 {
                let parent = c.rsplit('/').nth(1).unwrap_or("");
                format!("{parent}/{b}")
            } else {
                b.to_string()
            }
        })
        .collect()
}

/// Project seconds until a percentage reaches 100 at the recent fill rate, from sparse
/// `(epoch, percent)` samples. `None` unless there's a `≥ min_span`-second baseline with a
/// **rising** trend of at least `min_rise` percent — so it never extrapolates flat noise or a
/// post-reset drop. Used for "you'll hit the 5h limit in ~X".
pub fn project_to_full(samples: &[(i64, f64)], min_span: i64, min_rise: f64) -> Option<i64> {
    let (t0, u0) = *samples.first()?;
    let (t1, u1) = *samples.last()?;
    let span = t1 - t0;
    let rise = u1 - u0;
    if span < min_span || rise < min_rise {
        return None;
    }
    let rate = rise / span as f64; // percent/sec
    Some(((100.0 - u1) / rate) as i64)
}

/// True if any transitive descendant of `pid` is in `active` (a process doing work *right now*).
/// Bounded DFS over a `ppid → children` map. Mere existence of a child is not enough — only an
/// active one counts, so an idle resident MCP/LSP child doesn't pin its parent to "working".
pub fn has_active_descendant(
    pid: i32,
    children: &HashMap<i32, Vec<i32>>,
    active: &HashSet<i32>,
) -> bool {
    let mut stack: Vec<i32> = children.get(&pid).cloned().unwrap_or_default();
    let mut seen = 0;
    while let Some(p) = stack.pop() {
        seen += 1;
        if seen > 512 {
            break; // cycle/runaway guard
        }
        if active.contains(&p) {
            return true;
        }
        if let Some(kids) = children.get(&p) {
            stack.extend(kids);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_level_ramps_with_consumption() {
        assert_eq!(usage_level(0.0), Level::Ok);
        assert_eq!(usage_level(70.0), Level::Ok); // boundary is exclusive (>70)
        assert_eq!(usage_level(70.1), Level::Warn);
        assert_eq!(usage_level(85.0), Level::Warn); // >85
        assert_eq!(usage_level(85.1), Level::Urgent);
        assert_eq!(usage_level(100.0), Level::Urgent);
    }

    #[test]
    fn idle_level_reddens_past_threshold() {
        assert_eq!(idle_level(0, 300), Level::Warn);
        assert_eq!(idle_level(299, 300), Level::Warn);
        assert_eq!(idle_level(300, 300), Level::Urgent); // inclusive
        assert_eq!(idle_level(10_000, 300), Level::Urgent);
    }

    #[test]
    fn parse_stat_plain() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime …
        let raw = "2672089 (claude) S 2671000 2672089 2671000 34816 2672089 4194304 12 0 0 0 4200 1800 0 0";
        let s = parse_stat(raw).unwrap();
        assert_eq!(s.comm, "claude");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 2671000);
        assert_eq!(s.cpu, 4200 + 1800); // utime + stime
    }

    #[test]
    fn parse_stat_comm_with_spaces() {
        let raw = "100 (npm exec foo) R 1 1 1 0 0 0 0 0 0 0 10 20 0 0";
        let s = parse_stat(raw).unwrap();
        assert_eq!(s.comm, "npm exec foo");
        assert_eq!(s.state, 'R');
        assert_eq!(s.cpu, 30);
    }

    #[test]
    fn parse_stat_comm_with_parens() {
        // systemd's literal "(sd-pam)" comm — split on first '(' and last ')'.
        let raw = "999 ((sd-pam)) S 1 1 1 0 0 0 0 0 0 0 0 0";
        let s = parse_stat(raw).unwrap();
        assert_eq!(s.comm, "(sd-pam)");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 1);
        assert_eq!(s.cpu, 0);
    }

    #[test]
    fn parse_stat_malformed() {
        assert!(parse_stat("not a stat line").is_none());
        assert!(parse_stat("").is_none());
    }

    #[test]
    fn encode_project_matches_claude_scheme() {
        assert_eq!(
            encode_project("/home/birdy/projects/fdb-record-layer-go"),
            "-home-birdy-projects-fdb-record-layer-go"
        );
        assert_eq!(
            encode_project("/home/birdy/projects/sommerurlaub-2026"),
            "-home-birdy-projects-sommerurlaub-2026"
        );
        // `/.` → `--`, `_` and spaces → `-`, existing `-` survives.
        assert_eq!(
            encode_project("/x/esp-iot/.claude-worktrees/bm 1"),
            "-x-esp-iot--claude-worktrees-bm-1"
        );
        assert_eq!(encode_project("/a/foo_bar"), "-a-foo-bar");
    }

    #[test]
    fn pwd_from_environ_finds_pwd() {
        assert_eq!(
            pwd_from_environ(b"FOO=1\0PWD=/home/x\0BAR=2\0").as_deref(),
            Some("/home/x")
        );
        assert_eq!(pwd_from_environ(b"FOO=1\0BAR=2\0"), None);
        // `PWD` must match as a key prefix, not mid-string.
        assert_eq!(pwd_from_environ(b"OLDPWD=/y\0").as_deref(), None);
        assert_eq!(pwd_from_environ(b"PWD=\0").as_deref(), Some("")); // empty but present
    }

    #[test]
    fn idle_str_units() {
        assert_eq!(idle_str(0), "0s");
        assert_eq!(idle_str(59), "59s");
        assert_eq!(idle_str(60), "1m");
        assert_eq!(idle_str(3599), "59m");
        assert_eq!(idle_str(3600), "1h");
        assert_eq!(idle_str(7200), "2h");
    }

    #[test]
    fn human_dur_units() {
        assert_eq!(human_dur(-5), "now");
        assert_eq!(human_dur(0), "now");
        assert_eq!(human_dur(59), "0m");
        assert_eq!(human_dur(60), "1m");
        assert_eq!(human_dur(3600), "1h");
        assert_eq!(human_dur(90_000), "1d");
    }

    #[test]
    fn disambiguate_unique_uses_basename() {
        let cwds = vec!["/a/b/ezbar".to_string(), "/c/weather".to_string()];
        assert_eq!(disambiguate_labels(&cwds), vec!["ezbar", "weather"]);
    }

    #[test]
    fn disambiguate_collision_adds_parent() {
        let cwds = vec![
            "/x/y/repo".to_string(),
            "/x/z/repo".to_string(),
            "/q/solo".to_string(),
        ];
        assert_eq!(disambiguate_labels(&cwds), vec!["y/repo", "z/repo", "solo"]);
    }

    #[test]
    fn project_to_full_fits_rising_trend() {
        // 10% → 15% over 100s ⇒ 0.05%/s ⇒ (100-15)/0.05 = 1700s to full.
        let s = vec![(0, 10.0), (50, 12.0), (100, 15.0)];
        assert_eq!(project_to_full(&s, 60, 0.5), Some(1700));
    }

    #[test]
    fn project_to_full_rejects_noise_and_drops() {
        assert_eq!(project_to_full(&[], 60, 0.5), None);
        assert_eq!(project_to_full(&[(0, 50.0)], 60, 0.5), None); // single sample, span 0
        assert_eq!(project_to_full(&[(0, 50.0), (30, 60.0)], 60, 0.5), None); // span < 60
        assert_eq!(project_to_full(&[(0, 50.0), (100, 50.2)], 60, 0.5), None); // rise < 0.5
        assert_eq!(project_to_full(&[(0, 50.0), (100, 40.0)], 60, 0.5), None); // window reset (drop)
    }

    #[test]
    fn has_active_descendant_walks_transitively() {
        // 1 → 2 → 3 ; 1 → 4. Only 3 is active.
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        children.insert(1, vec![2, 4]);
        children.insert(2, vec![3]);
        let active: HashSet<i32> = [3].into_iter().collect();
        assert!(has_active_descendant(1, &children, &active));
        assert!(has_active_descendant(2, &children, &active));
        assert!(!has_active_descendant(4, &children, &active)); // leaf, idle
    }

    #[test]
    fn has_active_descendant_idle_children_dont_count() {
        // A parent whose only children are idle (e.g. a sleeping MCP server) is NOT working.
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        children.insert(1, vec![2]);
        children.insert(2, vec![3]);
        let active: HashSet<i32> = HashSet::new();
        assert!(!has_active_descendant(1, &children, &active));
    }

    #[test]
    fn has_active_descendant_survives_a_cycle() {
        // Malformed map with a cycle 1↔2 must not loop forever.
        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        children.insert(1, vec![2]);
        children.insert(2, vec![1]);
        let active: HashSet<i32> = HashSet::new();
        assert!(!has_active_descendant(1, &children, &active));
    }
}
