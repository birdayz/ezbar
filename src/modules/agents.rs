//! `agents` module: a native Claude Code **agent dashboard + spend meter** (RFC 0022, phase 2).
//!
//! The in-process sibling of the `claude` WASM plugin: the same Recount-style meter (one bar per
//! running agent, biggest spender on top, combined live **$/hr** + **tokens/s** + rate-limit bars),
//! but as a native [`Module`] with real `/proc` access — no cap-std sandbox, so `proc_cwd` is a
//! plain `readlink` and the cold-start warm-up the WASM build needed is gone. All the pure logic
//! (proc/stat parsing, project-dir encoding, windowed rates, token counting, limit projection) is
//! reused **verbatim** from `claude-logic`; only the I/O shell differs.
//!
//! **Threading (the native split):** the heavy I/O — scanning `/proc`, reading each session's cost
//! snapshot, tailing transcripts — runs in the subscription on `spawn_blocking`, emitting a derived
//! [`Poll`]. The windowing state (per-session anchors + sample ring, the `All|Today|1h` selector)
//! lives in the module struct and is merged in `update`, which does only HashMap + arithmetic work
//! (never blocks) — so a selector click re-windows instantly without waiting for the next poll.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local, TimeZone};
use ezbar_plugin::iced::alignment::{Horizontal, Vertical};
use ezbar_plugin::iced::futures::{SinkExt, Stream, StreamExt};
use ezbar_plugin::iced::mouse::Interaction;
use ezbar_plugin::iced::widget::text::Wrapping;
use ezbar_plugin::iced::widget::{
    button, canvas, column, container, mouse_area, row, stack, text, Space,
};
use ezbar_plugin::iced::{Background, Border, Color, Element, Length, Subscription};
use ezbar_plugin::icons::Icon;
use ezbar_plugin::ui::graph::{Graph, GraphKind};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::sway;
use claude_logic::{
    disambiguate_labels, encode_project, has_active_descendant, human_dur, idle_str, parse_limits,
    parse_session, parse_stat, project_to_full, usage_level, windowed_rate, Level, Limits, Sample,
    TokenCounter,
};

/// Which coding agent produced a row — drives the little brand icon and (since only Claude Code's
/// statusline reports a dollar figure) whether `$/hr`/`$total` are meaningful at all. Codex under
/// ChatGPT-plan auth tracks quota percentage, not USD, so its rows carry `cost = 0.0` (making the
/// generic rate math a harmless no-op) gated by `has_cost = false` so the renderer shows "—"
/// instead of a fabricated `$0/hr`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Provider {
    Claude,
    Codex,
}

impl Provider {
    fn icon(self) -> Icon {
        match self {
            Provider::Claude => Icon::Claude,
            Provider::Codex => Icon::OpenAi,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }
}

// ── tunables (ported from the WASM plugin; see its docs for the reasoning) ──────────────────────
const ATTN_SECS: i64 = 60; // quieter than this & not working ⇒ the row dims and shows idle time
const STALE_SECS: i64 = 4 * 3600; // past this an agent is abandoned, drops out of the live headline
const CPU_BUSY_TPS: i64 = 10; // ticks/s above which a proc counts as "doing work" (idle drip is ~1)
const SAMPLE_SECS: i64 = 60; // sample-ring resolution for the windowed rates
const RING_SECS: i64 = 24 * 3600; // sample-ring horizon
const POLL: Duration = Duration::from_millis(1500); // native /proc scan is cheap; keep it lively
const LINE_CAP: usize = 256 * 1024; // per-transcript-line buffer cap (a giant tool result is skipped)
const READ_CHUNK: usize = 64 * 1024;
const BAR_H: f32 = 24.0; // dock damage-bar / row height (text overlaid)

/// The rate window the popup selector chooses (ported). `start` is the epoch the window measures
/// from; `i64::MIN` for `All` ⇒ the lifetime/anchor baseline in [`windowed_rate`].
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Window {
    #[default]
    All,
    Today,
    Hour,
}

impl Window {
    fn start(self, now: i64) -> i64 {
        match self {
            Window::All => i64::MIN,
            Window::Hour => now - 3600,
            Window::Today => local_midnight(now),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Window::All => "All",
            Window::Today => "Today",
            Window::Hour => "1h",
        }
    }
}

/// Epoch of the most recent local midnight (the start of "Today"), via `chrono::Local`. Falls back
/// to `now` if the conversion can't resolve (a degenerate DST gap), which just makes Today empty.
fn local_midnight(now: i64) -> i64 {
    let Some(dt) = Local.timestamp_opt(now, 0).single() else {
        return now;
    };
    let Some(midnight) = dt.date_naive().and_hms_opt(0, 0, 0) else {
        return now;
    };
    Local
        .from_local_datetime(&midnight)
        .single()
        .map(|m| m.timestamp())
        .unwrap_or(now)
}

// ── poll payload (stream → update) ──────────────────────────────────────────────────────────────

/// One agent as derived by a poll, before windowing. Cumulative counters straight from the
/// statusline snapshot + transcript; the per-window `$/hr`/`tok/s` are computed later in `update`.
struct RawAgent {
    label: String,
    session: String,
    provider: Provider,
    idle: i64,
    working: bool,
    /// `0.0` (and `has_cost = false`) for a Codex agent — Codex reports no dollar figure under
    /// ChatGPT-plan auth, so this is a hard zero, not an estimate.
    cost: f64,
    has_cost: bool,
    api_secs: f64,
    out_tokens: u64,
    /// cumulative input tokens (input + cache creation + cache read), for the per-agent breakdown.
    in_tokens: u64,
    /// the git branch / worktree the agent's cwd is on (its worktree identity).
    branch: Option<String>,
    /// context-window fullness 0..100 (the actionable "about to compact" number).
    context_pct: Option<f64>,
    /// sway con_id of the window hosting this agent's terminal — click-to-focus target (P4),
    /// and (vs the live focused con) the "selected" highlight.
    con_id: Option<i64>,
}

/// A whole settled poll handed from the I/O stream to the module.
struct Poll {
    now: i64,
    agents: Vec<RawAgent>,
    limits: Option<Limits>,
    /// Codex's account-wide rate-limit snapshot — freshest one seen across every Codex rollout
    /// tailed this poll (it's account-wide, like Claude's, so any live session's copy is as good
    /// as any other's).
    codex_limits: Option<codex_logic::Limits>,
    /// con_id of the focused sway window at poll time (re-syncs the live focus tracking).
    focused: Option<i64>,
}

enum Msg {
    Poll(Poll),
    HoverEnter,
    HoverLeave,
    Open,
    Window(Window),
    /// Click-to-focus (P4): jump sway to the window hosting this agent's terminal.
    Focus(i64),
    /// Close button: terminate the agent's window (sway close request → the terminal exits, the
    /// agent gets SIGHUP).
    Kill(i64),
    /// "Organize" — gather every mappable agent window onto the current screen and lay them out
    /// deterministically by importance (master-stack), like a bag-sort: same agents → same result.
    Organize,
    /// The focused sway window changed (event-driven, between polls) — repaint the "selected"
    /// highlight instantly instead of waiting ~1s for the next poll.
    FocusChanged(Option<i64>),
}

// ── rendered agent (post-windowing) ─────────────────────────────────────────────────────────────

struct Agent {
    label: String,
    session: String,
    provider: Provider,
    idle: i64,
    working: bool,
    cost: f64,
    has_cost: bool,
    api_secs: f64,
    out_tokens: u64,
    in_tokens: u64,
    branch: Option<String>,
    context_pct: Option<f64>,
    dps: f64,
    tps: f64,
    con_id: Option<i64>,
}

pub struct Agents {
    instance: u64,
    agents: Vec<Agent>,
    limits: Option<Limits>,
    codex_limits: Option<codex_logic::Limits>,
    /// per-session **first-seen anchor** `(cost, api_secs, out_tokens)` — the token baseline for
    /// All (the transcript is only tailed from first sight), and the full baseline for short windows.
    anchors: HashMap<String, (f64, f64, u64)>,
    /// per-session sample ring (≈1-min resolution, 24h) — the Today/1h baselines.
    samples: HashMap<String, Vec<Sample>>,
    window: Window,
    /// trend of the combined live $/hr — the chip + popup sparkline.
    dps_hist: Vec<f64>,
    /// sparse `(epoch, five_hour_used%)` samples → project time-to-limit.
    limit_hist: Vec<(i64, f64)>,
    /// con_id of the focused sway window — updated event-driven (RFC 0022) so the "selected"
    /// highlight repaints the instant focus moves, not on the next poll.
    focused_con: Option<i64>,
    last_now: i64,
    /// false until the first poll lands ⇒ a quiet loading chip rather than a misleading "0 agents".
    have_data: bool,
}

impl Agents {
    pub fn new(instance: u64, _cfg: &toml::Value) -> Self {
        Agents {
            instance,
            agents: Vec::new(),
            limits: None,
            codex_limits: None,
            anchors: HashMap::new(),
            samples: HashMap::new(),
            window: Window::default(),
            dps_hist: Vec::new(),
            limit_hist: Vec::new(),
            focused_con: None,
            last_now: 0,
            have_data: false,
        }
    }

