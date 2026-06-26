//! ezbar WASM plugin: `claude` — a Claude Code **agent dashboard + spend meter**.
//!
//! Running several agents at once, what matters is **how fast you're burning money**. The chip
//! shows the live agent count, the combined **$/hr** (the "raid DPS"), the combined **tokens/s**,
//! and the 5h/7d rate-limit usage. The popup is a **Recount-style meter**: one bar per agent
//! (titled by its session name), biggest spender on top, each row showing its own $/hr and tok/s,
//! plus the account $/hr trend, the limit bars, and a clickable **`All | Today | 1h`** selector at
//! the bottom that re-windows every rate. (There is deliberately **no idle/"waiting for you"
//! alarm** — a session waiting on you isn't actionable from the bar.)
//!
//! ## Detection (robust, race-free, zero-config)
//! `idle` is `now − mtime(newest transcript .jsonl)` under `~/.claude/projects/<cwd>/` — the
//! filesystem is the source of truth for "time since this agent last did anything", so there
//! is no in-memory dwell state to get wrong and nothing to install. A **worker-descendant**
//! check over `/proc` (any non-shell child = actively running a tool) keeps an agent's dot green
//! while it grinds a long build/render that simply isn't writing the transcript yet.
//!
//! ## DPS (a real damage meter — no external tool, no `exec`)
//! Per agent we sample two cumulative counters from Claude Code's own per-session statusline
//! snapshot (`~/.claude/ezbar/sessions/<id>.json`, written by the ezbar wrapper): `total_cost_usd`
//! (the "damage") and `total_api_duration_ms` (the seconds the model was actually working). The
//! first time ezbar sees a session it **anchors** that pair; DPS is then `Δcost / Δactive` from the
//! anchor to now — the *overall* average since the meter started, exactly like Recount's overall
//! segment (total damage ÷ time-in-combat), not a recent window. Cost per hour of *active* model
//! time, so wall-clock idle (an agent parked waiting for you) neither inflates nor dilutes it, and
//! the number is steady rather than twitching on the last turn. The combined $/hr is one coherent
//! number everywhere — the chip, the popup header, the sum of the rows, and the trend sparkline all
//! show it. No `ccusage`, no token-pricing math — just `fs`.
//!
//! ## Windowed rates (All / Today / 1h)
//! The session snapshot has no token counter, so **tokens/s** is summed from the transcript files
//! (main + `subagents/*.jsonl`) — read incrementally, deduped by `message.id`, bounded so a
//! multi-MB tool-result line can't OOM the sandbox. Both $/hr and tok/s are windowable: the meter
//! keeps a per-session **all-time anchor** plus a coarse 24h **sample ring**, and the popup's
//! `All | Today | 1h` selector chooses which baseline the rates measure from.
//!
//! Data: `/proc` + `~/.claude` (transcripts, per-session cost, `ezbar-status.json` rate limits)
//! over read-only **fs**. No `exec`.
//!
//! ```toml
//! [modules.claude]
//! fs = [{ path = "/proc", at = "/proc", mode = "r" }, { path = "~/.claude", at = "/claude", mode = "r" }]
//! max_memory = "8M"  # headroom for chrono-tz (Today) + the 24h sample rings; a resource knob, set by hand
//! ```

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// Pure, host-unit-tested logic (proc parsing, project-dir encoding, projection, JSON parsing, …)
// lives in a sibling crate so it can run under `cargo test` even though this plugin is wasm-only.
use chrono_tz::{Tz, UTC};
use claude_logic::{human_dur, idle_str, Level, Limits, Sample, TokenCounter};
use ezbar_plugin_wasm::prelude::*;

// Quieter than this (and not actively working) ⇒ the popup row's dot dims and shows "Nm" idle
// info. Purely informational — no alarm.
const ATTN_SECS: i64 = 60;

// Past this an agent is abandoned (a session left open overnight), so it drops out of the live
// $/hr headline (`burning`) and reads as a dim parked row rather than inflating the combined rate.
const STALE_SECS: i64 = 4 * 3600;

// A process counts as "doing work" only above this CPU rate (clock ticks/sec; ticks are USER_HZ
// = 100/s regardless of kernel HZ). Measured: an *idle* Node/Ink process still drips ~1 tick/s
// of render-loop bookkeeping, and a heartbeating MCP/LSP server a few more — while a process
// doing real work pulls 30–40 ticks/s. 10/s cuts cleanly between them, so neither an idle agent
// nor an idle resident child gets mistaken for active.
const CPU_BUSY_TPS: i64 = 10;

// Sample ring for the windowed rates: one `(epoch, cost, api_secs, out_tokens)` per session at
// ≈1-minute resolution, kept for the last day. The all-time baseline lives separately (`anchors`),
// so the ring can be pruned freely — "All" still measures since ezbar started watching.
const SAMPLE_SECS: i64 = 60;
const RING_SECS: i64 = 24 * 3600;

/// The rate window the popup selector chooses: the whole watched span, since local midnight, or the
/// last hour. Changes which baseline `windowed_rate` measures from — and thus the headline numbers.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Window {
    #[default]
    All,
    Today,
    Hour,
}

impl Window {
    /// Epoch the window starts at (`i64::MIN` for All ⇒ the anchor baseline).
    fn start(self, now: i64, tz: Tz) -> i64 {
        match self {
            Window::All => i64::MIN,
            Window::Hour => now - 3600,
            Window::Today => local_midnight(now, tz),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Window::All => "All",
            Window::Today => "Today",
            Window::Hour => "1h",
        }
    }
    /// The mouse_area id for this window's selector chip.
    fn id(self) -> &'static str {
        match self {
            Window::All => "win-all",
            Window::Today => "win-today",
            Window::Hour => "win-1h",
        }
    }
}

