//! ezbar WASM plugin: `calendar` — the next meeting + countdown in the chip; **hover** opens the
//! day's agenda, and a meeting with a **Zoom** or **Google Meet** link is **click-to-join**: the
//! row hands a browser-ready join URL to `xdg-open`, so the browser opens straight onto the
//! in-meeting page — skipping Zoom's app-launch wall (Meet is web-first already). See
//! `calendar_logic::meeting`.
//!
//! It is the sandboxed replacement for the old built-in calendar module:
//!   * the secret iCal feed is read from `~/.config/ezbar/calendar_url` via an **fs** grant
//!     (mounted read-only at `/ezbar`), then fetched over the **network** grant;
//!   * wall-clock time needs the local zone, which the WASI sandbox can't see — the host hands
//!     it over via `ctx.local_timezone()` (RFC 0019) and we convert UTC (`SystemTime::now`)
//!     ourselves with chrono-tz;
//!   * joining a meeting runs `xdg-open`, the **exec** grant.
//!
//! Grant block (paste into `~/.config/ezbar/config.toml`, or run `ezbar grant calendar`):
//! ```toml
//! [modules.calendar]
//! network = ["calendar.google.com"]
//! fs = [{ path = "~/.config/ezbar", mode = "r" }]   # mounts at /ezbar
//! exec = ["xdg-open"]
//! max_memory = "8M"                                 # fixed baseline (chrono-tz), feed-independent
//! ```
//!
//! The feed (the user's whole calendar history, easily tens of MB) is **streamed** and sliced to
//! a couple of days around today as it arrives (RFC 0020), so it never lands whole in the sandbox
//! — memory is independent of feed size. The one fixed cost is the baseline: `chrono-tz` embeds
//! the whole IANA tz database (~2.5 MiB, over the 2 MiB default), so a small *fixed* `max_memory`
//! (8M) is set once and never has to grow.
//!
//! All time math happens in `update`; `view`/`popup` stay pure — they only assemble the DSL from
//! precomputed strings (no `SystemTime`, no host calls).

use std::time::{SystemTime, UNIX_EPOCH};

use calendar_logic::{parse_calendar, CalendarData, Slimmer};
use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::{Tz, UTC};
use ezbar_plugin_wasm::prelude::*;

/// Refetch the feed at most this often (seconds) — calendars move slowly and this is Google's
/// private endpoint; recomputing the countdown is cheap and happens every tick regardless.
const FETCH_INTERVAL_SECS: i64 = 300;
/// Re-tick cadence so the countdown badge stays fresh between fetches.
const TICK_MS: u32 = 30_000;
/// Shorter retry after a fetch error or while not configured.
const RETRY_MS: u32 = 60_000;

/// Where the secret iCal URL is read from inside the sandbox. The `fs` grant for
/// `~/.config/ezbar` mounts at the default guest path `/ezbar` (the host derives it from the
/// dir's basename), so the file lands at `/ezbar/calendar_url`. Override with `url_file`.
const DEFAULT_URL_FILE: &str = "/ezbar/calendar_url";

#[derive(Clone, Copy, PartialEq)]
enum RowState {
    Past,
    Ongoing,
    Soon,
    Upcoming,
    AllDay,
}

/// A render-ready agenda row, precomputed in `update` so `popup` can stay pure.
#[derive(Clone)]
struct Row {
    when: String,
    title: String,
    trailing: String,
    state: RowState,
    /// The browser join URL, if this event carries a Zoom or Google Meet link → row is clickable.
    join: Option<String>,
}

struct Calendar {
    url_file: String,
    url: String,
    tz: Tz,
    tz_loaded: bool,
    /// The feed sliced to a few days around today (KB, not the MB the server sends) — see
    /// `slim_ical`. We never keep the full feed: the sandbox memory cap can't hold it.
    slim: String,
    last_fetch: i64,
    configured: bool,
    loaded: bool,