    /// Merge a settled poll: anchor/sample each session, compute its windowed rates, sort the meter
    /// biggest-spender-first, and append the combined-rate trend point. Pure CPU — `update`-safe.
    fn ingest(&mut self, poll: &Poll) {
        self.last_now = poll.now;
        self.limits = poll.limits.clone();
        self.codex_limits = poll.codex_limits.clone();
        self.focused_con = poll.focused; // re-sync; the event stream keeps it fresh between polls
        self.sample_limit(poll.now);

        // Drop per-session state for sessions that are gone (keep it bounded + correct).
        let live_owned: HashSet<String> = poll.agents.iter().map(|a| a.session.clone()).collect();
        self.anchors.retain(|s, _| live_owned.contains(s));
        self.samples.retain(|s, _| live_owned.contains(s));

        for a in &poll.agents {
            if a.session.is_empty() {
                continue;
            }
            self.anchors
                .entry(a.session.clone())
                .or_insert((a.cost, a.api_secs, a.out_tokens));
            let ring = self.samples.entry(a.session.clone()).or_default();
            if ring.last().is_none_or(|s| poll.now - s.0 >= SAMPLE_SECS) {
                ring.push((poll.now, a.cost, a.api_secs, a.out_tokens));
                let cutoff = poll.now - RING_SECS;
                if let Some(pos) = ring.iter().position(|s| s.0 >= cutoff) {
                    if pos > 0 {
                        ring.drain(0..pos);
                    }
                }
            }
        }

        self.agents = poll
            .agents
            .iter()
            .map(|a| Agent {
                label: a.label.clone(),
                session: a.session.clone(),
                provider: a.provider,
                idle: a.idle,
                working: a.working,
                cost: a.cost,
                has_cost: a.has_cost,
                api_secs: a.api_secs,
                out_tokens: a.out_tokens,
                in_tokens: a.in_tokens,
                branch: a.branch.clone(),
                context_pct: a.context_pct,
                dps: 0.0,
                tps: 0.0,
                con_id: a.con_id,
            })
            .collect();
        self.recompute_rates();

        // Recount's inviolable law: the longest bar is always on top. The per-agent bar encodes
        // **output tokens** (the "damage done"), so the rows must be sorted by that same metric —
        // bar lengths then decrease monotonically down the list. Tiebreak by cost then name.
        self.agents.sort_by(|a, b| {
            b.out_tokens
                .cmp(&a.out_tokens)
                .then(cmp_desc(a.cost, b.cost))
                .then(a.label.cmp(&b.label))
        });

        self.dps_hist.push(self.live_dps());
        if self.dps_hist.len() > 60 {
            self.dps_hist.remove(0);
        }
        self.have_data = true;
    }

    /// Recompute every agent's windowed `(dps, tps)` for the current window from its anchor + ring.
    fn recompute_rates(&mut self) {
        let win_start = self.window.start(self.last_now);
        let empty: &[Sample] = &[];
        let rates: Vec<(f64, f64)> = self
            .agents
            .iter()
            .map(|a| {
                let anchor = self.anchors.get(&a.session).copied().unwrap_or((
                    a.cost,
                    a.api_secs,
                    a.out_tokens,
                ));
                let ring = self.samples.get(&a.session).map_or(empty, |v| v.as_slice());
                windowed_rate(ring, anchor, win_start, a.cost, a.api_secs, a.out_tokens)
            })
            .collect();
        for (a, (d, t)) in self.agents.iter_mut().zip(rates) {
            a.dps = d;
            a.tps = t;
        }
    }

    /// Combined **live** $/hr — the team's "raid DPS": the agents that are spending (a real rate and
    /// not abandoned). The bright popup rows sum to this.
    fn live_dps(&self) -> f64 {
        self.agents
            .iter()
            .filter(|a| burning(a))
            .map(|a| a.dps)
            .sum()
    }
    fn live_tps(&self) -> f64 {
        self.agents
            .iter()
            .filter(|a| burning(a))
            .map(|a| a.tps)
            .sum()
    }