/// Epoch of the most recent local midnight in `tz` — the start of "Today". Falls back to `now` if
/// the conversion can't be resolved (a degenerate DST gap), which just makes "Today" empty.
fn local_midnight(now: i64, tz: Tz) -> i64 {
    use chrono::TimeZone;
    let Some(utc) = chrono::Utc.timestamp_opt(now, 0).single() else {
        return now;
    };
    let local_date = utc.with_timezone(&tz).date_naive();
    let Some(midnight) = local_date.and_hms_opt(0, 0, 0) else {
        return now;
    };
    tz.from_local_datetime(&midnight)
        .single()
        .map(|m| m.timestamp())
        .unwrap_or(now)
}

struct Agent {
    /// Full cwd path, so colliding basenames can be disambiguated.
    cwd: String,
    /// Display label (basename, or parent/basename when basenames collide).
    label: String,
    /// This agent's session id (its newest transcript's filename) — keys the cost lookup.
    session: String,
    /// Seconds since this agent last wrote to its transcript (shown as dim "Nm" info, not an alarm).
    idle: i64,
    /// Actively running a tool right now (a non-shell worker descendant in `/proc`) — the accurate
    /// "working" signal, true even during a long build/render that isn't writing the transcript yet.
    working: bool,
    /// Cumulative session cost in $ (Claude Code's own figure, via the statusline snapshot) — this
    /// agent's total "damage done".
    cost: f64,
    /// Cumulative seconds of API-active model time — the DPS denominator (active combat time).
    api_secs: f64,
    /// Spend rate in $/hr — this agent's "DPS", `Δcost / Δactive` averaged since its anchor.
    dps: f64,
    /// Cumulative output tokens, summed from the session's transcript(s) — see `token_files`.
    out_tokens: u64,
    /// Output generation rate (tokens per active second) since its anchor — the `$/hr` sibling.
    tps: f64,
}

