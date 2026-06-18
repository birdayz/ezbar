//! Pure logic for the ezbar `claude` plugin, factored out so it can be **unit-tested on the
//! host**. The plugin crate (`wasm/claude`) is `cdylib`-only and links a wasm-only SDK, so its
//! own functions can't run under `cargo test`; this crate has no I/O and no SDK, builds for both
//! the host and `wasm32-wasip2`, and the plugin depends on it for the fiddly, bug-prone bits —
//! the `/proc/<pid>/stat` parse, Claude's project-dir encoding, the rate-limit projection, etc.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// A session's two cumulative, monotonic "damage" counters from its per-session statusline
/// snapshot (`~/.claude/ezbar/sessions/<id>.json`): total spend, and total **API-active** time
/// (the seconds the model was actually working — Claude Code's own measurement). Both are
/// computed for us, so the bar needs no token-pricing math and no external tool.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `cost.total_cost_usd` — cumulative $ spent.
    pub cost: f64,
    /// `cost.total_api_duration_ms / 1000` — cumulative seconds the model spent in API calls.
    /// This is the meter's "active combat time": wall-clock idle (the agent parked waiting for
    /// you) does not advance it, so it's the honest denominator for a burn *rate*.
    pub api_secs: f64,
    /// `session_name` — the title Claude Code shows for the session (what the user actually named
    /// the work, e.g. "Convert calendar to WASM"). Empty when the session hasn't been named.
    pub name: String,
}

/// Parse a session snapshot into its [`Session`] counters. `None` only when the cost figure is
/// absent/malformed; a missing active-time defaults to `0` (a brand-new session that hasn't made
/// an API call yet is valid — its DPS just stays 0 until it works).
pub fn parse_session(json: &str) -> Option<Session> {
    let v: Value = serde_json::from_str(json).ok()?;
    let cost = v["cost"]["total_cost_usd"].as_f64()?;
    let api_secs = v["cost"]["total_api_duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
    let name = v["session_name"].as_str().unwrap_or("").trim().to_string();
    Some(Session {
        cost,
        api_secs,
        name,
    })
}

/// The **anchor** sample for a session: `(epoch, cumulative_cost, cumulative_api_secs)` captured the
/// first time ezbar saw it. Every later rate is derived as a delta from this — so the meter measures
/// what it has *observed since it started*, like Recount records from the moment you open it.
pub type Damage = (i64, f64, f64);

/// Pull `(message.id, output_tokens)` out of one transcript `.jsonl` line, or `None` if it isn't an
/// assistant turn carrying usage. The session JSON has no cumulative token counter, so output
/// throughput is summed from the transcript (RFC: dedup by `message.id` — Claude writes each
/// assistant message 3–4× identically). Only call this on a line the caller has size-bounded; a
/// multi-MB tool-result line is not an assistant-usage line and is skipped before it reaches here.
pub fn parse_assistant_out(line: &str) -> Option<(String, u64)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str()? != "assistant" {
        return None;
    }
    let id = v["message"]["id"].as_str()?.to_string();
    let out = v["message"]["usage"]["output_tokens"].as_u64()?;
    Some((id, out))
}

/// Cumulative output tokens from a transcript stream, deduping Claude's consecutive re-writes of
/// the same assistant message (same `message.id`). Each message is counted **once, at its final
/// `output_tokens`** — the re-writes are sometimes streaming partials (`out=2` … then `out=3760`),
/// so we keep the latest value for the in-flight id and only fold it into the total when the id
/// changes. Fed line-by-line and **resumable across ticks**: persisting `(last_id, last_tokens)`
/// means a message whose duplicate lines straddle an incremental-read boundary is still counted
/// once, at its final value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenCounter {
    /// `message.id` of the in-flight (not-yet-folded) message.
    last_id: String,
    /// Its latest `output_tokens` — updated on every re-write, folded into `finalized` when the id
    /// changes (so a streaming message is counted at its final, largest value).
    last_tokens: u64,
    /// Sum of all messages whose id has since changed (i.e. finalized).
    finalized: u64,
}

impl TokenCounter {
    /// Feed one transcript line. On a new `message.id`, the previous message is finalized; the
    /// current id's `output_tokens` is always updated to the latest line's value.
    pub fn push_line(&mut self, line: &str) {
        if let Some((id, out)) = parse_assistant_out(line) {
            if id != self.last_id {
                self.finalized += self.last_tokens;
                self.last_id = id;
            }
            self.last_tokens = out;
        }
    }

    /// Cumulative output tokens (finalized messages + the in-flight one at its latest value).
    pub fn total(&self) -> u64 {
        self.finalized + self.last_tokens
    }
}