    // ── render model (built in update, consumed by the pure view/popup) ──
    has_next: bool,
    is_urgent: bool,
    is_overdue: bool,
    chip_title: String,
    chip_badge: String,
    header_date: String,
    header_clock: String,
    rows: Vec<Row>,
    /// Index in `rows` before which to draw the "now" divider, if any.
    marker_at: Option<usize>,
}

impl Default for Calendar {
    fn default() -> Self {
        Calendar {
            url_file: DEFAULT_URL_FILE.to_string(),
            url: String::new(),
            tz: UTC,
            tz_loaded: false,
            slim: String::new(),
            last_fetch: 0,
            configured: false,
            loaded: false,
            has_next: false,
            is_urgent: false,
            is_overdue: false,
            chip_title: String::new(),
            chip_badge: String::new(),
            header_date: String::new(),
            header_clock: String::new(),
            rows: Vec::new(),
            marker_at: None,
        }
    }
}

impl Plugin for Calendar {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            if k == "url_file" {
                self.url_file = v.clone();
            }
        }
        self.read_url_file();
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            Event::Timer => {
                if !self.tz_loaded {
                    self.tz = ctx.local_timezone().parse::<Tz>().unwrap_or(UTC);
                    self.tz_loaded = true;
                }
                if self.url.is_empty() {
                    self.read_url_file();
                }
                if self.url.is_empty() {
                    // Not configured: a quiet chip + a setup hint in the popup. Keep polling the
                    // file so dropping it in later is picked up without a reload.
                    self.configured = false;
                    self.loaded = true;
                    self.recompute();
                    ctx.set_timeout(RETRY_MS);
                    return true;
                }
                self.configured = true;

                let now = now_secs();
                let mut ok = true;
                if self.slim.is_empty() || now - self.last_fetch >= FETCH_INTERVAL_SECS {
                    // Stream the feed and slim it to today's window as it arrives (RFC 0020): the
                    // full multi-MB body never lands in the 2 MiB sandbox, so no OOM and no creep.
                    match self.fetch_slim(ctx) {
                        Ok(slim) => {
                            self.slim = slim;
                            self.last_fetch = now;
                        }
                        Err(e) => {
                            ctx.log(&format!("calendar: fetch failed: {e}"));
                            ok = false;
                        }
                    }
                }
                self.recompute();
                self.loaded = true;
                ctx.set_timeout(if ok { TICK_MS } else { RETRY_MS });
                true
            }
            // Click a joinable agenda row → open its meeting (Zoom web-client / Meet) in the browser.
            Event::Pointer {
                id,
                kind: PointerKind::Press,
                ..
            } => {
                if let Some(url) = id
                    .strip_prefix("event-")
                    .and_then(|s| s.parse::<usize>().ok())
                    .and_then(|i| self.rows.get(i))
                    .and_then(|r| r.join.clone())
                {
                    if let Err(e) = ctx.exec("xdg-open", &[url.as_str()], None) {
                        ctx.log(&format!("calendar: xdg-open failed: {e}"));
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn view(&self) -> Render {
        if !self.loaded {
            return row([
                Icon::Calendar.view(14.0, Token::FgDim),
                text("\u{2026}").color(Token::FgDim),
            ])
            .spacing(6.0);
        }
        if !self.has_next {
            // Nothing upcoming. If today still had meetings (all now in the past), surface a "✓ N"
            // so the day's agenda is an obvious hover away rather than a bare glyph that reads as
            // "empty/broken". Only a genuinely empty day falls back to the quiet glyph.
            let n = self.rows.len();
            if n == 0 {
                return Icon::Calendar.view(14.0, Token::FgDim);
            }
            return row([
                Icon::Calendar.view(14.0, Token::FgDim),
                text(format!("\u{2713} {n}")).color(Token::FgDim),
            ])
            .spacing(6.0);
        }
        let accent = if self.is_overdue {
            Token::Urgent
        } else if self.is_urgent {
            Token::Warn
        } else {
            Token::Accent
        };
        let title_color = if self.is_overdue {
            Token::Urgent
        } else if self.is_urgent {
            Token::Warn
        } else {
            Token::Fg
        };
        row([
            Icon::Calendar.view(14.0, Token::FgDim),
            text(self.chip_title.clone()).color(title_color),
            text(self.chip_badge.clone()).color(accent),
        ])
        .spacing(6.0)
    }

    fn popup(&self) -> Option<Render> {
        let body = if !self.configured {
            empty_state(
                "Calendar not set up",
                Some("Save your secret iCal URL to\n~/.config/ezbar/calendar_url"),
            )
        } else if self.rows.is_empty() {
            empty_state("No meetings today", None)
        } else {
            let n = self.rows.len();
            let mut items: Vec<Render> = Vec::with_capacity(n + 1);
            for (i, r) in self.rows.iter().enumerate() {
                if self.marker_at == Some(i) {
                    items.push(now_marker(&self.header_clock));
                }
                items.push(render_row(i, r));
            }
            column(items).spacing(4.0)
        };

        let evword = if self.rows.len() == 1 {
            "event"
        } else {
            "events"
        };
        let header = row([
            text(self.header_date.clone()).size(15.0).color(Token::Fg),
            text(format!("\u{b7}  {} {}", self.rows.len(), evword))
                .size(13.0)
                .color(Token::FgDim),
            text(self.header_clock.clone())
                .size(15.0)
                .color(Token::Accent),
        ])
        .spacing(10.0)
        .align(Align::Center);

        Some(container(column([header, divider(), body]).spacing(10.0)).padding(14.0))
    }
}

impl Calendar {
    /// Read the secret iCal URL out of the fs-mounted file (best-effort; empty if absent/denied).
    fn read_url_file(&mut self) {
        if let Ok(s) = std::fs::read_to_string(&self.url_file) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                self.url = trimmed.to_string();
            }
        }
    }

    /// Stream the iCal feed and slim it to today's window *as it arrives* (RFC 0020). We pull the
    /// body in 64 KiB chunks and push each through the [`Slimmer`], so the multi-MB feed never
    /// lands whole in the 2 MiB sandbox — only the KB-sized window survives. Returns the slim feed.
    fn fetch_slim(&self, ctx: &mut dyn Ctx) -> Result<String, String> {
        let today = now_in(self.tz).date_naive();
        let mut slimmer = Slimmer::new(today, 1, 2);
        let handle = ctx.http_open(&self.url)?;
        loop {
            match ctx.http_read(handle, 64 * 1024) {
                Ok(chunk) if chunk.is_empty() => break, // end of stream (host already closed it)
                Ok(chunk) => slimmer.push(&chunk),
                Err(e) => {
                    ctx.http_close(handle);
                    return Err(e);
                }
            }
        }
        ctx.http_close(handle); // idempotent
        Ok(slimmer.finish())
    }

    /// Rebuild the render model from the cached feed at the current instant.
    fn recompute(&mut self) {
        let now = now_in(self.tz);
        let data: CalendarData = if self.slim.is_empty() {
            CalendarData::default()
        } else {
            parse_calendar(&self.slim, now)
        };

        self.has_next = data.has_next;
        self.is_urgent = data.is_urgent;
        self.is_overdue = data.is_overdue;
        self.chip_title = truncate(&data.next_title, 24);
        self.chip_badge = if data.time_until_next == "ongoing" {
            "now".to_string()
        } else {
            data.time_until_next.clone()
        };
        self.header_date = now.format("%A, %B %-d").to_string();
        self.header_clock = now.format("%H:%M").to_string();

        let mut rows: Vec<Row> = Vec::new();
        // All-day events first (a labelled chip row), like the native module.
        for ev in data.today_events.iter().filter(|e| e.is_all_day) {
            rows.push(Row {
                when: "All day".to_string(),
                title: truncate(&ev.title, 40),
                trailing: String::new(),
                state: RowState::AllDay,
                join: ev.join_url.clone(),
            });
        }
        // Then timed events in chronological order, with a "now" marker before the first future
        // one (only once a prior timed event has been shown — matches the native popup).
        let all_day_count = rows.len();
        let mut marker_at = None;
        let mut shown = false;
        for (i, ev) in data
            .today_events
            .iter()
            .filter(|e| !e.is_all_day)
            .enumerate()
        {
            if marker_at.is_none() && shown && ev.start > now {
                marker_at = Some(all_day_count + i);
            }
            let state = if now >= ev.end {
                RowState::Past
            } else if now >= ev.start {
                RowState::Ongoing
            } else if ev.start - now <= Duration::minutes(15) {
                RowState::Soon
            } else {
                RowState::Upcoming
            };
            let when = format!(
                "{} \u{2013} {}",
                ev.start.format("%H:%M"),
                ev.end.format("%H:%M")
            );
            let trailing = if ev.start > now {
                rel(ev.start - now)
            } else if state == RowState::Ongoing {
                format!("ends {}", rel(ev.end - now))
            } else {
                String::new()
            };
            rows.push(Row {
                when,
                title: truncate(&ev.title, 36),
                trailing,
                state,
                join: ev.join_url.clone(),
            });
            shown = true;
        }
        self.rows = rows;
        self.marker_at = marker_at;
    }
}