    fn sample_limit(&mut self, now: i64) {
        let Some(p) = self.limits.as_ref().and_then(|l| l.five_used) else {
            return;
        };
        if self.limit_hist.last().is_none_or(|&(t, _)| now - t >= 20) {
            self.limit_hist.push((now, p));
            if self.limit_hist.len() > 24 {
                self.limit_hist.remove(0);
            }
        }
    }
    fn project_five(&self) -> Option<i64> {
        project_to_full(&self.limit_hist, 60, 0.5)
    }
}

impl Module for Agents {
    fn id(&self) -> &str {
        "agents"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        // The slow `/proc` poll, plus a lightweight event-driven stream of sway focus changes so
        // the "selected" highlight repaints the instant the user switches windows.
        Subscription::batch([
            ezbar_plugin::sub::keyed(self.instance, agents_stream),
            ezbar_plugin::sub::keyed(self.instance, focus_stream),
        ])
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Poll(p)) => {
                self.ingest(p);
                Response::none()
            }
            Some(Msg::HoverEnter) => Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::HoverLeave) => Response::request(HostRequest::ClosePopup),
            // Clicking the bar chip toggles the dock/sidebar (not a popup).
            Some(Msg::Open) => Response::request(HostRequest::ToggleDock),
            Some(Msg::Window(w)) => {
                if *w != self.window {
                    self.window = *w;
                    self.dps_hist.clear(); // don't blend the old window's trend with the new one's
                    self.recompute_rates();
                }
                Response::none()
            }
            // P4: focusing a window on another workspace/output makes sway switch to it — so one
            // click jumps to wherever the agent's terminal lives and focuses it.
            Some(Msg::Focus(con_id)) => {
                // focus + an opacity flash so the window is easy to spot (RFC 0022).
                sway::focus_flash(*con_id);
                Response::none()
            }
            Some(Msg::Kill(con_id)) => {
                sway::run_command(format!("[con_id={con_id}] kill"));
                Response::none()
            }
            Some(Msg::Organize) => {
                // Agents in importance order (dock order = output desc). Only those with a
                // resolvable local window participate.
                let cons: Vec<i64> = self.agents.iter().filter_map(|a| a.con_id).collect();
                if !cons.is_empty() {
                    sway::run_staged(organize_stages(&cons));
                }
                Response::none()
            }
            // Event-driven focus change → repaint the selected highlight now (no poll wait).
            Some(Msg::FocusChanged(c)) => {
                self.focused_con = *c;
                Response::none()
            }
            None => Response::none(),
        }
    }

    fn hover_messages(&self) -> Option<(ModMsg, ModMsg)> {
        Some((ModMsg::new(Msg::HoverEnter), ModMsg::new(Msg::HoverLeave)))
    }
    fn click_message(&self) -> Option<ModMsg> {
        Some(ModMsg::new(Msg::Open))
    }
    fn popup_size(&self) -> Option<(u32, u32)> {
        Some((460, 360))
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let pal = Pal::new(ctx);
        if !self.have_data {
            return row(vec![
                Icon::Bot.view(13.0, pal.dim),
                txt("\u{2026}".into(), 13.0, pal.dim),
            ])
            .spacing(5)
            .align_y(Vertical::Center)
            .into();
        }
        let total = self.agents.len();
        let bot = if total > 0 { pal.accent } else { pal.dim };
        let mut parts: Vec<Element<ModMsg>> = vec![
            Icon::Bot.view(13.0, bot),
            txt(
                format!("{total}"),
                13.0,
                if total > 0 { pal.fg } else { pal.dim },
            ),
        ];
        let any_cost = self.agents.iter().any(|a| a.has_cost);
        let total_dps = self.live_dps().max(0.0);
        if total > 0 {
            if any_cost {
                let dps_color = if total_dps >= 1.0 {
                    pal.accent
                } else {
                    pal.dim
                };
                parts.push(txt(fmt_rate(total_dps), 13.0, dps_color));
                if self.dps_hist.len() >= 2 {
                    parts.push(sparkline(self.dps_hist.clone(), dps_color, 48.0, 16.0));
                }
            }
            let total_tps = self.live_tps();
            parts.push(txt(
                fmt_tps(total_tps),
                13.0,
                if total_tps >= 1.0 {
                    pal.accent
                } else {
                    pal.dim
                },
            ));
        }
        if let Some(l) = self.limits.as_ref() {
            if l.five_used.is_some() || l.week_used.is_some() {
                parts.push(Space::new().width(Length::Fixed(7.0)).into());
            }
            if let Some(p) = l.five_used {
                parts.push(limit_pill("5h", p, &pal));
            }
            if let Some(p) = l.week_used {
                parts.push(limit_pill("7d", p, &pal));
            }
        }
        // Codex's account-wide limit windows, tagged with its icon (unlike Claude's pills above —
        // unchanged so an existing Claude-only setup looks exactly as it did) since a bare "7d"
        // beside a Claude "7d" would otherwise read as a duplicate, not a second provider.
        if let Some(cl) = self.codex_limits.as_ref() {
            for w in [&cl.primary, &cl.secondary].into_iter().flatten() {
                parts.push(Space::new().width(Length::Fixed(4.0)).into());
                parts.push(Provider::Codex.icon().view(11.0, pal.dim));
                parts.push(limit_pill(&w.label, w.used, &pal));
            }
        }
        row(parts).spacing(5).align_y(Vertical::Center).into()
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let pal = Pal::new(ctx);
        if !self.have_data {
            return Some(
                row(vec![
                    Icon::Bot.view(15.0, pal.accent),
                    txt("Agents".into(), 15.0, pal.fg),
                    txt("\u{2026}".into(), 15.0, pal.dim),
                ])
                .spacing(8)
                .align_y(Vertical::Center)
                .into(),
            );
        }
        let total = self.agents.len();
        let any_cost = self.agents.iter().any(|a| a.has_cost);
        let total_dps = self.live_dps().max(0.0);
        let total_cost: f64 = self.agents.iter().map(|a| a.cost).sum();
        let mut col: Vec<Element<ModMsg>> = Vec::new();

        // header
        let mut hdr: Vec<Element<ModMsg>> = vec![
            Icon::Bot.view(15.0, pal.accent),
            txt("Agents".into(), 15.0, pal.fg),
            txt(
                format!(
                    "\u{00b7} {total} agent{}",
                    if total == 1 { "" } else { "s" }
                ),
                12.0,
                pal.dim,
            ),
        ];
        if total > 0 && any_cost {
            hdr.push(txt(
                format!("\u{00b7} {}", fmt_rate(total_dps)),
                12.0,
                if total_dps >= 1.0 {
                    pal.accent
                } else {
                    pal.dim
                },
            ));
        }
        col.push(row(hdr).spacing(8).align_y(Vertical::Center).into());

        // the meter — one row per agent
        if total == 0 {
            col.push(txt("no agents running".into(), 13.0, pal.dim));
        } else {
            let max_out = self.agents.iter().map(|a| a.out_tokens).max().unwrap_or(0) as f64;
            for a in &self.agents {
                let dot_c = if a.working || a.idle < ATTN_SECS {
                    pal.ok
                } else {
                    pal.dim
                };
                let live = burning(a);
                let mut r: Vec<Element<ModMsg>> = vec![
                    Icon::Dot.view(12.0, dot_c),
                    meter_bar(a.out_tokens as f64, max_out, &pal),
                    txt(
                        pad_num(&fmt_rate_for(a), 7),
                        13.0,
                        if live { pal.accent } else { pal.dim },
                    ),
                    txt(pad_num(&fmt_tps(a.tps), 9), 11.0, pal.dim),
                    txt(pad_num(&fmt_cost(a), 6), 11.0, pal.dim),
                    a.provider.icon().view(12.0, pal.dim),
                    txt(a.label.clone(), 13.0, pal.fg),
                ];
                if !a.working && a.idle >= ATTN_SECS {
                    r.push(txt(idle_str(a.idle), 11.0, pal.dim));
                }
                // P4: the whole row is a click target that focuses this agent's window (when we
                // resolved one). A row with no mappable window (tmux/ssh/detached) stays inert.
                let row_el = row(r).spacing(8).align_y(Vertical::Center);
                let el: Element<ModMsg> = match a.con_id {
                    Some(cid) => mouse_area(row_el)
                        .interaction(Interaction::Pointer)
                        .on_press(ModMsg::new(Msg::Focus(cid)))
                        .into(),
                    None => row_el.into(),
                };
                col.push(el);
            }
        }

        // spend total + trend — only when some agent actually tracks a dollar cost (Codex under
        // ChatGPT-plan auth doesn't; showing "$0 total spent" while agents are running would be
        // read as broken, not "not applicable").
        if total > 0 && any_cost {
            col.push(rule(&pal));
            col.push(
                row(vec![
                    txt(format!("${total_cost:.0}"), 13.0, pal.fg),
                    txt("total spent".into(), 12.0, pal.dim),
                ])
                .spacing(6)
                .align_y(Vertical::Center)
                .into(),
            );
            if self.dps_hist.len() >= 3 {
                col.push(sparkline(self.dps_hist.clone(), pal.accent, 248.0, 34.0));
            }
        }

        // limits — Claude's and Codex's are independent account-wide quotas, so each gets its own
        // block; a provider tag line only appears when both are present (a single-provider setup
        // looks exactly as it always has).
        let has_claude_limits = self
            .limits
            .as_ref()
            .is_some_and(|l| l.five_used.is_some() || l.week_used.is_some());
        let has_codex_limits = self
            .codex_limits
            .as_ref()
            .is_some_and(|l| l.primary.is_some() || l.secondary.is_some());
        if has_claude_limits || has_codex_limits {
            col.push(rule(&pal));
            col.push(txt("Limits".into(), 12.0, pal.dim));
            if let Some(l) = &self.limits {
                if has_claude_limits && has_codex_limits {
                    col.push(provider_tag_row(Provider::Claude, &pal));
                }
                if let Some(p) = l.five_used {
                    col.push(limit_row("5h", p, l.five_reset_in, &pal));
                }
                if let Some(p) = l.week_used {
                    col.push(limit_row("7d", p, l.week_reset_in, &pal));
                }
                if let (Some(eta), Some(u5)) = (self.project_five(), l.five_used) {
                    if u5 > 20.0 && eta < l.five_reset_in {
                        col.push(
                            row(vec![
                                Icon::Alert.view(12.0, pal.urgent),
                                txt(
                                    format!(
                                        "5h limit in ~{} \u{00b7} resets {}",
                                        human_dur(eta),
                                        human_dur(l.five_reset_in)
                                    ),
                                    12.0,
                                    pal.urgent,
                                ),
                            ])
                            .spacing(6)
                            .align_y(Vertical::Center)
                            .into(),
                        );
                    }
                }
            }
            if let Some(cl) = &self.codex_limits {
                if has_claude_limits && has_codex_limits {
                    col.push(provider_tag_row(Provider::Codex, &pal));
                }
                for w in [&cl.primary, &cl.secondary].into_iter().flatten() {
                    col.push(limit_row(&w.label, w.used, w.reset_in, &pal));
                }
            }
        }

        // window selector — clickable only in the sticky (click-opened) popup
        col.push(rule(&pal));
        let sel = |w: Window| -> Element<ModMsg> {
            let on = self.window == w;
            mouse_area(txt(
                w.label().to_string(),
                12.0,
                if on { pal.accent } else { pal.dim },
            ))
            .interaction(Interaction::Pointer)
            .on_press(ModMsg::new(Msg::Window(w)))
            .into()
        };
        col.push(
            row(vec![
                sel(Window::All),
                sel(Window::Today),
                sel(Window::Hour),
            ])
            .spacing(16)
            .align_y(Vertical::Center)
            .into(),
        );

        Some(column(col).spacing(5).into())
    }

    /// The dock (RFC 0022): a vertical, two-lines-per-agent panel that reads cleanly down a narrow
    /// side surface — the agent's **name** on its own line (dot-coloured by liveness), its rates
    /// indented beneath. The whole agent block is the click-to-focus target. Compact limits + the
    /// window selector trail it. (The chip's hover `popup` stays the wide one-line Recount meter.)
    fn dock_view(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let pal = Pal::new(ctx);
        if !self.have_data {
            return Some(
                row(vec![
                    Icon::Bot.view(15.0, pal.accent),
                    txt("Agents".into(), 15.0, pal.fg),
                    txt("\u{2026}".into(), 15.0, pal.dim),
                ])
                .spacing(8)
                .align_y(Vertical::Center)
                .into(),
            );
        }
        let total = self.agents.len();
        let any_cost = self.agents.iter().any(|a| a.has_cost);
        let total_dps = self.live_dps().max(0.0);
        let mut col: Vec<Element<ModMsg>> = Vec::new();

        // ── header: title + (when agents exist) the Organize button, then the team summary ──
        let mut hdr: Vec<Element<ModMsg>> = vec![
            Icon::Bot.view(16.0, if total > 0 { pal.accent } else { pal.dim }),
            txt("Agents".into(), 15.0, pal.fg),
        ];
        if total > 0 {
            hdr.push(Space::new().width(Length::Fill).into());
            hdr.push(organize_button(&pal));
        }
        col.push(row(hdr).spacing(8).align_y(Vertical::Center).into());
        let tot_out: u64 = self.agents.iter().map(|a| a.out_tokens).sum();
        let tot_in: u64 = self.agents.iter().map(|a| a.in_tokens).sum();
        let max_out = self.agents.iter().map(|a| a.out_tokens).max().unwrap_or(0);
        let mut summary: Vec<Element<ModMsg>> = vec![txt(
            format!("{total} agent{}", if total == 1 { "" } else { "s" }),
            12.0,
            pal.dim,
        )];
        // $/hr only when some agent actually tracks a cost — see the popup's identical gate.
        if any_cost {
            summary.push(txt("\u{00b7}".into(), 12.0, pal.dim));
            summary.push(txt(
                fmt_rate(total_dps),
                12.0,
                if total_dps >= 1.0 {
                    pal.accent
                } else {
                    pal.dim
                },
            ));
        }
        summary.push(txt("\u{00b7}".into(), 12.0, pal.dim));
        summary.push(txt(
            format!("\u{2191}{}", fmt_tokens(tot_out)),
            12.0,
            pal.fg,
        ));
        summary.push(txt(
            format!("\u{2193}{}", fmt_tokens(tot_in)),
            10.0,
            Color { a: 0.5, ..pal.dim },
        ));
        col.push(row(summary).spacing(6).align_y(Vertical::Center).into());
        col.push(rule(&pal));

        // ── one meter block per agent (biggest producer on top) ──
        if total == 0 {
            col.push(txt("no agents running".into(), 13.0, pal.dim));
        } else {
            for a in &self.agents {
                let working = a.working;
                let recent = working || a.idle < ATTN_SECS;
                let frac = if max_out > 0 {
                    (a.out_tokens as f64 / max_out as f64) as f32
                } else {
                    0.0
                };
                // bar fill: kept DARK so the bright title reads at full contrast across the whole
                // bar (a light fill fights the light text — the classic meter mistake). Working →
                // dark-muted green; idle → dark-muted lilac (accent); parked → dimmer. Bright
                // saturated green is reserved for the liveness *dot*, not the fill.
                let fill_c = if working {
                    Color { a: 0.32, ..pal.ok }
                } else if recent {
                    Color {
                        a: 0.28,
                        ..pal.accent
                    }
                } else {
                    Color {
                        a: 0.16,
                        ..pal.accent
                    }
                };
                let track_c = Color { a: 0.07, ..pal.fg };
                let title_c = if recent {
                    pal.fg
                } else {
                    Color { a: 0.82, ..pal.fg }
                };

                // ── meter: a full-width damage bar with only the title overlaid; the numbers live
                // in a fixed right gutter OUTSIDE the bar, so the leader's fill can't collide with
                // them (and `%` reads as strongly as the token count) ──
                // clip to a width-appropriate length so the ellipsis is the last visible glyph —
                // a clean `…`, never a hard mid-glyph pixel cut (the container clip is just a net).
                let title_overlay = container(
                    text(clip(&a.label, 27))
                        .size(13.0)
                        .color(title_c)
                        .wrapping(Wrapping::None),
                )
                .padding([0, 10])
                .height(Length::Fixed(BAR_H))
                .align_y(Vertical::Center)
                .width(Length::Fill)
                .clip(true);
                let meter = stack(vec![
                    bar_bg(frac, BAR_H, fill_c, track_c),
                    title_overlay.into(),
                ]);

                // right gutter: output (the "damage") · % of total · $/hr (the live DPS — accent
                // only while recently active, else dim so a long-parked agent doesn't shout a stale
                // top rate).
                let top = row(vec![
                    liveness_dot(working, recent, &pal),
                    a.provider.icon().view(12.0, Color { a: 0.7, ..pal.dim }),
                    meter.into(),
                    rcell(
                        format!("\u{2191}{}", fmt_tokens(a.out_tokens)),
                        52.0,
                        12.0,
                        pal.fg,
                    ),
                    container(context_gauge(a.context_pct, &pal))
                        .width(Length::Fixed(58.0))
                        .align_x(Horizontal::Right)
                        .into(),
                    rcell(
                        fmt_rate_for(a),
                        62.0,
                        13.0,
                        if recent { pal.accent } else { pal.dim },
                    ),
                ])
                .spacing(6)
                .align_y(Vertical::Center);

                // sub-line — worktree/branch, then input tokens + idle (demoted: smaller, dimmer).
                let dim2 = Color { a: 0.5, ..pal.dim };
                let mut sub: Vec<Element<ModMsg>> =
                    vec![Space::new().width(Length::Fixed(19.0)).into()];
                if let Some(b) = &a.branch {
                    // git-branch glyph + name = the agent's worktree identity.
                    sub.push(txt(
                        format!("\u{e0a0} {b}"),
                        10.0,
                        Color {
                            a: 0.75,
                            ..pal.accent
                        },
                    ));
                    sub.push(txt("\u{00b7}".into(), 10.0, dim2));
                }
                sub.push(txt(
                    format!("\u{2193}{} in", fmt_tokens(a.in_tokens)),
                    10.0,
                    dim2,
                ));
                if !working && a.idle >= ATTN_SECS {
                    sub.push(txt("\u{00b7}".into(), 10.0, dim2));
                    sub.push(txt(idle_str(a.idle), 10.0, dim2));
                }
                let block = column(vec![top.into(), row(sub).spacing(5).into()])
                    .spacing(3)
                    .width(Length::Fill);

                // focus the block on click; the × close button is a *separate* sibling hit target
                // (no nested mouse_area), so a close never doubles as a focus. The × column is
                // always reserved (a Space when there's no mappable window) so $/hr stays aligned.
                let body: Element<ModMsg> = match a.con_id {
                    Some(cid) => mouse_area(block)
                        .interaction(Interaction::Pointer)
                        .on_press(ModMsg::new(Msg::Focus(cid)))
                        .into(),
                    None => block.into(),
                };
                let close: Element<ModMsg> = match a.con_id {
                    Some(cid) => kill_button(cid, &pal),
                    None => Space::new().width(Length::Fixed(20.0)).into(),
                };
                // Top-align so the × sits on the primary ($/hr) line, not centered on the 2-line
                // block where it would read as orphaned beside the sub-line gutter.
                let row_content = row(vec![body, close]).spacing(2).align_y(Vertical::Top);

                // selected = this agent's terminal is the focused sway window (tracked live via the
                // focus event stream) → accent wash + ring, repainted the instant focus moves.
                let selected = a.con_id.is_some() && a.con_id == self.focused_con;
                let accent = pal.accent;
                let cell = if selected {
                    container(row_content)
                        .padding([5, 6])
                        .width(Length::Fill)
                        .style(move |_| container::Style {
                            background: Some(Background::Color(Color { a: 0.10, ..accent })),
                            border: Border {
                                color: Color { a: 0.55, ..accent },
                                width: 1.0,
                                radius: 8.0.into(),
                            },
                            ..Default::default()
                        })
                } else {
                    container(row_content).padding([5, 6]).width(Length::Fill)
                };
                col.push(cell.into());
            }
        }

        // ── compact limits (Claude's + Codex's — see the popup's identical layout for why each
        // gets its own block and the provider tag only shows up when both are present) ──
        let has_claude_limits = self
            .limits
            .as_ref()
            .is_some_and(|l| l.five_used.is_some() || l.week_used.is_some());
        let has_codex_limits = self
            .codex_limits
            .as_ref()
            .is_some_and(|l| l.primary.is_some() || l.secondary.is_some());
        if has_claude_limits || has_codex_limits {
            col.push(rule(&pal));
            if let Some(l) = &self.limits {
                if has_claude_limits && has_codex_limits {
                    col.push(provider_tag_row(Provider::Claude, &pal));
                }
                if let Some(p) = l.five_used {
                    col.push(limit_row("5h", p, l.five_reset_in, &pal));
                }
                if let Some(p) = l.week_used {
                    col.push(limit_row("7d", p, l.week_reset_in, &pal));
                }
            }
            if let Some(cl) = &self.codex_limits {
                if has_claude_limits && has_codex_limits {
                    col.push(provider_tag_row(Provider::Codex, &pal));
                }
                for w in [&cl.primary, &cl.secondary].into_iter().flatten() {
                    col.push(limit_row(&w.label, w.used, w.reset_in, &pal));
                }
            }
        }

        // ── window selector ──
        col.push(rule(&pal));
        let sel = |w: Window| -> Element<ModMsg> {
            let on = self.window == w;
            mouse_area(txt(
                w.label().to_string(),
                12.0,
                if on { pal.accent } else { pal.dim },
            ))
            .interaction(Interaction::Pointer)
            .on_press(ModMsg::new(Msg::Window(w)))
            .into()
        };
        col.push(
            row(vec![
                sel(Window::All),
                sel(Window::Today),
                sel(Window::Hour),
            ])
            .spacing(16)
            .into(),
        );

        Some(column(col).spacing(6).into())
    }
}