/// Output **tokens per second of active model time** since an anchor — the generation-rate sibling
/// of [`dps`]. `Δtokens / Δactive`; `0.0` until there's a sliver of active time.
pub fn tps(anchor_tokens: u64, anchor_active: f64, tokens: u64, active: f64) -> f64 {
    let dtok = tokens.saturating_sub(anchor_tokens) as f64;
    let dactive = active - anchor_active;
    if dactive <= 0.0 {
        return 0.0;
    }
    dtok / dactive
}

/// **DPS** in $/hr: cost spent **per hour of active model time**, averaged from the `anchor` (the
/// session's first sample when ezbar started watching) to the latest `(cost, api_secs)`. This is
/// the *overall* average since the meter started — `Δcost / Δactive_time` over the whole watched
/// span, exactly like Recount's overall segment (total damage ÷ time-in-combat), NOT a recent
/// window. Active-time normalisation is the whole point: wall-clock idle (waiting on you) neither
/// inflates nor dilutes it, and the number is steady — it converges as the session runs rather than
/// twitching on the last turn. `0` until there's a measurable sliver of active time since the
/// anchor; cost only rises, so any non-increase clamps to 0 rather than going negative.
pub fn dps(anchor: Damage, cost: f64, api_secs: f64) -> f64 {
    let (_, c0, a0) = anchor;
    let dcost = (cost - c0).max(0.0);
    let dactive = api_secs - a0;
    if dactive <= 0.0 {
        return 0.0;
    }
    (dcost / dactive) * 3600.0
}

/// Account rate-limit usage, parsed from Claude Code's statusline JSON. `*_used` is percent
/// consumed (0..100); `*_reset_in` is seconds until the window resets.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub five_used: Option<f64>,
    pub five_reset_in: i64,
    pub week_used: Option<f64>,
    pub week_reset_in: i64,
}

