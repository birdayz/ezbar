//! Pure logic for the ezbar `agents` module's **Codex CLI** support, mirroring
//! `crates/claude-logic`'s role for Claude Code: the fiddly, bug-prone JSON parsing, isolated so
//! it unit-tests on the host with no filesystem access.
//!
//! Codex needs no statusline-wrapper hack the way Claude does — a Codex rollout (session log,
//! `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`) already carries its own cumulative token usage
//! and account-wide rate-limit snapshot as periodic `token_count` events, so the same file the
//! module tails for the transcript *is* the usage source. There is no dollar cost anywhere in it:
//! under ChatGPT-plan auth (this module's only supported mode so far) Codex tracks quota
//! percentage, not USD, so callers must not invent one.

use serde_json::Value;

/// Cumulative token usage + context-fullness from a rollout's latest `token_count` event.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    /// `total_token_usage.input_tokens` — cumulative prompt tokens (already inclusive of cached
    /// ones; unlike Claude's usage object, Codex's cache count is a *subset* of this, not extra).
    pub in_tokens: u64,
    /// `total_token_usage.output_tokens` (reasoning tokens are a subset of this, not extra).
    pub out_tokens: u64,
    /// Context-window fullness 0..100, Claude's `context_window.used_percentage` analogue. Codex
    /// reports no such figure directly, so it's derived from the **last** (not cumulative) turn's
    /// input size over the model's context window — a cumulative total would only grow and badly
    /// overstate fullness once compaction has pruned the live context. `None` without both figures.
    pub context_pct: Option<f64>,
}

/// One account-wide rate-limit window (Codex's own "primary"/"secondary" terminology) — the
/// analogue of `claude_logic::Limits`'s five-hour/seven-day pair, generalized because a Codex
/// plan's window sizes vary by plan rather than being fixed at 5h/7d.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    /// A short human label derived from the window size (`"5h"`, `"7d"`, …) via [`window_label`].
    pub label: String,
    /// Percent of the window consumed, 0..100.
    pub used: f64,
    /// Seconds until this window resets.
    pub reset_in: i64,
}

/// The rate-limit windows from a `token_count` event's `rate_limits` block. Either slot may be
/// absent — this account's plan, for instance, only ever populates `primary`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
}

/// A parsed `token_count` event: the cumulative usage it carries, plus the rate-limit snapshot
/// Codex attaches to the very same event (when present — some early-session ticks omit it).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenCountEvent {
    pub usage: Usage,
    pub limits: Option<Limits>,
}

/// Human label for a rate-limit window size in minutes: the two shapes Codex is known to use
/// (`300` → `"5h"`, `10080` → `"7d"`) get the same short form the Claude limit pills use; anything
/// else falls back to a generic day/hour/minute rendering so an unfamiliar plan still reads fine.
pub fn window_label(minutes: i64) -> String {
    if minutes <= 0 {
        return "0m".to_string();
    }
    if minutes % (24 * 60) == 0 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn parse_window(v: &Value, now: i64) -> Option<LimitWindow> {
    let used = v["used_percent"].as_f64()?;
    let minutes = v["window_minutes"].as_i64().unwrap_or(0);
    let reset_in = v["resets_at"].as_i64().map(|t| t - now).unwrap_or(0);
    Some(LimitWindow {
        label: window_label(minutes),
        used,
        reset_in,
    })
}

fn parse_limits_value(rl: &Value, now: i64) -> Option<Limits> {
    if rl.is_null() {
        return None;
    }
    let primary = parse_window(&rl["primary"], now);
    let secondary = parse_window(&rl["secondary"], now);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(Limits { primary, secondary })
}

/// Parse one rollout `.jsonl` line as a `token_count` event, or `None` if it's any other event
/// type (the vast majority of lines — `response_item`, `reasoning`, `function_call`, …). `now` is
/// the current epoch, used to turn `resets_at` into a countdown (mirrors
/// `claude_logic::parse_limits`).
pub fn parse_token_count(line: &str, now: i64) -> Option<TokenCountEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str()? != "event_msg" {
        return None;
    }
    let payload = &v["payload"];
    if payload["type"].as_str()? != "token_count" {
        return None;
    }
    let info = &payload["info"];
    let total = &info["total_token_usage"];
    let in_tokens = total["input_tokens"].as_u64().unwrap_or(0);
    let out_tokens = total["output_tokens"].as_u64().unwrap_or(0);
    let context_window = info["model_context_window"].as_u64();
    let last_in = info["last_token_usage"]["input_tokens"].as_u64();
    let context_pct = match (last_in, context_window) {
        (Some(li), Some(cw)) if cw > 0 => Some((li as f64 / cw as f64 * 100.0).min(100.0)),
        _ => None,
    };
    Some(TokenCountEvent {
        usage: Usage {
            in_tokens,
            out_tokens,
            context_pct,
        },
        limits: parse_limits_value(&payload["rate_limits"], now),
    })
}