// ── small theme palette ─────────────────────────────────────────────────────────────────────────

struct Pal {
    fg: Color,
    dim: Color,
    accent: Color,
    ok: Color,
    warn: Color,
    urgent: Color,
    sep: Color,
}
impl Pal {
    fn new(ctx: &Ctx) -> Self {
        Pal {
            fg: ctx.fg(),
            dim: ctx.fg_dim(),
            accent: ctx.accent(),
            ok: ctx.ok(),
            warn: ctx.warn(),
            urgent: ctx.urgent(),
            sep: ctx.sep(),
        }
    }
}

// ── render helpers ──────────────────────────────────────────────────────────────────────────────

fn txt<'a>(s: String, size: f32, color: Color) -> Element<'a, ModMsg> {
    text(s).size(size).color(color).into()
}

fn sparkline<'a>(values: Vec<f64>, color: Color, w: f32, h: f32) -> Element<'a, ModMsg> {
    canvas(Graph {
        values,
        kind: GraphKind::Generic,
        line_color: Some(color),
        line_width: 1.5,
        fill: true,
    })
    .width(Length::Fixed(w))
    .height(Length::Fixed(h))
    .into()
}

/// Map a severity [`Level`] to a theme colour.
fn level_color(l: Level, pal: &Pal) -> Color {
    match l {
        Level::Ok => pal.ok,
        Level::Warn => pal.warn,
        Level::Urgent => pal.urgent,
    }
}

