//! ezbar WASM plugin: `claude` — a Claude Code **agent dashboard + spend meter**.
//!
//! Running several agents at once, two things matter: **which one is waiting on you** (an idle
//! agent is wasted time) and **how fast you're burning money**. The chip shows the live count,
//! the worst idle (escalating amber → red), the combined **$/hr** (the "raid DPS"), and the
//! 5h/7d rate-limit usage. The popup is a **Recount-style meter**: one bar per agent, biggest
//! spender on top, plus the account $/hr trend and the limit bars.
//!
//! ## Detection (robust, race-free, zero-config)
//! `idle` is `now − mtime(newest transcript .jsonl)` under `~/.claude/projects/<cwd>/` — the
//! filesystem is the source of truth for "time since this agent last did anything", so there
//! is no in-memory dwell state to get wrong and nothing to install. A **worker-descendant**
//! check over `/proc` (any non-shell child = actively running a tool) suppresses the false
//! "waiting" when an agent is grinding a long build/render that simply isn't writing yet.
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
//! Data: `/proc` + `~/.claude` (transcripts, per-session cost, `ezbar-status.json` rate limits)
//! over read-only **fs**. No `exec`.
//!
//! ```toml
//! [modules.claude]
//! fs = [{ path = "/proc", at = "/proc", mode = "r" }, { path = "~/.claude", at = "/claude", mode = "r" }]
//! ```

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// Pure, host-unit-tested logic (proc parsing, project-dir encoding, projection, JSON parsing, …)
// lives in a sibling crate so it can run under `cargo test` even though this plugin is wasm-only.
use claude_logic::{human_dur, idle_str, Damage, Level, Limits};
use ezbar_plugin_wasm::prelude::*;

// Idle escalation (seconds since last transcript write, when not actively working):
const ATTN_SECS: i64 = 60; // quiet this long ⇒ "waiting for you" (amber)
const RED_SECS: i64 = 300; // quiet 5 min ⇒ red "go now"

// Past this, an agent isn't "waiting for you" — it's abandoned. A session left open overnight
// shouldn't pin the always-on chip red for a day and desensitise you to real alerts, so beyond the
// cutoff it drops out of the waiting alarm and reads as a dim "parked" row instead.
const STALE_SECS: i64 = 4 * 3600; // 4h quiet ⇒ parked, not waiting

// A process counts as "doing work" only above this CPU rate (clock ticks/sec; ticks are USER_HZ
// = 100/s regardless of kernel HZ). Measured: an *idle* Node/Ink process still drips ~1 tick/s
// of render-loop bookkeeping, and a heartbeating MCP/LSP server a few more — while a process
// doing real work pulls 30–40 ticks/s. 10/s cuts cleanly between them, so neither an idle agent
// nor an idle resident child gets mistaken for active.
const CPU_BUSY_TPS: i64 = 10;

struct Agent {
    /// Full cwd path, so colliding basenames can be disambiguated.
    cwd: String,
    /// Display label (basename, or parent/basename when basenames collide).
    label: String,
    /// This agent's session id (its newest transcript's filename) — keys the cost lookup.
    session: String,
    /// Seconds since this agent last wrote to its transcript.
    idle: i64,
    /// Quiet past the attention threshold and not actively running a tool ⇒ wants you.
    waiting: bool,
    /// Cumulative session cost in $ (Claude Code's own figure, via the statusline snapshot) — this
    /// agent's total "damage done".
    cost: f64,
    /// Cumulative seconds of API-active model time — the DPS denominator (active combat time).
    api_secs: f64,
    /// Spend rate in $/hr — this agent's "DPS", `Δcost / Δactive` averaged since its anchor.
    dps: f64,
}