#[derive(Default)]
struct Claude {
    agents: Vec<Agent>,
    limits: Option<Limits>,
    /// sparse `(epoch, five_hour_remaining%)` samples to project time-to-limit.
    limit_hist: Vec<(i64, f64)>,
    /// per-session **all-time anchor** — the first `(cost, api_secs, out_tokens)` ezbar saw. The
    /// baseline for the "All" window, so it measures everything since the meter started watching
    /// (like Recount's overall segment). O(1) per session.
    anchors: HashMap<String, (f64, f64, u64)>,
    /// per-session **sample ring** — `(epoch, cost, api_secs, out_tokens)` at ≈1-min resolution for
    /// the last 24h, the baselines for the "Today"/"1h" windows. Bounded + pruned to live sessions.
    samples: HashMap<String, Vec<Sample>>,
    /// Which window the popup selector has chosen — drives the displayed $/hr & tok/s.
    window: Window,
    /// Local timezone (RFC 0019), loaded once from the host; only "Today" needs it.
    tz: Option<Tz>,
    /// Per-transcript-file incremental token state: `path → (byte offset already read, counter)`.
    /// Transcripts are append-only, so each tick reads only the bytes past `offset`. Covers a
    /// session's main `.jsonl` and every `subagents/*.jsonl`. Pruned to files seen this tick.
    token_files: HashMap<String, (u64, TokenCounter)>,
    /// account $/hr over time — the meter's trend sparkline ("raid DPS").
    dps_hist: Vec<f64>,
    /// `pid → cpu ticks` from the last poll, to tell a *busy* child from an idle resident one.
    prev_cpu: HashMap<i32, u64>,
    /// epoch of the last poll, so the CPU delta is normalised to a rate (jitter-proof).
    prev_poll: i64,
    /// one-tick hysteresis: true after a single zero-agent reading, so a lone transient empty
    /// is absorbed rather than blinked through to the popup.
    saw_empty: bool,
    /// false until we've found an agent or the warm-up grace has passed — so a cold start (where
    /// cap-std hands back a PARTIAL `/proc` listing for the first seconds, missing the high-PID
    /// agents) renders a quiet loading chip instead of a misleading "0 agents".
    scanned: bool,
    /// epoch of the first tick — anchors the warm-up grace.
    started: i64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Plugin for Claude {
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            Event::Timer => {
                let now = now_secs();
                if self.started == 0 {
                    self.started = now;
                }
                if self.tz.is_none() {
                    self.tz = Some(ctx.local_timezone().parse().unwrap_or(UTC));
                }
                self.refresh_agents(now);
                self.limits = read_limits();
                self.sample_limit();

                // Trust the scan (paint the count) once we've found an agent, or after a warm-up
                // grace — never before. cap-std returns a PARTIAL `/proc` listing for the first
                // seconds of a cold start (missing the high-PID agents), so an early "0 agents"
                // is an artifact; until `scanned`, a quiet loading chip shows.
                if !self.agents.is_empty() || now - self.started >= 25 {
                    self.scanned = true;
                }

                // Trend of the account's combined live $/hr (the "raid DPS") for the meter
                // sparkline — the SAME quantity the chip and popup header show (`live_dps`),
                // sampled each settled tick so the graph, the header number, and the sum of the
                // Accent rows are one coherent metric rather than subtly different windows.
                if self.scanned {
                    self.dps_hist.push(self.live_dps());
                    if self.dps_hist.len() > 60 {
                        self.dps_hist.remove(0);
                    }
                }

                // Poll FAST until the scan settles (warms the cold cap-std `/proc` enumeration up
                // sooner so the agents appear in a second or two, not ~15), then the calm cadence.
                ctx.set_timeout(if self.scanned { 3000 } else { 500 });
                true
            }
            // Click the All / Today / 1h selector at the popup bottom → re-window the rates.
            Event::Pointer {
                id,
                kind: PointerKind::Press,
                ..
            } => {
                let w = match id.as_str() {
                    "win-all" => Some(Window::All),
                    "win-today" => Some(Window::Today),
                    "win-1h" => Some(Window::Hour),
                    _ => None,
                };
                match w {
                    Some(w) => {
                        if w != self.window {
                            self.window = w;
                            // The trend sparkline plots the windowed live $/hr; drop its history so
                            // it doesn't blend the old window's points with the new one's.
                            self.dps_hist.clear();
                            self.recompute_rates(now_secs());
                        }
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn view(&self) -> Render {
        // Cold start: we haven't scanned `/proc` yet, so show a quiet loading chip rather than a
        // misleading "0 agents" before the first reading lands.
        if !self.scanned {
            return row([
                Icon::Bot.view(13.0, Token::FgDim),
                text("\u{2026}").size(13.0).color(Token::FgDim),
            ])
            .spacing(5.0)
            .align(Align::Center);
        }
        let total = self.agents.len();
        // The Bot is Accent when agents run, dim when none. (Idle agents are no longer alarmed —
        // a session waiting on you isn't actionable from the bar, so there's no ⚠ escalation.)
        let bot = if total > 0 {
            Token::Accent
        } else {
            Token::FgDim
        };
        let mut parts = vec![
            Icon::Bot.view(13.0, bot),
            text(format!("{total}")).size(13.0).color(if total > 0 {
                Token::Fg
            } else {
                Token::FgDim
            }),
        ];
        // The live spend rate — the whole team's combined $/hr (the "raid DPS"). It's the meter's
        // headline, so it's ALWAYS on the bar whenever agents exist: Accent while money is moving,
        // dim "$0/hr" when the raid's gone quiet. (A DPS meter shows the DPS even when it's zero —
        // a vanished number reads as "broken", a $0 reads as "idle".)
        // `.max(0.0)` normalises a float -0.0 / sub-dollar negative so it never prints "$-0/hr".
        let total_dps: f64 = self.live_dps().max(0.0);
        if total > 0 {
            let dps_color = if total_dps >= 1.0 {
                Token::Accent
            } else {
                Token::FgDim
            };
            parts.push(text(format!("${total_dps:.0}/hr")).size(13.0).color(dps_color));
            // Inline sparkline of the combined $/hr — the SAME `dps_hist` the popup charts, so the
            // bar carries the team's spend *trend* at a glance (the cpu/temperature-style chip
            // graph), not just the instantaneous number. `Generic` auto-fits the y-range to the
            // data (DPS is on no fixed scale), host-sized to a 48x16 chip sparkline; our line
            // colour wins (Generic has no threshold palette). Needs >=2 points to draw a segment.
            if self.dps_hist.len() >= 2 {
                parts.push(
                    Graph {
                        values: self.dps_hist.clone(),
                        kind: GraphKind::Generic,
                        line: dps_color.into(),
                    }
                    .view(),
                );
            }
            // combined output throughput — the team's generation rate alongside the spend rate.
            let total_tps = self.live_tps();
            parts.push(
                text(fmt_tps(total_tps))
                    .size(13.0)
                    .color(if total_tps >= 1.0 {
                        Token::Accent
                    } else {
                        Token::FgDim
                    }),
            );
        }
        // 5h then 7d limit *used* — labelled so a glance isn't a guess. Each label+value is
        // bound tighter (3px) than the chip's 5px inter-element gap, so the two read as two
        // pairs, not four loose tokens. Colour ramps green→amber→red as usage climbs.
        let limit_pill = |label: &str, used: f64| {
            row([
                text(label.to_string()).size(11.0).color(Token::FgDim),
                text(format!("{used:.0}%"))
                    .size(13.0)
                    .color(usage_token(used)),
            ])
            .spacing(3.0)
            .align(Align::Center)
        };
        if let Some(l) = self.limits.as_ref() {
            // a wider gap fences the account rate-limit cluster off from the agent/DPS cluster, so
            // the chip reads as two groups rather than a run of equal-spaced tokens.
            if l.five_used.is_some() || l.week_used.is_some() {
                parts.push(spacer(7.0));
            }
            if let Some(p) = l.five_used {
                parts.push(limit_pill("5h", p));
            }
            if let Some(p) = l.week_used {
                parts.push(limit_pill("7d", p));
            }
        }
        row(parts).spacing(5.0).align(Align::Center)
    }

    fn popup(&self) -> Option<Render> {
        // Cold start: don't claim "no agents running" before the first scan — show a header that
        // reads as loading. (Keeps the popup non-interactive, so it stays hover-driven.)
        if !self.scanned {
            return Some(
                row([
                    Icon::Bot.view(15.0, Token::Accent),
                    text("Claude Code").size(15.0).color(Token::Fg),
                    text("\u{2026}").size(15.0).color(Token::FgDim),
                ])
                .spacing(8.0)
                .align(Align::Center),
            );
        }
        let total = self.agents.len();
        let total_dps: f64 = self.live_dps().max(0.0);
        let total_cost: f64 = self.agents.iter().map(|a| a.cost).sum();
        let mut col: Vec<Render> = Vec::new();

        // ── header: title + agent count + the raid DPS (combined live $/hr) ──
        // The count is the TOTAL — it always equals the chip's count, so the two surfaces can't
        // contradict each other. Who's working vs idle/parked is carried by the per-row dot
        // colour and the dim idle time, not by a separate summary line.
        let mut hdr = vec![
            Icon::Bot.view(15.0, Token::Accent),
            text("Claude Code").size(15.0).color(Token::Fg),
            text(format!(
                "\u{00b7} {total} agent{}",
                if total == 1 { "" } else { "s" }
            ))
            .size(12.0)
            .color(Token::FgDim),
        ];
        if total > 0 {
            hdr.push(
                text(format!("\u{00b7} ${total_dps:.0}/hr"))
                    .size(12.0)
                    .color(if total_dps >= 1.0 {
                        Token::Accent
                    } else {
                        Token::FgDim
                    }),
            );
        }
        col.push(row(hdr).spacing(8.0).align(Align::Center));

        // ── the meter: one bar per agent, biggest spender on top (Recount Damage-Done mode) ──
        if total == 0 {
            col.push(text("no agents running").size(13.0).color(Token::FgDim));
        } else {
            let max_cost = self.agents.iter().map(|a| a.cost).fold(0.0_f64, f64::max);
            // Recount's row is (total, DPS) with the bar ∝ total: the bar gives the at-a-glance
            // ranking, the printed $total the exact money (this is a *spend* meter — you want to
            // read the dollars), and the $/hr the live burn rate. $/hr is the one Accent, but only
            // when the agent is actually burning *now* — a parked agent's last rate stays dim.
            for a in &self.agents {
                // dot state: green while working (running a tool) or recently active, dim once
                // idle/parked. No amber/red "waiting on you" escalation — it wasn't actionable.
                let dot_c = if a.working || a.idle < ATTN_SECS {
                    Token::Ok
                } else {
                    Token::FgDim
                };
                // Each row shows THIS agent's own overall average $/hr (its DPS since ezbar
                // anchored it) — like Recount, where a player's overall DPS stays on their row even
                // after they stop. Accent when it's still burning now, dim once it's idle/parked
                // (the number persists, the colour drops). Every cell is the same `$N/hr` shape, so
                // the column still aligns; the Accent rows are exactly the ones summed into the
                // header (`live_dps`), so the bright values still foot to the headline.
                let live = burning(a);
                let rate = format!("${:.0}/hr", a.dps.max(0.0));
                let total = format!("${:.0}", a.cost);
                let tok = fmt_tps(a.tps);
                let mut r = vec![
                    Icon::Dot.view(12.0, dot_c),
                    meter_bar(a.cost, max_cost),
                    text(pad_num(&rate, 7)).size(13.0).color(if live {
                        Token::Accent
                    } else {
                        Token::FgDim
                    }),
                    // output throughput, dim (secondary to the spend rate but useful at a glance).
                    text(pad_num(&tok, 9)).size(11.0).color(Token::FgDim),
                    text(pad_num(&total, 6)).size(11.0).color(Token::FgDim),
                    text(a.label.clone()).size(13.0).color(Token::Fg),
                ];
                // a quiet, not-currently-working agent shows how long it's been idle — dim info,
                // not an alarm (a working agent mid-build isn't "idle" even if it hasn't written).
                if !a.working && a.idle >= ATTN_SECS {
                    r.push(text(idle_str(a.idle)).size(11.0).color(Token::FgDim));
                }
                col.push(row(r).spacing(8.0).align(Align::Center));
            }
        }

        // ── spend: total across the running agents, + the account $/hr trend ──
        if total > 0 {
            col.push(rule());
            col.push(
                row([
                    text(format!("${total_cost:.0}"))
                        .size(13.0)
                        .color(Token::Fg),
                    text("total spent").size(12.0).color(Token::FgDim),
                ])
                .spacing(6.0)
                .align(Align::Center),
            );
            if self.dps_hist.len() >= 3 {
                col.push(
                    Chart {
                        values: self.dps_hist.clone(),
                        line: Token::Accent.into(),
                        width: 248.0,
                        height: 34.0,
                    }
                    .view(),
                );
            }
        }

        // ── limits ──
        if let Some(l) = &self.limits {
            col.push(rule());
            col.push(text("Limits").size(12.0).color(Token::FgDim));
            if let Some(p) = l.five_used {
                col.push(limit_row("5h", p, l.five_reset_in));
            }
            if let Some(p) = l.week_used {
                col.push(limit_row("7d", p, l.week_reset_in));
            }
            // The genuinely useful headline: if you'll exhaust the 5h limit *before* it resets,
            // say when — that's the wall you actually hit. Only shown when it binds.
            if let (Some(eta), Some(u5)) = (self.project_five(), l.five_used) {
                if u5 > 20.0 && eta < l.five_reset_in {
                    col.push(
                        row([
                            Icon::Alert.view(12.0, Token::Urgent),
                            text(format!(
                                "5h limit in ~{} \u{00b7} resets {}",
                                human_dur(eta),
                                human_dur(l.five_reset_in)
                            ))
                            .size(12.0)
                            .color(Token::Urgent),
                        ])
                        .spacing(6.0)
                        .align(Align::Center),
                    );
                }
            }
        }

        // Window selector — click to re-window every rate ($/hr & tok/s, chip + rows): All|Today|1h.
        // The chosen one is Accent; the rest dim. Clicks reach `update` as `Pointer{Press}` by id.
        col.push(rule());
        let sel = |w: Window| {
            let on = self.window == w;
            mouse_area(
                w.id(),
                text(w.label())
                    .size(12.0)
                    .color(if on { Token::Accent } else { Token::FgDim }),
            )
        };
        col.push(
            row([sel(Window::All), sel(Window::Today), sel(Window::Hour)])
                .spacing(16.0)
                .align(Align::Center),
        );

        Some(column(col).spacing(5.0))
    }
}

impl Claude {
    /// Read live agents from `/proc`, derive idle from each transcript, read each one's session
    /// cost + compute its live $/hr, and sort the meter biggest-spender first.
    fn refresh_agents(&mut self, now: i64) {
        let (procs, children) = read_procs();

        // Never publish an empty snapshot from a failed scan. `/proc` always holds hundreds of
        // system processes, so an *empty* read means `read_dir("/proc")` glitched this tick (a
        // transient in the cap-std sandbox) — not "you closed every agent". Keeping the last
        // good list instead of zeroing it kills the one-frame "no agents running" flash the
        // popup would otherwise flicker through every time a scan came back empty. A genuine
        // zero-agents state still renders, because the scan still returns the system procs.
        if procs.is_empty() {
            return;
        }

        // "active" = a process doing work right now: runnable/in-IO, or burning CPU *above the
        // idle-drip rate* since the last poll. The rate threshold is load-bearing: an idle
        // resident child (MCP/LSP server, even one that heartbeats) ticks only a little, so
        // mere "CPU rose at all" would wrongly pin its parent agent to "working" forever
        // (ai-agent-pro). Normalised by real elapsed time so a delayed poll can't inflate it.
        let elapsed = if self.prev_poll > 0 {
            (now - self.prev_poll).max(1)
        } else {
            1
        };
        self.prev_poll = now;
        let busy_delta = (CPU_BUSY_TPS * elapsed) as u64;
        let mut active = std::collections::HashSet::new();
        let mut next_cpu = HashMap::new();
        for p in &procs {
            next_cpu.insert(p.pid, p.cpu);
            let busy = matches!(p.state, 'R' | 'D')
                || self
                    .prev_cpu
                    .get(&p.pid)
                    .is_some_and(|&prev| p.cpu.saturating_sub(prev) >= busy_delta);
            if busy {
                active.insert(p.pid);
            }
        }
        self.prev_cpu = next_cpu;

        // Each live claude proc, with cwd and two distinct flags:
        //   • `working` — has an actively-running tool descendant. This (and only this) suppresses
        //     a false "waiting", because a long build/render can be quiet without needing you.
        //   • `busy` — working OR the claude process itself is CPU-active (streaming/thinking).
        //     Used ONLY to order the per-cwd mtime hand-out, never as the detector: a *parked*
        //     claude still drips ~1 tick/s of idle render-loop bookkeeping, so self-CPU can't tell
        //     idle from busy on its own — but it's a fine tie-breaker for "who owns the fresh write".
        struct Raw {
            cwd: String,
            working: bool,
            busy: bool,
        }
        let mut raws: Vec<Raw> = procs
            .iter()
            .filter(|p| p.comm == "claude")
            .map(|p| {
                // `working` = an actively-running tool descendant; only that suppresses a false
                // "waiting" (an idle resident MCP/LSP child must NOT pin the agent to working).
                let working = claude_logic::has_active_descendant(p.pid, &children, &active);
                Raw {
                    cwd: proc_cwd(p.pid),
                    working,
                    busy: working || active.contains(&p.pid),
                }
            })
            .collect();

        // Idle + the session id come from the transcript files. A cwd's project dir is shared by
        // every session that ran there, so per cwd hand the freshest transcripts to the *busy*
        // agents (they own the recent writes) and the older to the rest — a busy sibling then
        // can't reset a genuinely-waiting one's idle, and each agent gets its own session id
        // (the transcript filename) for the cost lookup (ai-agent-pro #2).
        let mut by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in raws.iter().enumerate() {
            by_cwd.entry(r.cwd.clone()).or_default().push(i);
        }
        let mut idle = vec![0i64; raws.len()];
        let mut session = vec![String::new(); raws.len()];
        for (cwd, idxs) in &by_cwd {
            let sessions = transcript_sessions(cwd); // (session_id, mtime), newest first
            let mut order = idxs.clone();
            order.sort_by_key(|&i| !raws[i].busy); // busy agents claim the fresh writes first
            for (k, &i) in order.iter().enumerate() {
                if let Some((sid, m)) = sessions.get(k) {
                    idle[i] = (now - m).max(0);
                    session[i] = sid.clone();
                }
            }
        }

        let mut agents: Vec<Agent> = raws
            .drain(..)
            .enumerate()
            .map(|(i, r)| Agent {
                label: String::new(), // filled by disambiguate_labels / session name below
                idle: idle[i],
                working: r.working,
                session: std::mem::take(&mut session[i]),
                cwd: r.cwd,
                cost: 0.0,
                api_secs: 0.0,
                dps: 0.0,
                out_tokens: 0,
                tps: 0.0,
            })
            .collect();

        // Invariant: don't flash a populated list straight to empty on a single tick. Even with
        // the system-procs guard above, absorb ONE zero-agent reading before committing it — any
        // transient that yields "0 claude" (a partial scan, a stat read losing the race with a
        // process table mutation) is swallowed instead of blinking "no agents running". A real
        // "you closed every agent" still lands on the next tick (≤3s later).
        if agents.is_empty() && !self.agents.is_empty() && !self.saw_empty {
            self.saw_empty = true;
            return;
        }
        self.saw_empty = agents.is_empty();

        // `parent/base` labels where basenames collide (worktrees, two terminals in one repo).
        let cwds: Vec<String> = agents.iter().map(|a| a.cwd.clone()).collect();
        for (a, label) in agents
            .iter_mut()
            .zip(claude_logic::disambiguate_labels(&cwds))
        {
            a.label = label;
        }

        // Per agent: read this session's cumulative `(cost, active_secs)` from its statusline
        // snapshot. The rate ($/hr) is derived later from the sample ring + window, not here.
        for a in &mut agents {
            if let Some(s) = read_session(&a.session) {
                a.cost = s.cost;
                a.api_secs = s.api_secs;
                // Prefer the session's own title (what the user named the work) over the cwd
                // basename; fall back to the disambiguated cwd label when the session is unnamed.
                // Titles run long (40–60 chars), so clip to keep the popup row from blowing out.
                if !s.name.is_empty() {
                    a.label = clip(&s.name, 32);
                }
            }
        }
        // Tokens: sum each session's cumulative output tokens from its transcript(s) — the main
        // `.jsonl` plus every `subagents/*.jsonl` — read INCREMENTALLY (append-only ⇒ only the new
        // bytes each tick), deduped by message.id, then derive tok/s like the DPS anchor. A line
        // larger than the cap (a multi-MB tool result, never assistant usage) is skipped unparsed,
        // so this stays inside the sandbox.
        let mut live_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        for a in &mut agents {
            if a.session.is_empty() {
                continue;
            }
            let mut sum = 0u64;
            for path in transcript_files(&a.cwd, &a.session) {
                // First sight of a file: tail from its current EOF rather than summing history —
                // a session's transcript can be 100s of MB, and reading it all in one tick would
                // risk the WALL backstop. tok/s is "since ezbar started watching" (like the DPS
                // anchor), so only tokens generated from now on matter.
                let is_new = !self.token_files.contains_key(&path);
                let entry = self.token_files.entry(path.clone()).or_default();
                if is_new {
                    entry.0 = file_len(&path);
                }
                read_new_lines(&path, &mut entry.0, &mut entry.1);
                sum += entry.1.total();
                live_files.insert(path);
            }
            a.out_tokens = sum;
        }

        // Anchor each session's first-seen counters (the all-time "All" baseline) and append a
        // coarse timestamped sample (≈1-min resolution, last 24h) — these feed the windowed rates.
        for a in &agents {
            if a.session.is_empty() {
                continue;
            }
            self.anchors
                .entry(a.session.clone())
                .or_insert((a.cost, a.api_secs, a.out_tokens));
            let ring = self.samples.entry(a.session.clone()).or_default();
            if ring.last().is_none_or(|s| now - s.0 >= SAMPLE_SECS) {
                ring.push((now, a.cost, a.api_secs, a.out_tokens));
                let cutoff = now - RING_SECS;
                if let Some(pos) = ring.iter().position(|s| s.0 >= cutoff) {
                    if pos > 0 {
                        ring.drain(0..pos);
                    }
                }
            }
        }

        // Drop per-session state (anchor, ring) + transcript files no longer live.
        let live: std::collections::HashSet<&str> =
            agents.iter().map(|a| a.session.as_str()).collect();
        self.anchors.retain(|sid, _| live.contains(sid.as_str()));
        self.samples.retain(|sid, _| live.contains(sid.as_str()));
        self.token_files.retain(|p, _| live_files.contains(p));

        self.agents = agents;
        // Windowed $/hr & tok/s for the current window (also recomputed on a selector click).
        self.recompute_rates(now);

        // Recount order: biggest spender on top. We rank by *total* spend ("damage done"), not by
        // $/hr — all-opus agents burn at nearly the same per-active-hour rate (model-bound), so a
        // $/hr ranking would be a near-flat, uninformative meter, while total spend spreads wide
        // and gives the bars real hierarchy. Tiebreak by the windowed $/hr, then name, for stability.
        self.agents.sort_by(|a, b| {
            cmp_desc(a.cost, b.cost)
                .then(cmp_desc(a.dps, b.dps))
                .then(a.label.cmp(&b.label))
        });
    }

    /// Recompute every agent's windowed $/hr & tok/s for the current `window`, from its sample ring
    /// and all-time anchor. Called each tick and whenever the popup window selector is clicked, so
    /// the chip and rows re-window instantly without waiting for the next poll.
    fn recompute_rates(&mut self, now: i64) {
        let win_start = self.window.start(now, self.tz.unwrap_or(UTC));
        let empty: &[Sample] = &[];
        let rates: Vec<(f64, f64)> = self
            .agents
            .iter()
            .map(|a| {
                let anchor = self
                    .anchors
                    .get(&a.session)
                    .copied()
                    .unwrap_or((a.cost, a.api_secs, a.out_tokens));
                let ring = self.samples.get(&a.session).map_or(empty, |v| v.as_slice());
                claude_logic::windowed_rate(
                    ring, anchor, win_start, a.cost, a.api_secs, a.out_tokens,
                )
            })
            .collect();
        for (a, (d, t)) in self.agents.iter_mut().zip(rates) {
            a.dps = d;
            a.tps = t;
        }
    }

    /// Combined **live** $/hr — the team's "raid DPS" — summing the agents that are spending (the
    /// Accent rows: a real $/hr and not abandoned). A long-parked session's overall average still
    /// shows on its own row but drops out of the headline. The bright rows sum to this — one
    /// coherent number for the chip, header, and sparkline.
    fn live_dps(&self) -> f64 {
        self.agents
            .iter()
            .filter(|a| burning(a))
            .map(|a| a.dps)
            .sum()
    }

    /// Combined output generation rate (tokens/s) — the same set of agents that feed `live_dps`.
    fn live_tps(&self) -> f64 {
        self.agents
            .iter()
            .filter(|a| burning(a))
            .map(|a| a.tps)
            .sum()
    }

    /// Record a sparse `five_hour_used%` sample (≥20s apart) so `project_five` can fit a fill
    /// rate. The limit only moves per-turn, so dense sampling would just be noise.
    fn sample_limit(&mut self) {
        let Some(p) = self.limits.as_ref().and_then(|l| l.five_used) else {
            return;
        };
        let now = now_secs();
        if self.limit_hist.last().is_none_or(|&(t, _)| now - t >= 20) {
            self.limit_hist.push((now, p));
            if self.limit_hist.len() > 24 {
                self.limit_hist.remove(0);
            }
        }
    }

    /// Project seconds until the 5h limit hits 100% used at the recent fill rate. `None` unless
    /// there's a ≥60s baseline with a meaningful, *rising* trend (so we never extrapolate noise).
    fn project_five(&self) -> Option<i64> {
        claude_logic::project_to_full(&self.limit_hist, 60, 0.5)
    }
}

/// Is this agent **spending** — a real $/hr and not abandoned (parked past `STALE_SECS`)? Its
/// overall-average $/hr then counts toward the live headline and paints Accent. A briefly-idle
/// agent (between turns) still counts, so the headline doesn't collapse to $0 whenever the fleet
/// is momentarily waiting; only a long-abandoned session drops out.
fn burning(a: &Agent) -> bool {
    a.dps >= 1.0 && a.idle < STALE_SECS
}

// ── rendering helpers ────────────────────────────────────────────────────────

/// Map a pure severity [`Level`] (from `claude-logic`) to a theme token.
fn token(l: Level) -> Token {
    match l {
        Level::Ok => Token::Ok,
        Level::Warn => Token::Warn,
        Level::Urgent => Token::Urgent,
    }
}

/// Colour for a limit by how much is *used*: green with headroom, amber past 70%, red past 85%.
fn usage_token(used: f64) -> Token {
    token(claude_logic::usage_level(used))
}

/// A two-tone box-drawing bar of `used` %, filling toward the cap, with the percent right-aligned
/// in a fixed cell (figure-spaces share a digit's advance) so the trailing reset column lines up.
fn limit_row(label: &str, used: f64, reset_in: i64) -> Render {
    const W: usize = 10;
    let filled = (((used / 100.0) * W as f64).round() as usize).min(W);
    let lvl = usage_token(used);
    let pct = format!("{used:.0}");
    let pad = "\u{2007}".repeat(3usize.saturating_sub(pct.len())); // figure space = digit width
    row([
        text(label).size(12.0).color(Token::FgDim),
        row([
            text("\u{2588}".repeat(filled)).size(13.0).color(lvl),
            text("\u{2591}".repeat(W - filled))
                .size(13.0)
                .color(Token::FgDim),
        ])
        .spacing(0.0),
        text(format!("{pad}{pct}%")).size(13.0).color(lvl),
        text(format!("\u{00b7} {}", human_dur(reset_in)))
            .size(11.0)
            .color(Token::FgDim),
    ])
    .spacing(8.0)
    .align(Align::Center)
}

/// A dim divider spanning the popup width (~248px at size 8).
fn rule() -> Render {
    text("\u{2500}".repeat(60)).size(8.0).color(Token::FgDim)
}

/// A fixed-width block-char meter bar (Recount-style): `value/max` of the cells filled, the rest
/// dim. Scaled by total spend, so the biggest spender's bar is full and the rest rank against it.
/// Filled cells are **Fg** (neutral structure), NOT Accent — Accent is reserved for the one live
/// signal, the $/hr; the bar encodes *how much* (cumulative, already spent), which is not "money
/// moving now". An all-dim bar (value 0) reads as "nothing spent yet".
fn meter_bar(value: f64, max: f64) -> Render {
    const W: usize = 10;
    // Any nonzero spender gets at least a one-cell sliver (a raider in the meter is never blank),
    // even when dwarfed by the top bar; only a true zero renders empty.
    let n = if max > 0.0 && value > 0.0 {
        (((value / max) * W as f64).round() as usize).clamp(1, W)
    } else {
        0
    };
    row([
        text("\u{2588}".repeat(n)).size(13.0).color(Token::Fg),
        text("\u{2591}".repeat(W - n))
            .size(13.0)
            .color(Token::FgDim),
    ])
    .spacing(0.0)
}

/// Descending `f64` comparison for the Recount sort (NaN sorts as equal, never panics).
fn cmp_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

/// Right-align a short numeric string in a fixed `width`-char cell with figure-spaces (U+2007
/// shares a digit's advance), so a stack of `$/hr` (or `$total`) values forms a clean right-edged
/// column even in the bar's proportional font — same trick the limit rows use for their percents.
fn pad_num(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(s.chars().count());
    format!("{}{}", "\u{2007}".repeat(pad), s)
}

/// Output throughput as `45 t/s` / `1.2k t/s` (tokens per active second).
fn fmt_tps(t: f64) -> String {
    if t >= 1000.0 {
        format!("{:.1}k t/s", t / 1000.0)
    } else {
        format!("{:.0} t/s", t)
    }
}

/// Clip a label to `max` chars with an ellipsis — session titles run long (40–60 chars) and would
/// otherwise blow out the popup row.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

// ── data (fs) ────────────────────────────────────────────────────────────────

struct Proc {
    pid: i32,
    comm: String,
    /// Scheduler state: `R`/`D` mean it's on/await-CPU (doing work) right now.
    state: char,
    /// `utime + stime` in clock ticks — diffed across polls to spot a *busy* child.
    cpu: u64,
}

/// One pass over `/proc`: every readable process and a `ppid → children` map, parsed from
/// `/proc/<pid>/stat`. Used to find `claude` procs and walk their descendants.
fn read_procs() -> (Vec<Proc>, HashMap<i32, Vec<i32>>) {
    let mut procs = Vec::new();
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return (procs, children);
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        let Some(st) = claude_logic::parse_stat(&stat) else {
            continue;
        };
        let pid: i32 = name.parse().unwrap_or(0);
        children.entry(st.ppid).or_default().push(pid);
        procs.push(Proc {
            pid,
            comm: st.comm,
            state: st.state,
            cpu: st.cpu,
        });
    }
    (procs, children)
}

/// A process's working directory. The kernel exposes it as the magic symlink `/proc/<pid>/cwd`,
/// but the cap-std sandbox refuses to `readlink` magic symlinks (they resolve to an absolute
/// path outside the preopen) — so that always fails here and every agent would label as `?`.
/// Fall back to the `PWD` env var from `/proc/<pid>/environ`, a plain readable file for a
/// same-uid process, which the launching shell sets to the cwd.
///
/// One twist: `claude --worktree <name>` chdir's into `<repo>/.claude/worktrees/<name>` while
/// `PWD` stays at the repo root — so the bare `PWD` would map a worktree agent to the *parent
/// repo's* transcripts and surface a wrong, older session (a worktree session rendered under an
/// unrelated session that had last run in the parent repo). The cmdline is plain-readable, so
/// recover the worktree name and rebuild the real cwd. `?` only if nothing resolves.
fn proc_cwd(pid: i32) -> String {
    if let Ok(target) = fs::read_link(format!("/proc/{pid}/cwd")) {
        return target.to_string_lossy().to_string();
    }
    let Ok(env) = fs::read(format!("/proc/{pid}/environ")) else {
        return "?".into();
    };
    let Some(pwd) = claude_logic::pwd_from_environ(&env) else {
        return "?".into();
    };
    if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
        if let Some(name) = claude_logic::worktree_from_cmdline(&cmdline) {
            return claude_logic::worktree_cwd(&pwd, &name);
        }
    }
    pwd
}

/// `(session_id, mtime)` for `cwd`'s transcript `.jsonl` files, **newest first** — one per
/// session that ran there. The session id is the filename stem (the session UUID), which is also
/// the key for the per-session cost snapshot. Claude stores transcripts under
/// `~/.claude/projects/<encode_project(cwd)>`, mapped here to `/claude/projects/…`.
fn transcript_sessions(cwd: &str) -> Vec<(String, i64)> {
    let dir = format!("/claude/projects/{}", claude_logic::encode_project(cwd));
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, i64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let sid = name.strip_suffix(".jsonl")?.to_string();
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)?;
            Some((sid, mtime))
        })
        .collect();
    out.sort_unstable_by_key(|&(_, mtime)| std::cmp::Reverse(mtime)); // newest first
    out
}