fn limit_pill<'a>(label: &str, used: f64, pal: &Pal) -> Element<'a, ModMsg> {
    row(vec![
        txt(label.to_string(), 11.0, pal.dim),
        txt(
            format!("{used:.0}%"),
            13.0,
            level_color(usage_level(used), pal),
        ),
    ])
    .spacing(3)
    .align_y(Vertical::Center)
    .into()
}

fn limit_row<'a>(label: &str, used: f64, reset_in: i64, pal: &Pal) -> Element<'a, ModMsg> {
    // Same rounded track+fill as the agent bars (no graph-paper hatch); the fill ramps
    // green→amber→red by usage via `usage_level`.
    let c = level_color(usage_level(used), pal);
    row(vec![
        container(txt(label.to_string(), 12.0, pal.dim))
            .width(Length::Fixed(22.0))
            .into(),
        // muted fill — the rate-limit bars are secondary, so they must not out-shout the
        // (now dark) damage bars above them.
        container(bar_bg(
            (used / 100.0) as f32,
            8.0,
            Color { a: 0.7, ..c },
            Color { a: 0.07, ..pal.fg },
        ))
        .width(Length::Fixed(120.0))
        .into(),
        rcell(format!("{used:.0}%"), 36.0, 12.0, c),
        txt(format!("\u{00b7} {}", human_dur(reset_in)), 11.0, pal.dim),
    ])
    .spacing(8)
    .align_y(Vertical::Center)
    .into()
}

/// A fixed-width block-char meter bar (Recount-style): `value/max` of the cells filled. Filled cells
/// are neutral **fg** (structure, "how much already spent"); accent is reserved for the live $/hr.
fn meter_bar<'a>(value: f64, max: f64, pal: &Pal) -> Element<'a, ModMsg> {
    const W: usize = 10;
    let n = if max > 0.0 && value > 0.0 {
        (((value / max) * W as f64).round() as usize).clamp(1, W)
    } else {
        0
    };
    row(vec![
        txt("\u{2588}".repeat(n), 13.0, pal.fg),
        txt("\u{2591}".repeat(W - n), 13.0, pal.dim),
    ])
    .spacing(0)
    .into()
}

fn rule<'a>(pal: &Pal) -> Element<'a, ModMsg> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style({
            let c = pal.sep;
            move |_| container::Style {
                background: Some(Background::Color(c)),
                ..Default::default()
            }
        })
        .into()
}

/// Is this agent **spending** — any positive $/hr and not abandoned (parked past `STALE_SECS`)?
fn burning(a: &Agent) -> bool {
    a.dps > 0.0 && a.idle < STALE_SECS
}

/// Spend rate as `$N/hr`, but a positive sub-dollar rate renders `<$1/hr`, never a bare `$0`.
fn fmt_rate(dps: f64) -> String {
    let d = dps.max(0.0);
    if d <= 0.0 {
        "$0/hr".to_string()
    } else if d < 1.0 {
        "<$1/hr".to_string()
    } else {
        format!("${d:.0}/hr")
    }
}

/// `$/hr` for a row, or an em dash when the provider tracks no dollar cost at all (Codex under
/// ChatGPT-plan auth) — a bare `$0/hr` there would misread as "confirmed zero spend" rather than
/// "not tracked".
fn fmt_rate_for(a: &Agent) -> String {
    if a.has_cost {
        fmt_rate(a.dps)
    } else {
        "\u{2014}".to_string()
    }
}

/// Lifetime `$cost` for a row, or an em dash — see [`fmt_rate_for`].
fn fmt_cost(a: &Agent) -> String {
    if a.has_cost {
        format!("${:.0}", a.cost)
    } else {
        "\u{2014}".to_string()
    }
}

/// A small "which provider" tag line (icon + name) — only inserted ahead of a limits block when
/// BOTH Claude's and Codex's are present, so a single-provider setup's layout never changes.
fn provider_tag_row<'a>(p: Provider, pal: &Pal) -> Element<'a, ModMsg> {
    row(vec![
        p.icon().view(11.0, pal.dim),
        txt(p.label().to_string(), 11.0, pal.dim),
    ])
    .spacing(5)
    .align_y(Vertical::Center)
    .into()
}

fn fmt_tps(t: f64) -> String {
    if t >= 1000.0 {
        format!("{:.1}k t/s", t / 1000.0)
    } else {
        format!("{:.0} t/s", t)
    }
}