/// The `cwd` a rollout was started in, from its first line (a `session_meta` event) — Codex's
/// analogue of Claude's encoded project directory, except Codex just stores the raw path, no
/// encoding to reverse. `None` for any other line (a caller should only ever pass line 1).
pub fn session_cwd(first_line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(first_line).ok()?;
    if v["type"].as_str()? != "session_meta" {
        return None;
    }
    v["payload"]["cwd"].as_str().map(str::to_string)
}

/// A rollout's best available display label, built up incrementally as its lines are tailed (see
/// [`parse_goal`] / [`parse_user_message`]) — Codex's analogue of Claude's `session_name`. Codex
/// has no single field for this; it's assembled from whichever of two sources actually fired,
/// since neither is guaranteed present (a short/casual session may set no goal at all).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLabel {
    /// The latest `thread_goal_updated` event's objective, if the agent ever set one — an
    /// explicit, distilled task description, so it wins over the raw first message when present.
    pub goal: Option<String>,
    /// The FIRST `user_message` text seen, if no goal has (yet) been set — a reasonable proxy for
    /// "what's this session about" (mirrors how chat UIs commonly title a thread from message 1).
    pub first_message: Option<String>,
}

impl SessionLabel {
    /// The best label to show, or `None` if neither source has fired yet — the caller then falls
    /// back to a cwd-based label, exactly as Claude does when `session_name` is empty.
    pub fn best(&self) -> Option<&str> {
        self.goal.as_deref().or(self.first_message.as_deref())
    }
}

/// Parse one rollout line as a `thread_goal_updated` event, returning its objective text (empty
/// after trimming is treated as absent, same as Claude's `parse_session` for `session_name`).
pub fn parse_goal(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str()? != "event_msg" {
        return None;
    }
    let payload = &v["payload"];
    if payload["type"].as_str()? != "thread_goal_updated" {
        return None;
    }
    let objective = payload["goal"]["objective"].as_str()?.trim();
    (!objective.is_empty()).then(|| objective.to_string())
}