/// One agenda row. Joinable rows are wrapped in a `mouse_area` (whole row is the click target)
/// and carry an accent "Join ↗" affordance; the row click reaches `update` as `Pointer{Press}`.
fn render_row(i: usize, r: &Row) -> Render {
    let (dot_c, title_c) = match r.state {
        RowState::Past => (Token::FgDim, Token::FgDim),
        RowState::Ongoing => (Token::Ok, Token::Ok),
        RowState::Soon => (Token::Warn, Token::Warn),
        RowState::Upcoming => (Token::Accent, Token::Fg),
        RowState::AllDay => (Token::Accent, Token::Fg),
    };
    let time_c = if r.state == RowState::Ongoing {
        Token::Ok
    } else {
        Token::FgDim
    };
    let mut cells = vec![
        Icon::Dot.view(8.0, dot_c),
        text(r.when.clone()).size(13.0).color(time_c),
        text(r.title.clone()).size(13.0).color(title_c),
    ];
    if !r.trailing.is_empty() {
        cells.push(text(r.trailing.clone()).size(12.0).color(Token::FgDim));
    }
    if r.join.is_some() {
        cells.push(text("Join \u{2197}").size(12.0).color(Token::Accent));
    }
    let content = row(cells).spacing(10.0).align(Align::Center);
    if r.join.is_some() {
        mouse_area(format!("event-{i}"), content)
    } else {
        content
    }
}