/// Compact token count: `1.2M` / `45k` / `123`.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// A rounded proportional bar that fills its container width: `frac` of a `h`-tall track filled
/// with `fill` on a dim `track`, both rounded. The Recount "damage" bar — used full-width as the
/// agent-row background (text overlaid) and fixed-width for the rate-limit rows. A nonzero `frac`
/// always shows a visible sliver.
fn bar_bg<'a>(frac: f32, h: f32, fill: Color, track: Color) -> Element<'a, ModMsg> {
    let frac = frac.clamp(0.0, 1.0);
    let fk = if frac > 0.0 {
        ((frac * 1000.0).round() as u16).max(10)
    } else {
        0
    };
    let rk = 1000u16.saturating_sub(fk);
    let radius = (h / 2.0).min(7.0);
    let mut inner: Vec<Element<ModMsg>> = Vec::new();
    if fk > 0 {
        inner.push(
            container(Space::new())
                .width(Length::FillPortion(fk))
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(fill)),
                    border: Border {
                        radius: radius.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into(),
        );
    }
    if rk > 0 {
        inner.push(Space::new().width(Length::FillPortion(rk)).into());
    }
    container(row(inner).spacing(0))
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .style(move |_| container::Style {
            background: Some(Background::Color(track)),
            border: Border {
                radius: radius.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

/// Liveness indicator (RFC 0022): a **filled** solid-Ok dot when the agent is actively running a
/// tool, a **hollow** ring otherwise — so "who's working" reads by *shape*, not hue alone (it
/// survives a busy wallpaper and colour-blindness). `recent` (active in the last minute) keeps the
/// ring a soft Ok; long-parked goes dim.
fn liveness_dot<'a>(working: bool, recent: bool, pal: &Pal) -> Element<'a, ModMsg> {
    if working {
        container(Space::new())
            .width(Length::Fixed(11.0))
            .height(Length::Fixed(11.0))
            .style({
                let c = pal.ok;
                move |_| container::Style {
                    background: Some(Background::Color(c)),
                    border: Border {
                        radius: 5.5.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })
            .into()
    } else {
        let c = if recent {
            Color { a: 0.6, ..pal.ok }
        } else {
            pal.dim
        };
        container(Space::new())
            .width(Length::Fixed(10.0))
            .height(Length::Fixed(10.0))
            .style(move |_| container::Style {
                background: None,
                border: Border {
                    color: c,
                    width: 1.5,
                    radius: 5.0.into(),
                },
                ..Default::default()
            })
            .into()
    }
}

/// A fixed-width, right-aligned text cell — so `↑tokens`, `%`, and `$/hr` form clean vertical
/// columns down the dock instead of floating with each row's text width.
fn rcell<'a>(s: String, w: f32, size: f32, color: Color) -> Element<'a, ModMsg> {
    container(text(s).size(size).color(color).wrapping(Wrapping::None))
        .width(Length::Fixed(w))
        .align_x(Horizontal::Right)
        .clip(true)
        .into()
}

/// A small close (×) button — terminates the agent's window (sway close request) on click. Dim by
/// default, **red on hover** (the destructive-action affordance), via the button's own status — a
/// deliberate, separate hit target from the row's focus click.
fn kill_button<'a>(con_id: i64, pal: &Pal) -> Element<'a, ModMsg> {
    let dim = Color { a: 0.5, ..pal.fg };
    let red = pal.urgent;
    button(
        container(text("\u{00d7}").size(15.0))
            .center_x(Length::Fixed(20.0))
            .center_y(Length::Fixed(BAR_H)),
    )
    .padding(0)
    .on_press(ModMsg::new(Msg::Kill(con_id)))
    .style(move |_, status| button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => red,
            _ => dim,
        },
        ..Default::default()
    })
    .into()
}

/// The "Organize" icon button in the dock header — one click lays every agent window out on the
/// current screen by importance (a deterministic master-stack). Dim, accent on hover.
fn organize_button<'a>(pal: &Pal) -> Element<'a, ModMsg> {
    let dim = Color { a: 0.6, ..pal.fg };
    let accent = pal.accent;
    button(text("\u{f009}").size(13.0)) // nf th-large (a tiling/grid glyph)
        .padding([2, 6])
        .on_press(ModMsg::new(Msg::Organize))
        .style(move |_, status| button::Style {
            background: None,
            text_color: match status {
                button::Status::Hovered | button::Status::Pressed => accent,
                _ => dim,
            },
            ..Default::default()
        })
        .into()
}

/// Context-window fullness as a small **gauge** (mini rounded bar) + a dim %. It's a "fullness"
/// widget, deliberately decoupled (bar + *dim* number) from the bright output-token count beside it
/// so the figure can't be misread as "% of output". Neutral with room, amber ≥70%, red ≥85%; the
/// bar makes the warning pop. "—" when the snapshot has no context figure yet.
fn context_gauge<'a>(pct: Option<f64>, pal: &Pal) -> Element<'a, ModMsg> {
    match pct {
        Some(p) => {
            let c = if p >= 85.0 {
                pal.urgent
            } else if p >= 70.0 {
                pal.warn
            } else {
                Color { a: 0.6, ..pal.fg }
            };
            row(vec![
                container(bar_bg(
                    (p / 100.0) as f32,
                    6.0,
                    c,
                    Color { a: 0.1, ..pal.fg },
                ))
                .width(Length::Fixed(26.0))
                .into(),
                txt(format!("{p:.0}%"), 10.0, Color { a: 0.6, ..pal.dim }),
            ])
            .spacing(4)
            .align_y(Vertical::Center)
            .into()
        }
        None => txt("\u{2014}".to_string(), 11.0, pal.dim),
    }
}

/// Right-align a short numeric string in a fixed `width`-char cell with figure-spaces (U+2007).
fn pad_num(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(s.chars().count());
    format!("{}{}", "\u{2007}".repeat(pad), s)
}

fn cmp_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

/// The "Organize" command: tile every agent window into a packed **grid** on the current workspace,
/// in importance order (row-major: most important top-left). The grid is ~square, biased wide for
/// landscape monitors — `cols = ⌈√N⌉` (so 4 → 2×2, 6 → 3×2, 9 → 3×3). Re-running rebuilds the same
/// grid (the bag-sort property).
///
/// How it builds reliably over sway IPC: a `move container to workspace` lands the window next to
/// the *focused* node, so the trick is (1) lay the column-heads out as one horizontal row first —
/// that gives real siblings to split off — then (2) for each head, `split vertical` and drop the
/// rest of its column below it. (Splitting a lone top-level container instead just nests everything
/// inside it, which is the trap earlier naive attempts fell into.)
/// Off-screen workspace agents are parked on while the grid is rebuilt (so the build always starts
/// from a clean slate → re-running reproduces the same grid).
const ORG_SCRATCH: &str = "ezbar_org_tmp";

/// Tile every agent window into a packed **grid** on the current workspace, in importance order
/// (row-major: most important top-left). The grid is ~square, biased wide for landscape monitors —
/// `cols = ⌈√N⌉` (so 4 → 2×2, 6 → 3×2, 9 → 3×3).
///
/// Returned as staged commands (not one batch) for two reasons sway forces on us:
///  - **Racing**: `move → split → move` sent together no-ops the split (the move hasn't landed in
///    the tree yet), so the caller ([`crate::sources::sway::run_staged`]) spaces them out.
///  - **Determinism**: the build is sensitive to the starting tree, so we first park every agent on
///    an off-screen scratch workspace — a clean slate, so re-running reproduces the same grid.
///
/// The build: lay the column-heads out as one horizontal row first (real siblings to split off),
/// then for each head `split vertical` and drop the rest of its column below it. (Splitting a lone
/// top-level container instead just nests everything inside it — the trap naive attempts fell into.)
fn organize_stages(cons: &[i64]) -> Vec<String> {
    let n = cons.len();
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize; // smart sizing — columns ≥ rows
    let heads = cols.min(n); // the top row: one head per column
    let mut v = Vec::new();

    // 0. reset: park every agent off-screen, un-floated, so the build starts clean.
    for &c in cons {
        v.push(format!(
            "[con_id={c}] floating disable; [con_id={c}] move container to workspace {ORG_SCRATCH}"
        ));
    }
    // 1. pull the column-heads back as the top row, then force it horizontal.
    for &c in &cons[..heads] {
        v.push(format!("[con_id={c}] move container to workspace current"));
    }
    v.push(format!(
        "[con_id={}] focus; focus parent; layout splith",
        cons[0]
    ));
    // 2. for each column: split its head and stack the column's remaining agents below it, in order.
    for c in 0..heads {
        let head = cons[c];
        let below: Vec<i64> = (1..)
            .map(|r| c + r * cols)
            .take_while(|&i| i < n)
            .map(|i| cons[i])
            .collect();
        if below.is_empty() {
            continue;
        }
        v.push(format!("[con_id={head}] focus; split vertical"));
        let mut last = head;
        for w in below {
            v.push(format!(
                "[con_id={last}] focus; [con_id={w}] move container to workspace current"
            ));
            last = w;
        }
    }
    v.push(format!("[con_id={}] focus", cons[0])); // focus the lead agent
    v
}

// ── the I/O stream (spawn_blocking) ─────────────────────────────────────────────────────────────

/// Poll-to-poll I/O bookkeeping owned by the stream: CPU deltas (busy-child detection) and the
/// incremental transcript read offsets + token counters. None of this is needed by the renderer.
#[derive(Default)]
struct Scanner {
    prev_cpu: HashMap<i32, u64>,
    prev_poll: i64,
    /// `path → (byte offset already read, counter)` — transcripts are append-only.
    token_files: HashMap<String, (u64, TokenCounter)>,
    /// one-tick hysteresis: absorb a single transient "0 agents" reading.
    saw_empty: bool,
    had_agents: bool,
    /// rollout file path → its `cwd` (from line 1, a `session_meta` event) — memoized because a
    /// rollout's cwd never changes, so re-parsing it every poll would be pure waste.
    codex_cwd_cache: HashMap<String, String>,
    /// rollout file path → (byte offset already read, latest cumulative usage seen, best label
    /// seen so far). Unlike Claude's `token_files`, no dedup counter is needed for usage: Codex's
    /// `token_count` events already carry running totals, so the latest one read simply wins.
    codex_files: HashMap<String, (u64, codex_logic::Usage, codex_logic::SessionLabel)>,
    /// rollout file path → accumulated active seconds — ezbar's own busy-wall-clock proxy for
    /// Codex's missing self-reported "API active duration" (see the module doc comment).
    codex_active: HashMap<String, f64>,
    /// The freshest Codex rate-limit snapshot ever seen (by the mtime of the rollout it came
    /// from), account-wide like Claude's. **Must persist across polls**: a `token_count` event only
    /// rides along on an actual model turn, which happens far less often than the poll cadence, so
    /// resetting this every tick would flash the limit pills empty between turns instead of holding
    /// the last known reading — unlike `codex_files`' usage figures, there is no per-tick fallback
    /// source to re-derive it from.
    codex_limits: Option<(i64, codex_logic::Limits)>,
}

/// Event-driven sway focus changes → `Msg::FocusChanged`, so the dock's selected highlight tracks
/// the active window instantly (the heavy `/proc` poll stays on its calm cadence).
fn focus_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    sway::focused_con().map(|c| ModMsg::new(Msg::FocusChanged(c)))
}

fn agents_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        4,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let mut scanner = Scanner::default();
            loop {
                let (s, poll) = tokio::task::spawn_blocking(move || {
                    let mut s = scanner;
                    let p = s.scan();
                    (s, p)
                })
                .await
                .unwrap_or_else(|_| (Scanner::default(), None));
                scanner = s;
                if let Some(p) = poll {
                    let _ = out.send(ModMsg::new(Msg::Poll(p))).await;
                }
                tokio::time::sleep(POLL).await;
            }
        },
    )
}