/// Parse the `rate_limits` out of the statusline JSON (`~/.claude/ezbar-status.json`). `now` is
/// the current epoch, used to turn each window's absolute `resets_at` into a countdown. `None`
/// when the JSON is malformed or carries no `rate_limits` block.
pub fn parse_limits(json: &str, now: i64) -> Option<Limits> {
    let v: Value = serde_json::from_str(json).ok()?;
    let rl = &v["rate_limits"];
    if rl.is_null() {
        return None;
    }
    let reset_in = |k: &str| rl[k]["resets_at"].as_i64().map(|t| t - now).unwrap_or(0);
    Some(Limits {
        five_used: rl["five_hour"]["used_percentage"].as_f64(),
        five_reset_in: reset_in("five_hour"),
        week_used: rl["seven_day"]["used_percentage"].as_f64(),
        week_reset_in: reset_in("seven_day"),
    })
}

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
    fn token_counter_dedups_consecutive_message_rewrites() {
        let l = |id: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","usage":{{"output_tokens":{out}}}}}}}"#
            )
        };
        let mut c = TokenCounter::default();
        // msg A written 3× identically → counted once; B once; a non-assistant line ignored.
        c.push_line(&l("msgA", 576));
        c.push_line(&l("msgA", 576));
        c.push_line(&l("msgA", 576));
        c.push_line(r#"{"type":"user","message":{"content":"hi"}}"#);
        c.push_line(&l("msgB", 100));
        c.push_line(&l("msgB", 100));
        assert_eq!(c.total(), 676);
        // resumable: a dup of the last id straddling a tick boundary must not recount.
        c.push_line(&l("msgB", 100));
        assert_eq!(c.total(), 676);
    }

    #[test]
    fn token_counter_takes_final_value_of_a_streamed_message() {
        let l = |id: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","usage":{{"output_tokens":{out}}}}}}}"#
            )
        };
        let mut c = TokenCounter::default();
        // a streamed message: tiny partials first, then the final count — count the final, not 2.
        c.push_line(&l("m", 2));
        c.push_line(&l("m", 2));
        c.push_line(&l("m", 3760));
        assert_eq!(c.total(), 3760);
        // two distinct messages with the SAME token value both count (id, not value, dedups).
        c.push_line(&l("n", 3760));
        assert_eq!(c.total(), 7520);
    }

    #[test]
    fn parse_assistant_out_only_assistant_usage() {
        assert_eq!(
            parse_assistant_out(
                r#"{"type":"assistant","message":{"id":"m1","usage":{"output_tokens":42}}}"#
            ),
            Some(("m1".to_string(), 42))
        );
        assert_eq!(parse_assistant_out(r#"{"type":"user","message":{}}"#), None);
        assert_eq!(
            parse_assistant_out(r#"{"type":"assistant","message":{"id":"m2"}}"#),
            None // no usage
        );
        assert_eq!(parse_assistant_out("not json"), None);
    }

    #[test]
    fn tps_is_tokens_per_active_second() {
        assert_eq!(tps(0, 0.0, 600, 10.0), 60.0); // 600 tok / 10s
        assert_eq!(tps(100, 5.0, 100, 5.0), 0.0); // no active delta
        assert_eq!(tps(50, 0.0, 40, 10.0), 0.0); // tokens below anchor clamps to 0
    }

    #[test]
    fn parse_session_reads_name_and_trims() {
        let s = parse_session(
            r#"{"cost":{"total_cost_usd":1.0},"session_name":"  Refactor the bar  "}"#,
        )
        .unwrap();
        assert_eq!(s.name, "Refactor the bar"); // trimmed
                                                // missing session_name → empty (falls back to the cwd label in the plugin)
        assert_eq!(
            parse_session(r#"{"cost":{"total_cost_usd":1.0}}"#)
                .unwrap()
                .name,
            ""
        );
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
    fn parse_limits_from_statusline_json() {
        // shape mirrors ~/.claude/ezbar-status.json (Claude Code's statusline payload).
        let json = r#"{"cost":{"total_cost_usd":1.0},"rate_limits":{
            "five_hour":{"used_percentage":58.0,"resets_at":1000},
            "seven_day":{"used_percentage":24.0,"resets_at":2000}}}"#;
        let l = parse_limits(json, 100).unwrap();
        assert_eq!(l.five_used, Some(58.0));
        assert_eq!(l.five_reset_in, 900); // resets_at 1000 − now 100
        assert_eq!(l.week_used, Some(24.0));
        assert_eq!(l.week_reset_in, 1900);
    }

    #[test]
    fn parse_limits_rejects_missing_or_bad() {
        assert!(parse_limits(r#"{"cost":{}}"#, 0).is_none()); // no rate_limits
        assert!(parse_limits("not json", 0).is_none());
        // present but reset times absent → countdown defaults to 0, not a panic.
        let l = parse_limits(
            r#"{"rate_limits":{"five_hour":{"used_percentage":10.0}}}"#,
            50,
        )
        .unwrap();
        assert_eq!(l.five_used, Some(10.0));
        assert_eq!(l.five_reset_in, 0);
        assert_eq!(l.week_used, None);
    }

    #[test]
    fn parse_session_reads_cost_and_active_time() {
        let json = r#"{"session_id":"abc","cost":{"total_cost_usd":52.26,"total_api_duration_ms":120000},"model":{"id":"claude-opus-4-8"}}"#;
        let s = parse_session(json).unwrap();
        assert_eq!(s.cost, 52.26);
        assert_eq!(s.api_secs, 120.0); // 120000 ms
        assert_eq!(parse_session(r#"{"cost":{}}"#), None); // no cost figure
        assert_eq!(parse_session("not json"), None);
        // cost present, active-time absent ⇒ valid session, api_secs 0 (DPS just stays 0).
        let s2 = parse_session(r#"{"cost":{"total_cost_usd":1.0}}"#).unwrap();
        assert_eq!(s2.api_secs, 0.0);
    }

    #[test]
    fn dps_is_overall_cost_per_active_hour_since_anchor() {
        // anchor at session start ($0, 0s active): $6 across 360s active ⇒ $60 per active hour.
        assert!((dps((0, 0.0, 0.0), 6.0, 360.0) - 60.0).abs() < 1e-6);
        // anchored mid-session ($2 over 120s already): delta $4 over 240s active ⇒ $60/active-hr.
        assert!((dps((100, 2.0, 120.0), 6.0, 360.0) - 60.0).abs() < 1e-6);
        // no active time accrued since the anchor (idle, or just started) ⇒ 0, not a divide-by-zero.
        assert_eq!(dps((0, 5.0, 100.0), 5.0, 100.0), 0.0);
        // cost can't sit below the anchor (cost only rises); a drop clamps to 0, never negative.
        assert_eq!(dps((0, 9.0, 10.0), 5.0, 20.0), 0.0);
        // it's the OVERALL average, not a window — an old anchor keeps the whole span averaged:
        // anchor $0/0s; now $100 over 1000s active ⇒ $360/active-hr, however long ago that started.
        assert!((dps((0, 0.0, 0.0), 100.0, 1000.0) - 360.0).abs() < 1e-6);
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