/// The "now" divider between finished and upcoming events.
fn now_marker(clock: &str) -> Render {
    row([
        text(format!("now {clock}")).size(11.0).color(Token::Accent),
        text("\u{2500}".repeat(28)).size(7.0).color(Token::Accent),
    ])
    .spacing(8.0)
    .align(Align::Center)
}

fn divider() -> Render {
    text("\u{2500}".repeat(48)).size(7.0).color(Token::FgDim)
}

fn empty_state(line: &str, hint: Option<&str>) -> Render {
    let mut inner = vec![
        Icon::Calendar.view(30.0, Token::FgDim),
        text(line.to_string()).size(14.0).color(Token::Fg),
    ];
    if let Some(h) = hint {
        inner.push(text(h.to_string()).size(12.0).color(Token::FgDim));
    }
    container(column(inner).spacing(10.0).align(Align::Center)).padding(8.0)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_in(tz: Tz) -> DateTime<Tz> {
    Utc.timestamp_opt(now_secs(), 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().expect("epoch is valid"))
        .with_timezone(&tz)
}

/// Human "in 9m" / "in 2h" / "in 1h30m" for a positive duration.
fn rel(d: Duration) -> String {
    let mins = d.num_minutes().max(0);
    if mins < 60 {
        format!("in {mins}m")
    } else {
        let (h, m) = (mins / 60, mins % 60);
        if m > 0 {
            format!("in {h}h{m}m")
        } else {
            format!("in {h}h")
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars[..max.saturating_sub(1)].iter().collect();
    out.push('\u{2026}');
    out
}

export_plugin!(Calendar);