impl Scanner {
    /// One full poll: scan `/proc`, derive idle/working/cost/tokens per agent, read limits.
    /// `None` ⇒ a glitchy/empty scan that must NOT be published (would flash "no agents").
    fn scan(&mut self) -> Option<Poll> {
        let now = now_secs();
        let (procs, children) = read_procs();
        // `/proc` always holds the system processes — an empty read is a glitch, not "all closed".
        if procs.is_empty() {
            return None;
        }

        // active = doing work now: runnable/in-IO, or burning CPU above the idle-drip rate.
        let elapsed = if self.prev_poll > 0 {
            (now - self.prev_poll).max(1)
        } else {
            1
        };
        self.prev_poll = now;
        let busy_delta = (CPU_BUSY_TPS * elapsed) as u64;
        let mut active = HashSet::new();
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

        struct Raw {
            pid: i32,
            cwd: String,
            working: bool,
            busy: bool,
        }
        let to_raw = |p: &Proc| -> Raw {
            let working = has_active_descendant(p.pid, &children, &active);
            Raw {
                pid: p.pid,
                cwd: proc_cwd(p.pid),
                working,
                busy: working || active.contains(&p.pid),
            }
        };
        let raws: Vec<Raw> = procs
            .iter()
            .filter(|p| p.comm == "claude")
            .map(to_raw)
            .collect();
        let codex_raws: Vec<Raw> = procs
            .iter()
            .filter(|p| p.comm == "codex")
            .map(to_raw)
            .collect();

        // Absorb ONE transient zero-agent reading before committing it.
        if raws.is_empty() && codex_raws.is_empty() && self.had_agents && !self.saw_empty {
            self.saw_empty = true;
            return None;
        }
        self.saw_empty = raws.is_empty() && codex_raws.is_empty();
        self.had_agents = !raws.is_empty() || !codex_raws.is_empty();

        // Disambiguate labels across BOTH providers' cwds together, so a Claude agent and a Codex
        // agent that happen to share a basename still get `parent/basename` — the provider icon
        // alone tells them apart, but a stray identical label beside it would still read as a bug.
        let cwds: Vec<String> = raws
            .iter()
            .chain(codex_raws.iter())
            .map(|r| r.cwd.clone())
            .collect();
        let all_labels = disambiguate_labels(&cwds);
        let (labels, codex_labels) = all_labels.split_at(raws.len());

        // P4 window mapping: `pid → con_id` of every sway window, walked up each agent's ppid
        // ancestry to the first window-owning pid. The window TITLE matters too: claude sets the
        // terminal title to the running task, so it tells two SAME-CWD agents apart (e.g. two `main`
        // sessions) — without it the dock can't know which process is on which session. One sway
        // query per poll (skipped cheaply if sway isn't there).
        let ppid: HashMap<i32, i32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
        let (win_vec, focused) = sway::window_nodes();
        let mut win: HashMap<i32, i64> = HashMap::new();
        let mut title_by_con: HashMap<i64, String> = HashMap::new();
        for (p, c, t) in win_vec {
            win.insert(p, c);
            title_by_con.insert(c, t);
        }
        let con_id_for = |start: i32| -> Option<i64> {
            let mut cur = start;
            for _ in 0..64 {
                if let Some(&id) = win.get(&cur) {
                    return Some(id);
                }
                match ppid.get(&cur) {
                    Some(&p) if p > 1 => cur = p,
                    _ => return None,
                }
            }
            None
        };
        // each agent's window con_id + its task title (claude's status glyph stripped).
        let con_ids: Vec<Option<i64>> = raws.iter().map(|r| con_id_for(r.pid)).collect();
        let codex_con_ids: Vec<Option<i64>> =
            codex_raws.iter().map(|r| con_id_for(r.pid)).collect();
        let win_titles: Vec<String> = con_ids
            .iter()
            .map(|c| {
                c.and_then(|c| title_by_con.get(&c))
                    .map(|t| strip_status(t))
                    .unwrap_or_default()
            })
            .collect();

        // Map each agent process to its session. FIRST by window title → session name (claude's
        // title is the live task, so this is reliable even when several processes share one cwd, the
        // bug where two `main` agents pointed at the same window). Whatever can't be title-matched
        // falls back to the old heuristic: busy procs claim the freshest transcripts.
        let mut by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in raws.iter().enumerate() {
            by_cwd.entry(r.cwd.clone()).or_default().push(i);
        }
        let mut idle = vec![0i64; raws.len()];
        let mut session = vec![String::new(); raws.len()];
        for (cwd, idxs) in &by_cwd {
            // candidate sessions, newest first, with names (a tiny per-session json — cheap to read).
            let cand: Vec<(String, i64, String)> = transcript_sessions(cwd)
                .into_iter()
                .take(idxs.len() + 8)
                .map(|(sid, m)| {
                    let name = read_session(&sid).map(|s| s.name).unwrap_or_default();
                    (sid, m, name)
                })
                .collect();
            let mut used = vec![false; cand.len()];
            let mut unmatched: Vec<usize> = Vec::new();
            // 1. title → session-name match.
            for &i in idxs {
                let t = &win_titles[i];
                let mut hit = None;
                if !t.is_empty() {
                    for (k, (_, _, name)) in cand.iter().enumerate() {
                        if !used[k] && title_matches(t, name) {
                            hit = Some(k);
                            break;
                        }
                    }
                }
                match hit {
                    Some(k) => {
                        used[k] = true;
                        session[i] = cand[k].0.clone();
                        idle[i] = (now - cand[k].1).max(0);
                    }
                    None => unmatched.push(i),
                }
            }
            // 2. fallback: busy procs first, freshest unused transcript.
            unmatched.sort_by_key(|&i| !raws[i].busy);
            let mut ci = 0;
            for &i in &unmatched {
                while ci < cand.len() && used[ci] {
                    ci += 1;
                }
                if ci < cand.len() {
                    used[ci] = true;
                    session[i] = cand[ci].0.clone();
                    idle[i] = (now - cand[ci].1).max(0);
                    ci += 1;
                }
            }
        }

        let mut agents: Vec<RawAgent> = Vec::with_capacity(raws.len());
        let mut live_files: HashSet<String> = HashSet::new();
        for (i, r) in raws.iter().enumerate() {
            let sess = session[i].clone();
            let mut label = labels[i].clone();
            let (mut cost, mut api_secs) = (0.0, 0.0);
            let mut context_pct = None;
            if let Some(s) = read_session(&sess) {
                cost = s.cost;
                api_secs = s.api_secs;
                context_pct = s.context_pct;
                if !s.name.is_empty() {
                    label = clip(&s.name, 32);
                }
            }
            // tokens: cumulative output + input across the session's transcript(s). First sight
            // reads the FULL history (offset 0) so the counts are lifetime totals, not just
            // since-watch — native has no sandbox memory cap, and giant tool-result lines are still
            // skipped by LINE_CAP, so a multi-MB line never buffers. Then incremental each tick.
            let (mut out_tokens, mut in_tokens) = (0u64, 0u64);
            if !sess.is_empty() {
                for path in transcript_files(&r.cwd, &sess) {
                    let is_new = !self.token_files.contains_key(&path);
                    let entry = self.token_files.entry(path.clone()).or_default();
                    if is_new {
                        entry.0 = 0;
                    }
                    read_new_lines(&path, &mut entry.0, &mut entry.1);
                    out_tokens += entry.1.total();
                    in_tokens += entry.1.total_in();
                    live_files.insert(path);
                }
            }
            let con_id = con_ids[i];
            agents.push(RawAgent {
                label,
                session: sess,
                provider: Provider::Claude,
                idle: idle[i],
                working: r.working,
                cost,
                has_cost: true,
                api_secs,
                out_tokens,
                in_tokens,
                branch: git_branch(&r.cwd),
                context_pct,
                con_id,
            });
        }
        self.token_files.retain(|p, _| live_files.contains(p));

        // ── Codex: bucket by cwd, resolve each to its most-plausible rollout file. No window-title
        // matching here (Codex sets no comparable live-task title we could match against) — busy
        // procs simply claim the freshest file first, same fallback Claude uses when title
        // matching can't help. ──
        let mut codex_by_cwd: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, r) in codex_raws.iter().enumerate() {
            codex_by_cwd.entry(r.cwd.clone()).or_default().push(i);
        }
        let rollout_files = codex_rollout_files(&codex_day_dirs(now));
        for f in &rollout_files {
            if !self.codex_cwd_cache.contains_key(f) {
                if let Some(cwd) = codex_first_line_cwd(f) {
                    self.codex_cwd_cache.insert(f.clone(), cwd);
                }
            }
        }
        let mut codex_session = vec![String::new(); codex_raws.len()];
        let mut codex_idle = vec![0i64; codex_raws.len()];
        for (cwd, idxs) in &codex_by_cwd {
            let mut cand: Vec<(String, i64)> = rollout_files
                .iter()
                .filter(|f| self.codex_cwd_cache.get(f.as_str()) == Some(cwd))
                .filter_map(|f| file_mtime(f).map(|m| (f.clone(), m)))
                .collect();
            cand.sort_by_key(|&(_, m)| std::cmp::Reverse(m));
            cand.truncate(idxs.len() + 8);
            let mut used = vec![false; cand.len()];
            let mut order: Vec<usize> = idxs.clone();
            order.sort_by_key(|&i| !codex_raws[i].busy); // busy procs claim the freshest file first
            let mut ci = 0;
            for &i in &order {
                while ci < cand.len() && used[ci] {
                    ci += 1;
                }
                if ci < cand.len() {
                    used[ci] = true;
                    codex_session[i] = cand[ci].0.clone();
                    codex_idle[i] = (now - cand[ci].1).max(0);
                    ci += 1;
                }
            }
        }