#[derive(Default)]
struct Claude {
    agents: Vec<Agent>,
    limits: Option<Limits>,
    /// sparse `(epoch, five_hour_remaining%)` samples to project time-to-limit.
    limit_hist: Vec<(i64, f64)>,
    /// per-session **anchor** — the first `(epoch, cost, active_secs)` ezbar saw for each session.
    /// DPS is the average from here to now, so the meter measures everything since it started (like
    /// Recount's overall segment), not a recent window. O(1) per session — no growing time series.
    anchors: HashMap<String, Damage>,
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
        let worst = self.worst_idle();
        // The Bot stays calm — Accent when agents run, dim when none. Escalation is carried
        // ONLY by the ⚠ + idle text below, so a single agent going quiet can't double-paint
        // the whole chip amber (one signal, one colour).
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
        let total_dps: f64 = self.live_dps();
        if total > 0 {
            parts.push(
                text(format!("${total_dps:.0}/hr"))
                    .size(13.0)
                    .color(if total_dps >= 1.0 {
                        Token::Accent
                    } else {
                        Token::FgDim
                    }),
            );
        }
        // the loud bit: agents waiting for you, with the worst idle — escalates amber → red.
        if let Some(d) = worst {
            let n = self.agents.iter().filter(|a| a.waiting).count();
            let c = idle_color(d);
            parts.push(Icon::Alert.view(13.0, c));
            // lead with the worst idle (the part that makes you act); prefix the count only when
            // more than one waits. The middot is spaced so "3 · 25h" can't read as decimal "3.25h".
            let txt = if n == 1 {
                idle_str(d)
            } else {
                format!("{n} \u{00b7} {}", idle_str(d))
            };
            parts.push(text(txt).size(13.0).color(c));
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
        let total_dps: f64 = self.live_dps();
        let total_cost: f64 = self.agents.iter().map(|a| a.cost).sum();
        let mut col: Vec<Render> = Vec::new();

        // ── header: title + agent count + the raid DPS (combined live $/hr) ──
        // The count is the TOTAL — it always equals the chip's count, so the two surfaces can't
        // contradict each other. Who's working / waiting / parked is carried by the per-row dot
        // colours and idle times (and the chip's ⚠ alarm), not by a separate summary line.
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
                // dot state: green working, amber/red waiting-on-you, dim when parked (gone stale).
                let dot_c = if a.waiting {
                    idle_color(a.idle)
                } else if a.idle >= STALE_SECS {
                    Token::FgDim
                } else {
                    Token::Ok
                };
                // Each row shows THIS agent's own overall average $/hr (its DPS since ezbar
                // anchored it) — like Recount, where a player's overall DPS stays on their row even
                // after they stop. Accent when it's still burning now, dim once it's idle/parked
                // (the number persists, the colour drops). Every cell is the same `$N/hr` shape, so
                // the column still aligns; the Accent rows are exactly the ones summed into the
                // header (`live_dps`), so the bright values still foot to the headline.
                let live = burning(a);
                let rate = format!("${:.0}/hr", a.dps);
                let total = format!("${:.0}", a.cost);
                let mut r = vec![
                    Icon::Dot.view(12.0, dot_c),
                    meter_bar(a.cost, max_cost),
                    text(pad_num(&rate, 7)).size(13.0).color(if live {
                        Token::Accent
                    } else {
                        Token::FgDim
                    }),
                    text(pad_num(&total, 6)).size(11.0).color(Token::FgDim),
                    text(a.label.clone()).size(13.0).color(Token::Fg),
                ];
                // a waiting agent also shows how long it's been quiet, in its escalation colour.
                if a.waiting {
                    r.push(text(idle_str(a.idle)).size(11.0).color(dot_c));
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
                label: String::new(), // filled by disambiguate_labels below
                // Detector gates on `working` ALONE — a parked agent (no tool child) flags after
                // ATTN_SECS even though its idle render loop nudges CPU. Above STALE_SECS it's no
                // longer "waiting for you" (abandoned), so it leaves the alarm and reads as parked.
                waiting: !r.working && (ATTN_SECS..STALE_SECS).contains(&idle[i]),
                idle: idle[i],
                session: std::mem::take(&mut session[i]),
                cwd: r.cwd,
                cost: 0.0,
                api_secs: 0.0,
                dps: 0.0,
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

        // DPS per agent: read this session's cumulative `(cost, active_secs)` from its statusline
        // snapshot, anchor it the first time we see it, and take the average from the anchor to now
        // — Δcost/Δactive over the whole watched span (Recount's overall segment). Nothing
        // pre-divided, and just one stored sample per session.
        for a in &mut agents {
            if let Some(s) = read_session(&a.session) {
                a.cost = s.cost;
                a.api_secs = s.api_secs;
                let anchor = *self
                    .anchors
                    .entry(a.session.clone())
                    .or_insert((now, s.cost, s.api_secs));
                a.dps = claude_logic::dps(anchor, s.cost, s.api_secs);
            }
        }
        // Drop anchors for sessions no longer live so the map can't grow unbounded.
        let live: std::collections::HashSet<&str> =
            agents.iter().map(|a| a.session.as_str()).collect();
        self.anchors.retain(|sid, _| live.contains(sid.as_str()));

        // Recount order: biggest spender on top. We rank by *total* spend ("damage done"), not
        // by $/hr — all-opus agents burn at nearly the same per-active-hour rate (the rate is
        // model-bound), so a $/hr ranking would be a near-flat, uninformative meter, while total
        // spend spreads wide and gives the bars real hierarchy. Exactly Recount's default mode.
        // Tiebreak by $/hr, then name, for a stable order.
        agents.sort_by(|a, b| {
            cmp_desc(a.cost, b.cost)
                .then(cmp_desc(a.dps, b.dps))
                .then(a.label.cmp(&b.label))
        });
        self.agents = agents;
    }

    /// Combined **live** $/hr — the team's "raid DPS" — counting only agents that are burning
    /// right now (the Accent rows). A waiting/parked agent's overall average still shows on its own
    /// row, but it isn't money leaving the account this second, so it doesn't inflate the headline.
    /// The bright rows therefore sum to this. One coherent number for the chip, header, sparkline.
    fn live_dps(&self) -> f64 {
        self.agents
            .iter()
            .filter(|a| burning(a))
            .map(|a| a.dps)
            .sum()
    }

    fn worst_idle(&self) -> Option<i64> {
        self.agents
            .iter()
            .filter(|a| a.waiting)
            .map(|a| a.idle)
            .max()
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

/// Is this agent **burning right now** — actively spending, not waiting on you or parked? Its
/// overall-average $/hr then counts toward the live headline and paints Accent. Idle agents still
/// show their average on their own row, just dim and out of the live total.
fn burning(a: &Agent) -> bool {
    a.dps >= 1.0 && !a.waiting && a.idle < STALE_SECS
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

/// Colour for an idle agent by how long it's been quiet: amber, then red past `RED_SECS`.
fn idle_color(secs: i64) -> Token {
    token(claude_logic::idle_level(secs, RED_SECS))
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
/// same-uid process, which the launching shell sets to the cwd. `?` only if neither resolves.
fn proc_cwd(pid: i32) -> String {
    if let Ok(target) = fs::read_link(format!("/proc/{pid}/cwd")) {
        return target.to_string_lossy().to_string();
    }
    if let Ok(env) = fs::read(format!("/proc/{pid}/environ")) {
        if let Some(pwd) = claude_logic::pwd_from_environ(&env) {
            return pwd;
        }
    }
    "?".into()
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
    out.sort_unstable_by(|a, b| b.1.cmp(&a.1)); // newest first
    out
}

fn read_limits() -> Option<Limits> {
    let data = fs::read_to_string("/claude/ezbar-status.json").ok()?;
    claude_logic::parse_limits(&data, now_secs())
}

/// This session's cumulative damage counters (`cost`, `api_secs`), from its per-session statusline
/// snapshot (`~/.claude/ezbar/sessions/<id>.json`) written by the ezbar statusline wrapper. No
/// external tool — Claude Code already computed the figures; we just read them.
fn read_session(session_id: &str) -> Option<claude_logic::Session> {
    let data = fs::read_to_string(format!("/claude/ezbar/sessions/{session_id}.json")).ok()?;
    claude_logic::parse_session(&data)
}

export_plugin!(Claude);