/// Parse one rollout line as a `user_message` event, returning its text.
pub fn parse_user_message(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"].as_str()? != "event_msg" {
        return None;
    }
    let payload = &v["payload"];
    if payload["type"].as_str()? != "user_message" {
        return None;
    }
    let msg = payload["message"].as_str()?.trim();
    (!msg.is_empty()).then(|| msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // SANITIZED fixture — mirrors the real shape of a Codex rollout `token_count` event
    // (`~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`); figures are fabricated.
    fn token_count_line(in_tok: u64, out_tok: u64, last_in: u64, ctx_window: u64) -> String {
        format!(
            r#"{{"timestamp":"2026-07-19T08:18:06.233Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{in_tok},"cached_input_tokens":0,"output_tokens":{out_tok},"reasoning_output_tokens":10,"total_tokens":{},"cached_input_tokens_detail":null}},"last_token_usage":{{"input_tokens":{last_in},"cached_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":1,"total_tokens":{}}},"model_context_window":{ctx_window}}},"rate_limits":{{"limit_id":"codex","limit_name":null,"primary":{{"used_percent":34.0,"window_minutes":10080,"resets_at":1784949883}},"secondary":null,"credits":{{"has_credits":false,"unlimited":false,"balance":"0"}},"individual_limit":null,"plan_type":"prolite","rate_limit_reached_type":null}}}}}}"#,
            in_tok + out_tok,
            last_in + 5
        )
    }

    #[test]
    fn parses_cumulative_usage_and_context_pct_from_last_turn() {
        let line = token_count_line(13205, 275, 6460, 25840);
        let e = parse_token_count(&line, 100).unwrap();
        assert_eq!(e.usage.in_tokens, 13205);
        assert_eq!(e.usage.out_tokens, 275);
        // context_pct comes from the LAST turn's input, not the cumulative total (which only
        // grows and would badly overstate fullness after compaction).
        assert_eq!(e.usage.context_pct, Some(25.0));
    }

    #[test]
    fn parses_rate_limits_alongside_usage() {
        let line = token_count_line(100, 10, 50, 1000);
        let e = parse_token_count(&line, 1784949783).unwrap();
        let l = e.limits.unwrap();
        let p = l.primary.unwrap();
        assert_eq!(p.label, "7d");
        assert_eq!(p.used, 34.0);
        assert_eq!(p.reset_in, 100); // resets_at 1784949883 - now 1784949783
        assert_eq!(l.secondary, None);
    }

    #[test]
    fn non_token_count_lines_are_ignored() {
        assert_eq!(
            parse_token_count(r#"{"type":"response_item","payload":{}}"#, 0),
            None
        );
        assert_eq!(
            parse_token_count(
                r#"{"type":"event_msg","payload":{"type":"task_started"}}"#,
                0
            ),
            None
        );
        assert_eq!(parse_token_count("not json", 0), None);
    }

    #[test]
    fn missing_total_token_usage_defaults_to_zero_not_a_panic() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#;
        let e = parse_token_count(line, 0).unwrap();
        assert_eq!(e.usage.in_tokens, 0);
        assert_eq!(e.usage.out_tokens, 0);
        assert_eq!(e.usage.context_pct, None);
        assert_eq!(e.limits, None);
    }

    #[test]
    fn window_label_maps_known_and_generic_sizes() {
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(10080), "7d");
        assert_eq!(window_label(120), "2h");
        assert_eq!(window_label(2880), "2d");
        assert_eq!(window_label(90), "90m");
        assert_eq!(window_label(0), "0m");
    }

    #[test]
    fn session_cwd_reads_the_session_meta_line() {
        let line = r#"{"timestamp":"2026-07-17T15:22:50.195Z","type":"session_meta","payload":{"session_id":"019f70ac-55cb-7273-b04e-41e9a9a31b81","cwd":"/home/user/projects/demo","originator":"codex_exec"}}"#;
        assert_eq!(
            session_cwd(line).as_deref(),
            Some("/home/user/projects/demo")
        );
        assert_eq!(session_cwd(r#"{"type":"event_msg"}"#), None);
        assert_eq!(session_cwd("not json"), None);
    }

    // SANITIZED fixture — mirrors the real shape of a `thread_goal_updated` event.
    #[test]
    fn parse_goal_reads_the_objective() {
        let line = r#"{"timestamp":"2026-07-19T10:52:25.222Z","type":"event_msg","payload":{"type":"thread_goal_updated","threadId":"x","goal":{"threadId":"x","objective":"  keep working on the short until 10/10  ","status":"active","tokensUsed":0,"timeUsedSeconds":0,"createdAt":1,"updatedAt":1}}}"#;
        assert_eq!(
            parse_goal(line).as_deref(),
            Some("keep working on the short until 10/10") // trimmed
        );
        assert_eq!(
            parse_goal(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#),
            None
        );
        assert_eq!(parse_goal("not json"), None);
    }

    #[test]
    fn parse_goal_rejects_empty_objective() {
        let line = r#"{"type":"event_msg","payload":{"type":"thread_goal_updated","goal":{"objective":"   "}}}"#;
        assert_eq!(parse_goal(line), None);
    }

    // SANITIZED fixture — mirrors the real shape of a `user_message` event.
    #[test]
    fn parse_user_message_reads_the_text() {
        let line = r#"{"timestamp":"2026-07-19T12:24:56.683Z","type":"event_msg","payload":{"type":"user_message","message":"fix the login bug","images":[],"local_images":[],"text_elements":[]}}"#;
        assert_eq!(
            parse_user_message(line).as_deref(),
            Some("fix the login bug")
        );
        assert_eq!(
            parse_user_message(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#),
            None
        );
        assert_eq!(parse_user_message("not json"), None);
    }

    #[test]
    fn session_label_prefers_goal_over_first_message() {
        let mut l = SessionLabel::default();
        assert_eq!(l.best(), None);
        l.first_message = Some("herllo".to_string()); // a real casual first message, typo and all
        assert_eq!(l.best(), Some("herllo"));
        l.goal = Some("Ship the Codex dock integration".to_string());
        assert_eq!(l.best(), Some("Ship the Codex dock integration"));
    }
}
