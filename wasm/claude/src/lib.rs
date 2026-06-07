//! ezbar WASM plugin: `claude` — a Claude Code **agent dashboard**.
//!
//! The genuinely useful signal when you run several agents at once: **which one has been
//! idle the longest** (idle agents are usually waiting for you = wasted time), and **are you
//! near a rate limit**. The chip stays calm until an agent goes quiet, then it escalates with
//! idle time (amber → red). The popup is a btop-style panel: idle agents first (longest on
//! top, with how long), then the 5h block (cost / burn / projection / a spend-rate sparkline),
//! then limit bars with a "you'll hit the wall before it resets" projection.
//!
//! ## Detection (robust, race-free, zero-config)
//! `idle` is `now − mtime(newest transcript .jsonl)` under `~/.claude/projects/<cwd>/` — the
//! filesystem is the source of truth for "time since this agent last did anything", so there
//! is no in-memory dwell state to get wrong and nothing to install. A **worker-descendant**
//! check over `/proc` (any non-shell child = actively running a tool) suppresses the false
//! "waiting" when an agent is grinding a long build/render that simply isn't writing yet.
//! (The old `notify-send` sniff was a lost race — that process lives milliseconds.)
//!
//! Data: `/proc` (live `claude` procs + their cwd + process tree) and `~/.claude` (transcripts
//! + `ezbar-status.json` rate limits) over read-only **fs**; `bunx ccusage` over **exec**.
//!
//! ```toml
//! [modules.claude]
//! fs = [{ path = "/proc", at = "/proc", mode = "r" }, { path = "~/.claude", at = "/claude", mode = "r" }]
//! exec = ["bunx"]
//! ```

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// Pure, host-unit-tested logic (proc parsing, project-dir encoding, projection, JSON parsing, …)
// lives in a sibling crate so it can run under `cargo test` even though this plugin is wasm-only.
use claude_logic::{human_dur, idle_str, Block, Level, Limits};
use ezbar_plugin_wasm::prelude::*;

// Idle escalation (seconds since last transcript write, when not actively working):
const ATTN_SECS: i64 = 60; // quiet this long ⇒ "waiting for you" (amber)
const RED_SECS: i64 = 300; // quiet 5 min ⇒ red "go now"

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
    /// Seconds since this agent last wrote to its transcript.
    idle: i64,
    /// Quiet past the attention threshold and not actively running a tool ⇒ wants you.
    waiting: bool,
}

#[derive(Default)]
struct Claude {
    agents: Vec<Agent>,
    block: Option<Block>,
    limits: Option<Limits>,
    /// per-block spend-rate samples (Δcost between block polls) for the sparkline.
    burn_hist: Vec<f64>,
    prev_cost: Option<f64>,
    /// sparse `(epoch, five_hour_remaining%)` samples to project time-to-limit.
    limit_hist: Vec<(i64, f64)>,
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
    /// epoch of the last ccusage block read — paces it to ~60s without a tick counter.
    last_block: i64,
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
                let was_scanned = self.scanned;
                self.refresh_agents();
                self.limits = read_limits();
                self.sample_limit();

                // Trust the scan (paint the count) once we've found an agent, or after a warm-up
                // grace — never before. cap-std returns a PARTIAL `/proc` listing for the first
                // seconds of a cold start (missing the high-PID agents), so an early "0 agents"
                // is an artifact; until `scanned`, a quiet loading chip shows.
                if !self.agents.is_empty() || now - self.started >= 25 {
                    self.scanned = true;
                }