        let mut live_codex_files: HashSet<String> = HashSet::new();
        for (i, r) in codex_raws.iter().enumerate() {
            let path = codex_session[i].clone();
            let mut usage = codex_logic::Usage::default();
            // cwd-based fallback (the disambiguated basename — Claude's identical fallback when
            // it has no session name either), overridden below once a goal/first-message fires.
            let mut label = codex_labels[i].clone();
            if !path.is_empty() {
                let is_new = !self.codex_files.contains_key(&path);
                let entry = self.codex_files.entry(path.clone()).or_default();
                if is_new {
                    entry.0 = 0; // first sight: tail from the start for the true cumulative total
                }
                let limits =
                    read_codex_new_lines(&path, &mut entry.0, &mut entry.1, &mut entry.2, now);
                usage = entry.1;
                if let Some(best) = entry.2.best() {
                    label = clip(best, 32);
                }
                live_codex_files.insert(path.clone());
                // Rate limits are account-wide, like Claude's — keep whichever snapshot came from
                // the most-recently-modified rollout (the one a live turn is actually appending to).
                // Persisted in `self` (not a local reset each poll): a `token_count` line only
                // arrives on an actual model turn, far less often than the poll cadence, so between
                // turns there's nothing fresh to see and the last known reading must stand.
                if let Some(l) = limits {
                    if let Some(mt) = file_mtime(&path) {
                        if self.codex_limits.as_ref().is_none_or(|(fm, _)| mt > *fm) {
                            self.codex_limits = Some((mt, l));
                        }
                    }
                }
            }
            // Our own busy-wall-clock accumulator — Codex reports no self-measured "API active
            // duration" the way Claude's statusline does, so this is the denominator for its tok/s.
            let acc = self.codex_active.entry(path.clone()).or_insert(0.0);
            if r.busy {
                *acc += elapsed as f64;
            }

            agents.push(RawAgent {
                label,
                session: path,
                provider: Provider::Codex,
                idle: codex_idle[i],
                working: r.working,
                cost: 0.0,
                has_cost: false,
                api_secs: *acc,
                out_tokens: usage.out_tokens,
                in_tokens: usage.in_tokens,
                branch: git_branch(&r.cwd),
                context_pct: usage.context_pct,
                con_id: codex_con_ids[i],
            });
        }
        self.codex_files.retain(|p, _| live_codex_files.contains(p));
        self.codex_active
            .retain(|p, _| live_codex_files.contains(p));

        Some(Poll {
            now,
            agents,
            limits: read_limits(now),
            codex_limits: self.codex_limits.as_ref().map(|(_, l)| l.clone()),
            focused,
        })
    }
}

// ── data (real fs) ──────────────────────────────────────────────────────────────────────────────

struct Proc {
    pid: i32,
    ppid: i32,
    comm: String,
    state: char,
    cpu: u64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn read_procs() -> (Vec<Proc>, HashMap<i32, Vec<i32>>) {
    let mut procs = Vec::new();
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (procs, children);
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        let Some(st) = parse_stat(&stat) else {
            continue;
        };
        let pid: i32 = name.parse().unwrap_or(0);
        children.entry(st.ppid).or_default().push(pid);
        procs.push(Proc {
            pid,
            ppid: st.ppid,
            comm: st.comm,
            state: st.state,
            cpu: st.cpu,
        });
    }
    (procs, children)
}

/// A process's cwd — native can read the magic symlink directly (no cap-std sandbox), so the
/// WASM build's `PWD`-from-`environ` + `--worktree` reconstruction is unnecessary here.
fn proc_cwd(pid: i32) -> String {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "?".into())
}

/// The git branch the agent's cwd is on — its **worktree** identity (RFC 0022). Handles both a
/// normal `.git` directory and a worktree's `.git` *file* (`gitdir: …`, absolute or relative). A
/// branch ref yields the branch name; a detached HEAD the short sha; `None` if not a git checkout.
fn git_branch(cwd: &str) -> Option<String> {
    let dotgit = format!("{cwd}/.git");
    let md = std::fs::symlink_metadata(&dotgit).ok()?;
    let gitdir = if md.is_dir() {
        dotgit
    } else {
        let s = std::fs::read_to_string(&dotgit).ok()?;
        let p = s.strip_prefix("gitdir:")?.trim();
        if p.starts_with('/') {
            p.to_string()
        } else {
            format!("{cwd}/{p}")
        }
    };
    let head = std::fs::read_to_string(format!("{gitdir}/HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| (!head.is_empty()).then(|| head.chars().take(7).collect()))
}

/// `(session_id, mtime)` for a cwd's transcripts, newest first.
fn transcript_sessions(cwd: &str) -> Vec<(String, i64)> {
    let dir = format!("{}/.claude/projects/{}", home(), encode_project(cwd));
    let Ok(entries) = std::fs::read_dir(&dir) else {
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
    out.sort_unstable_by_key(|&(_, m)| std::cmp::Reverse(m));
    out
}

fn transcript_files(cwd: &str, session: &str) -> Vec<String> {
    let dir = format!("{}/.claude/projects/{}", home(), encode_project(cwd));
    let mut out = vec![format!("{dir}/{session}.jsonl")];
    let subdir = format!("{dir}/{session}/subagents");
    if let Ok(entries) = std::fs::read_dir(&subdir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".jsonl") {
                out.push(format!("{subdir}/{name}"));
            }
        }
    }
    out
}

/// Strip claude's leading status glyph (the ✳ / ⠐ spinner + a space) off a terminal title, leaving
/// the task text to compare against a session name.
fn strip_status(title: &str) -> String {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

/// Whether a window task title and a session name refer to the same task — equal, or one a prefix
/// of the other (claude or the terminal may truncate either). Requires a real overlap (≥8 chars) so
/// two unrelated short titles don't collide.
fn title_matches(title: &str, name: &str) -> bool {
    let (a, b) = (title.trim(), name.trim());
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.len().min(b.len()) >= 8 && (a.starts_with(b) || b.starts_with(a))
}

fn read_session(session_id: &str) -> Option<claude_logic::Session> {
    if session_id.is_empty() {
        return None;
    }
    let data = std::fs::read_to_string(format!(
        "{}/.claude/ezbar/sessions/{session_id}.json",
        home()
    ))
    .ok()?;
    parse_session(&data)
}

fn read_limits(now: i64) -> Option<Limits> {
    let data = std::fs::read_to_string(format!("{}/.claude/ezbar-status.json", home())).ok()?;
    parse_limits(&data, now)
}

// ── Codex ────────────────────────────────────────────────────────────────────────────────────────

/// A file's mtime as an epoch-seconds integer, or `None` if it can't be stat'd.
fn file_mtime(path: &str) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Today's and yesterday's Codex rollout directories (`~/.codex/sessions/<y>/<m>/<d>`) — enough to
/// catch an overnight session without walking the whole multi-year history tree Codex keeps.
fn codex_day_dirs(now: i64) -> Vec<String> {
    let Some(today) = Local.timestamp_opt(now, 0).single() else {
        return Vec::new();
    };
    [today, today - chrono::Duration::days(1)]
        .into_iter()
        .map(|d| {
            format!(
                "{}/.codex/sessions/{:04}/{:02}/{:02}",
                home(),
                d.year(),
                d.month(),
                d.day()
            )
        })
        .collect()
}

/// Every rollout `.jsonl` file under the given day directories.
fn codex_rollout_files(dirs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".jsonl") {
                out.push(format!("{dir}/{name}"));
            }
        }
    }
    out
}

/// A rollout's `cwd`, read from just its first line (a `session_meta` event) — cheap, and the
/// result is cached by the caller since a rollout's cwd never changes.
fn codex_first_line_cwd(path: &str) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    codex_logic::session_cwd(line.trim_end())
}

/// Tail a Codex rollout incrementally, keeping only the LATEST `token_count` event's cumulative
/// usage in `*usage` (unlike Claude's transcript, these are already-summed running totals — no
/// per-message dedup needed, just "last one read wins"). Also updates `*label` — Codex's analogue
/// of Claude's `session_name` — from whichever of `thread_goal_updated`/`user_message` events
/// appear (see [`codex_logic::SessionLabel`]; neither is guaranteed present). Returns the freshest
/// rate-limit snapshot seen in this read, if any (`rate_limits` rides along on a `token_count`).
fn read_codex_new_lines(
    path: &str,
    off: &mut u64,
    usage: &mut codex_logic::Usage,
    label: &mut codex_logic::SessionLabel,
    now: i64,
) -> Option<codex_logic::Limits> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return None;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *off {
        *off = len;
    }
    if f.seek(SeekFrom::Start(*off)).is_err() {
        return None;
    }
    let mut committed = *off;
    let mut pos = *off;
    let mut line: Vec<u8> = Vec::new();
    let mut over = false;
    let mut buf = [0u8; READ_CHUNK];
    let mut fresh_limits = None;
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
                        if let Some(ev) = codex_logic::parse_token_count(s, now) {
                            *usage = ev.usage;
                            if let Some(l) = ev.limits {
                                fresh_limits = Some(l);
                            }
                        } else if let Some(g) = codex_logic::parse_goal(s) {
                            label.goal = Some(g); // latest wins — the goal can be redefined
                        } else if label.first_message.is_none() {
                            if let Some(m) = codex_logic::parse_user_message(s) {
                                label.first_message = Some(m); // first wins — it's the title
                            }
                        }
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
    fresh_limits
}

/// Read transcript lines appended past `*off`, feeding each (size-bounded) complete line to `tc`.
fn read_new_lines(path: &str, off: &mut u64, tc: &mut TokenCounter) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *off {
        *off = len; // shrank (compaction) → re-tail from the new end
    }
    if f.seek(SeekFrom::Start(*off)).is_err() {
        return;
    }
    let mut committed = *off;
    let mut pos = *off;
    let mut line: Vec<u8> = Vec::new();
    let mut over = false;
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

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}