fn read_limits() -> Option<Limits> {
    let data = fs::read_to_string("/claude/ezbar-status.json").ok()?;
    claude_logic::parse_limits(&data, now_secs())
}

/// A session's transcript files: the main `…/<session>.jsonl` plus every `…/<session>/subagents/
/// *.jsonl` (Task-spawned agents live in their own file tree, not interleaved). All guest paths
/// under the `/claude` fs mount.
fn transcript_files(cwd: &str, session: &str) -> Vec<String> {
    let dir = format!("/claude/projects/{}", claude_logic::encode_project(cwd));
    let mut out = vec![format!("{dir}/{session}.jsonl")];
    let subdir = format!("{dir}/{session}/subagents");
    if let Ok(entries) = fs::read_dir(&subdir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".jsonl") {
                out.push(format!("{subdir}/{name}"));
            }
        }
    }
    out
}

/// Current byte length of a file (0 if unstattable) — used to tail a transcript from its EOF on
/// first sight rather than reading its (possibly enormous) history.
fn file_len(path: &str) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Per-line cap: buffer+parse transcript lines up to this size; a longer line is a tool-result
/// dump (hundreds of KB up to multi-MB; never assistant usage) and is scanned-and-skipped, never
/// buffered — so a giant line can't blow the 2 MiB sandbox (one capped line + its serde Value
/// stays well under). 256 KiB still covers any real assistant turn (the largest observed is ~93 KB
/// / ~28K output tokens).
const LINE_CAP: usize = 256 * 1024;
const READ_CHUNK: usize = 64 * 1024;