                // Read the ccusage block (popup-only) every ~60s — but ONLY once the chip is
                // already painting a populated frame (`was_scanned`, i.e. scanned on a prior
                // tick). It's a blocking `exec` that parks the fiber for seconds (cold `bunx` is
                // slow); running it before the chip has painted would freeze a half-loaded chip.
                if was_scanned && (self.last_block == 0 || now - self.last_block >= 60) {
                    self.last_block = now;
                    let block = read_block(ctx);
                    if let Some(b) = &block {
                        // sparkline plots the *spend rate* (Δcost), not cumulative cost.
                        if let Some(prev) = self.prev_cost {
                            self.burn_hist.push((b.cost - prev).max(0.0));
                            if self.burn_hist.len() > 60 {
                                self.burn_hist.remove(0);
                            }
                        }
                        self.prev_cost = Some(b.cost);
                    }
                    self.block = block;
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
        // the loud bit: agents waiting for you, with the worst idle — escalates amber → red.
        if let Some(d) = worst {
            let n = self.agents.iter().filter(|a| a.waiting).count();
            let c = idle_color(d);
            parts.push(Icon::Alert.view(13.0, c));
            parts.push(
                text(format!("{n}\u{00b7}{}", idle_str(d)))
                    .size(13.0)
                    .color(c),
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
        let waiting = self.agents.iter().filter(|a| a.waiting).count();
        let mut col: Vec<Render> = Vec::new();

        // ── header ──
        col.push(
            row([
                Icon::Bot.view(15.0, Token::Accent),
                text("Claude Code").size(15.0).color(Token::Fg),
                text(format!("\u{00b7} {total} running"))
                    .size(12.0)
                    .color(Token::FgDim),
            ])
            .spacing(8.0)
            .align(Align::Center),
        );
        if waiting > 0 {
            let c = self.worst_idle().map(idle_color).unwrap_or(Token::Warn);
            col.push(
                row([
                    Icon::Alert.view(13.0, c),
                    text(format!("{waiting} waiting for you"))
                        .size(13.0)
                        .color(c),
                ])
                .spacing(6.0)
                .align(Align::Center),
            );
        }

        // ── agents (longest-idle first) ──
        if total == 0 {
            col.push(text("no agents running").size(13.0).color(Token::FgDim));
        } else {
            for a in &self.agents {
                // One glyph for every row (shared advance ⇒ names align flush); colour carries
                // state: green = active, amber/red = idle-and-waiting by how long.
                let c = if a.waiting {
                    idle_color(a.idle)
                } else {
                    Token::Ok
                };
                let mut r = vec![
                    Icon::Dot.view(12.0, c),
                    text(a.label.clone()).size(13.0).color(Token::Fg),
                ];
                // Show the idle time when it's notable (waiting, or quiet ≥30s); a freshly
                // active agent stays clean (name + green dot). No extra spacer — the row's
                // 8px spacing already sets the gap, on-grid with every other tag in the panel.
                if a.waiting || a.idle >= 30 {
                    r.push(text(idle_str(a.idle)).size(11.0).color(if a.waiting {
                        c
                    } else {
                        Token::FgDim
                    }));
                }
                col.push(row(r).spacing(8.0).align(Align::Center));
            }
        }

        // ── 5-hour block ──
        if let Some(b) = &self.block {
            col.push(rule());
            col.push(text("5-hour block").size(12.0).color(Token::Accent));
            col.push(
                row([
                    text(format!("${:.2}", b.cost)).size(13.0).color(Token::Fg),
                    text(format!("${:.0}/hr", b.burn))
                        .size(13.0)
                        .color(Token::Fg),
                    text(format!("{} left", human_dur(b.mins_left * 60)))
                        .size(13.0)
                        .color(Token::FgDim),
                ])
                .spacing(10.0)
                .align(Align::Center),
            );
            col.push(
                text(format!(
                    "projected ${:.0} \u{00b7} {}",
                    b.projected,
                    short_model(&b.model)
                ))
                .size(12.0)
                .color(Token::FgDim),
            );
            if self.burn_hist.len() >= 3 {
                // line colour reads the *risk*, not the brand: green while healthy, amber/red as
                // the 5h limit tightens — so the sparkline says "you're burning toward the wall"
                // and never double-duties Accent (which is the header's job).
                let line = self
                    .limits
                    .as_ref()
                    .and_then(|l| l.five_used)
                    .map(usage_token)
                    .unwrap_or(Token::Ok);
                col.push(
                    Chart {
                        values: self.burn_hist.clone(),
                        line: line.into(),
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
            col.push(text("Limits").size(12.0).color(Token::Accent));
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
    /// Read live agents from `/proc`, derive idle time from each one's transcript, flag the
    /// quiet-and-not-working ones, and sort longest-idle first.
    fn refresh_agents(&mut self) {
        let now = now_secs();
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

        // Idle comes from transcript mtime, but a cwd's project dir is shared by every session
        // that ran there — so we can't map a pid to a file. Instead, per cwd, hand the freshest
        // mtimes to the *busy* agents (streaming or tool-running — they own the recent writes)
        // and the older ones to the rest. That way a busy sibling can't reset idle for a
        // genuinely-waiting one: the stale file lands on the idle agent and correctly flags it
        // (ai-agent-pro #2).
        let mut by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in raws.iter().enumerate() {
            by_cwd.entry(r.cwd.clone()).or_default().push(i);
        }
        let mut idle = vec![0i64; raws.len()];
        for (cwd, idxs) in &by_cwd {
            let mtimes = transcript_mtimes(cwd); // newest first
            let mut order = idxs.clone();
            order.sort_by_key(|&i| !raws[i].busy); // busy agents claim the fresh writes first
            for (k, &i) in order.iter().enumerate() {
                idle[i] = mtimes.get(k).map(|&m| (now - m).max(0)).unwrap_or(0);
            }
        }

        let mut agents: Vec<Agent> = raws
            .drain(..)
            .enumerate()
            .map(|(i, r)| Agent {
                label: String::new(), // filled by disambiguate_labels below
                // Detector gates on `working` ALONE — a parked agent (no tool child) flags after
                // ATTN_SECS even though its idle render loop nudges CPU. `busy` only steered the
                // mtime hand-out above.
                waiting: !r.working && idle[i] >= ATTN_SECS,
                idle: idle[i],
                cwd: r.cwd,
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
        agents.sort_by(|a, b| {
            b.waiting
                .cmp(&a.waiting)
                .then(b.idle.cmp(&a.idle))
                .then(a.label.cmp(&b.label))
        });
        self.agents = agents;
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

fn short_model(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_string()
}

// ── data (fs + exec) ─────────────────────────────────────────────────────────

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

/// True if `pid` has any descendant that is *actively working* (`active` set) — runnable/in-IO
/// Epoch mtimes of `cwd`'s transcript `.jsonl` files, **newest first** — one per session that
/// ran there. Claude stores transcripts under `~/.claude/projects/<encode_project(cwd)>`, mapped
/// here to `/claude/projects/…`. Empty when there's no transcript yet.
fn transcript_mtimes(cwd: &str) -> Vec<i64> {
    let dir = format!("/claude/projects/{}", claude_logic::encode_project(cwd));
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut mtimes: Vec<i64> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        })
        .collect();
    mtimes.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    mtimes
}

fn read_limits() -> Option<Limits> {
    let data = fs::read_to_string("/claude/ezbar-status.json").ok()?;
    claude_logic::parse_limits(&data, now_secs())
}

fn read_block(ctx: &mut dyn Ctx) -> Option<Block> {
    let o = ctx
        .exec("bunx", &["ccusage", "blocks", "--active", "--json"], None)
        .ok()?;
    if o.code != 0 {
        return None;
    }
    claude_logic::parse_block(&o.stdout)
}

export_plugin!(Claude);