/// Read transcript lines appended past `*off`, feeding each (size-bounded) complete line to `tc`,
/// and advance `*off` to the last newline (a partial trailing line is re-read next tick). If the
/// file shrank (compaction/rotation), re-tail from its new EOF — we don't re-sum the rewritten
/// history (it's old turns), we just keep counting new generation forward.
fn read_new_lines(path: &str, off: &mut u64, tc: &mut TokenCounter) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = fs::File::open(path) else {
        return;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *off {
        *off = len; // shrank → re-tail from the new end; counter keeps its running total
    }
    if f.seek(SeekFrom::Start(*off)).is_err() {
        return;
    }
    let mut committed = *off;
    let mut pos = *off;
    let mut line: Vec<u8> = Vec::new();
    let mut over = false; // current line exceeded LINE_CAP → scan to its newline, don't buffer
    let mut buf = [0u8; READ_CHUNK];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for &b in &buf[..n] {
            pos += 1;
            if b == b'\n' {
                if !over {
                    if let Ok(s) = std::str::from_utf8(&line) {
                        tc.push_line(s);
                    }
                }
                line.clear();
                over = false;
                committed = pos;
            } else if !over {
                if line.len() >= LINE_CAP {
                    over = true;
                    line.clear();
                } else {
                    line.push(b);
                }
            }
        }
    }
    *off = committed;
}

/// This session's cumulative damage counters (`cost`, `api_secs`), from its per-session statusline
/// snapshot (`~/.claude/ezbar/sessions/<id>.json`) written by the ezbar statusline wrapper. No
/// external tool — Claude Code already computed the figures; we just read them.
fn read_session(session_id: &str) -> Option<claude_logic::Session> {
    let data = fs::read_to_string(format!("/claude/ezbar/sessions/{session_id}.json")).ok()?;
    claude_logic::parse_session(&data)
}

export_plugin!(Claude);
