//! `clock` module: the date/time. `[modules.clock] format = "%Y-%m-%d %H:%M:%S"`
//! (chrono strftime).
//!
//! Scheduling is **event-driven, not a poll** (RFC 0016): the rendered string only
//! changes at the boundary of the finest field in `format`, so the stream sleeps to the
//! *next boundary* of that field instead of waking every second. One wake per visible
//! change, recomputed from the real clock each time → drift-free. A `MAX_IDLE` cap turns
//! the long sleeps (minute/hour/day formats) into a cheap watchdog that catches
//! suspend/resume and NTP steps without a logind dependency (`tokio` sleeps on
//! `CLOCK_MONOTONIC`, which *freezes* across suspend).

use std::time::Duration;

use chrono::{
    DateTime, Datelike, Local, LocalResult, Months, NaiveDate, NaiveDateTime, TimeZone, Timelike,
    Weekday,
};
use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::mouse::Interaction;
use ezbar_plugin::iced::widget::{column, container, mouse_area, row, text, Space};
use ezbar_plugin::iced::{Background, Border, Color, Element, Length, Subscription};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

/// Watchdog cap on a single sleep (RFC 0016 §3.2). Bounds post-suspend / post-NTP-step
/// staleness to this for *any* format, at ≤2 redundant (no-op, deduped) wakes/min.
const MAX_IDLE: Duration = Duration::from_secs(30);

/// How the calendar popup is triggered (RFC 0016 §4). `Hover` = a display-only glance card;
/// `Click` = the sticky, navigable month grid; `Off` = bare chip, no popup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Hover,
    Click,
    Off,
}

enum Msg {
    Tick(String),
    HoverEnter,
    HoverLeave,
    Open,
    PrevMonth,
    NextMonth,
    Today,
}

pub struct Clock {
    instance: u64,
    format: String,
    popup_format: String,
    text: String,
    trigger: Trigger,
    week_numbers: bool,
    sunday_first: bool,
    /// First day of the month the click-grid is showing; `‹ ›` shift it, `Today` resets it.
    shown_month: NaiveDate,
}

impl Clock {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let s = |k: &str| cfg.get(k).and_then(|v| v.as_str());
        let format = s("format").unwrap_or("%Y-%m-%d %H:%M:%S").to_string();
        let popup_format = s("popup_format").unwrap_or("%A, %B %-d %Y").to_string();
        let trigger = match s("calendar").unwrap_or("hover") {
            "click" => Trigger::Click,
            "off" => Trigger::Off,
            _ => Trigger::Hover,
        };
        let week_numbers = cfg
            .get("week_numbers")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let sunday_first = matches!(s("first_day"), Some("sunday"));
        Clock {
            instance,
            format,
            popup_format,
            text: String::new(),
            trigger,
            week_numbers,
            sunday_first,
            shown_month: first_of_month(Local::now().date_naive()),
        }
    }
}

impl Module for Clock {
    fn id(&self) -> &str {
        "clock"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with((self.instance, self.format.clone()), clock_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Tick(s)) => {
                self.text = s.clone();
                Response::none()
            }
            Some(Msg::HoverEnter) => Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::HoverLeave) => Response::request(HostRequest::ClosePopup),
            Some(Msg::Open) => {
                // each fresh open starts on the current month (nav state doesn't linger).
                self.shown_month = first_of_month(Local::now().date_naive());
                Response::request(HostRequest::OpenPopup(PopupMode::Click))
            }
            Some(Msg::PrevMonth) => {
                self.shown_month = self
                    .shown_month
                    .checked_sub_months(Months::new(1))
                    .unwrap_or(self.shown_month);
                Response::none()
            }
            Some(Msg::NextMonth) => {
                self.shown_month = self
                    .shown_month
                    .checked_add_months(Months::new(1))
                    .unwrap_or(self.shown_month);
                Response::none()
            }
            Some(Msg::Today) => {
                self.shown_month = first_of_month(Local::now().date_naive());
                Response::none()
            }
            None => Response::none(),
        }
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        text(self.text.clone()).into()
    }

    fn hover_messages(&self) -> Option<(ModMsg, ModMsg)> {
        (self.trigger == Trigger::Hover)
            .then(|| (ModMsg::new(Msg::HoverEnter), ModMsg::new(Msg::HoverLeave)))
    }

    fn click_message(&self) -> Option<ModMsg> {
        (self.trigger == Trigger::Click).then(|| ModMsg::new(Msg::Open))
    }

    fn popup_size(&self) -> Option<(u32, u32)> {
        // grid width + the host's 12px popup padding on each side + a little breathing room.
        let w = (self.grid_w() + 40.0) as u32;
        match self.trigger {
            Trigger::Click => Some((w, 320)),
            Trigger::Hover => Some((w, 184)),
            Trigger::Off => None,
        }
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let pal = Pal::from_ctx(ctx);
        match self.trigger {
            Trigger::Off => None,
            Trigger::Hover => Some(self.glance(pal)),
            Trigger::Click => Some(self.month_view(pal)),
        }
    }
}

// ---- calendar popup (RFC 0016 §4 / §6.1) ----

const CELL: f32 = 30.0; // square day-cell side
const DISC: f32 = 24.0; // today-marker disc diameter
const WK_W: f32 = 24.0; // ISO week-number column width

impl Clock {
    /// Content width: the 7 day columns + the optional week-number column. Both popups are
    /// constrained to this and centered, so they don't left-hug a wider surface.
    fn grid_w(&self) -> f32 {
        7.0 * CELL + if self.week_numbers { WK_W } else { 0.0 }
    }

    /// The sticky click popup: a navigable month grid (fixed 6 rows so the popup never
    /// changes height on `‹ ›`), ISO week-number column, today disc, alpha-recessive
    /// weekend / adjacent-month days.
    fn month_view(&self, pal: Pal) -> Element<'_, ModMsg> {
        let today = Local::now().date_naive();
        let shown = self.shown_month;

        // ‹  JUNE 2026  ›  — clicking the label jumps back to today.
        let label = mouse_area(
            text(shown.format("%B %Y").to_string().to_uppercase())
                .size(14)
                .color(pal.fg),
        )
        .interaction(Interaction::Pointer)
        .on_press(ModMsg::new(Msg::Today));
        let header = row![
            chev("\u{2039}", Msg::PrevMonth, pal),
            Space::new().width(Length::Fill),
            label,
            Space::new().width(Length::Fill),
            chev("\u{203A}", Msg::NextMonth, pal),
        ]
        .align_y(Vertical::Center);

        // weekday initials, aligned to the day columns (week-# spacer first).
        let mut hdr: Vec<Element<ModMsg>> = Vec::new();
        if self.week_numbers {
            hdr.push(container(Space::new()).width(Length::Fixed(WK_W)).into());
        }
        for wd in weekday_order(self.sunday_first) {
            hdr.push(
                container(text(weekday_initial(wd)).size(11).color(pal.dim))
                    .center_x(Length::Fixed(CELL))
                    .into(),
            );
        }

        // grid: start on the configured first-day-of-week of the week containing the 1st.
        let back = if self.sunday_first {
            shown.weekday().num_days_from_sunday()
        } else {
            shown.weekday().num_days_from_monday()
        } as i64;
        let grid_start = shown - chrono::Duration::days(back);
        let mut weeks: Vec<Element<ModMsg>> = Vec::new();
        for r in 0..6 {
            let row_start = grid_start + chrono::Duration::days(7 * r);
            let mut cells: Vec<Element<ModMsg>> = Vec::new();
            if self.week_numbers {
                // ISO weeks are Monday-defined — key the column off the row's Monday.
                let monday = row_start + chrono::Duration::days(days_to_monday(row_start));
                cells.push(
                    container(
                        text(format!("{:02}", monday.iso_week().week()))
                            .size(11)
                            .color(pal.dim),
                    )
                    .center_x(Length::Fixed(WK_W))
                    .center_y(Length::Fixed(CELL))
                    .into(),
                );
            }
            for d in 0..7 {
                let date = row_start + chrono::Duration::days(d);
                let in_month = date.month() == shown.month() && date.year() == shown.year();
                cells.push(day_cell(date, today, in_month, pal));
            }
            weeks.push(row(cells).align_y(Vertical::Center).into());
        }

        let grid = column![header, rule(pal.sep, 1.0), row(hdr), column(weeks)]
            .spacing(6)
            .width(Length::Fixed(self.grid_w()));
        container(grid).center_x(Length::Fill).into()
    }

    /// The hover glance card: long date, ISO week + day-of-year, and a static current-week
    /// strip (today disced). Display-only — no chevrons (a hover surface can't honor them).
    fn glance(&self, pal: Pal) -> Element<'_, ModMsg> {
        let now = Local::now();
        let date = now.date_naive();
        let long = text(now.format(&self.popup_format).to_string())
            .size(15)
            .color(pal.fg);
        let meta = text(format!(
            "ISO week {:02} \u{00b7} day {} of {}",
            date.iso_week().week(),
            date.ordinal(),
            days_in_year(date.year())
        ))
        .size(12)
        .color(pal.dim);

        let back = if self.sunday_first {
            date.weekday().num_days_from_sunday()
        } else {
            date.weekday().num_days_from_monday()
        } as i64;
        let week_start = date - chrono::Duration::days(back);
        let mut hdr: Vec<Element<ModMsg>> = Vec::new();
        let mut days: Vec<Element<ModMsg>> = Vec::new();
        for (i, wd) in weekday_order(self.sunday_first).into_iter().enumerate() {
            hdr.push(
                container(text(weekday_initial(wd)).size(11).color(pal.dim))
                    .center_x(Length::Fixed(CELL))
                    .into(),
            );
            let d = week_start + chrono::Duration::days(i as i64);
            days.push(day_cell(d, date, true, pal));
        }

        let card = column![
            long,
            meta,
            rule(pal.sep, 1.0),
            row(hdr),
            row(days).align_y(Vertical::Center)
        ]
        .spacing(6)
        .width(Length::Fixed(self.grid_w()));
        container(card).center_x(Length::Fill).into()
    }
}

/// One day cell: a fixed square so columns align. Today = an `accent` disc with
/// luminance-picked cut-out ink (legible on any theme). Otherwise alpha-recessive:
/// adjacent-month ghosted, weekend dimmed, in-month weekday at full `fg`.
fn day_cell<'a>(
    date: NaiveDate,
    today: NaiveDate,
    in_month: bool,
    pal: Pal,
) -> Element<'a, ModMsg> {
    let inner: Element<ModMsg> = if date == today {
        container(text(format!("{}", date.day())).size(13).color(pal.ink))
            .center_x(Length::Fixed(DISC))
            .center_y(Length::Fixed(DISC))
            .style(move |_| container::Style {
                background: Some(Background::Color(pal.accent)),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: (DISC / 2.0).into(),
                },
                text_color: Some(pal.ink),
                ..Default::default()
            })
            .into()
    } else {
        let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
        let c = if !in_month {
            Color { a: 0.28, ..pal.fg }
        } else if weekend {
            Color { a: 0.55, ..pal.fg }
        } else {
            pal.fg
        };
        text(format!("{}", date.day())).size(13).color(c).into()
    };
    container(inner)
        .center_x(Length::Fixed(CELL))
        .center_y(Length::Fixed(CELL))
        .into()
}

/// A clickable chevron with a comfortable hit target.
fn chev<'a>(glyph: &str, msg: Msg, pal: Pal) -> Element<'a, ModMsg> {
    mouse_area(
        container(text(glyph.to_string()).size(18).color(pal.dim))
            .center_x(Length::Fixed(28.0))
            .center_y(Length::Fixed(28.0)),
    )
    .interaction(Interaction::Pointer)
    .on_press(ModMsg::new(msg))
    .into()
}

/// A full-width hairline.
fn rule<'a>(color: Color, h: f32) -> Element<'a, ModMsg> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(h))
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            ..Default::default()
        })
        .into()
}

/// Theme colors copied out of `Ctx` so leaf helpers can own them (`Color: Copy`).
#[derive(Clone, Copy)]
struct Pal {
    fg: Color,
    dim: Color,
    accent: Color,
    sep: Color,
    ink: Color,
}

impl Pal {
    fn from_ctx(ctx: &Ctx) -> Self {
        let accent = ctx.accent();
        Pal {
            fg: ctx.fg(),
            dim: ctx.fg_dim(),
            accent,
            sep: ctx.sep(),
            ink: ink_for(accent),
        }
    }
}

/// Luminance-aware cut-out ink: dark on a light accent, light on a dark accent — so the
/// today-disc number is legible on ANY theme (vs. a hardcoded dark RGB that turns to mud on
/// a pastel light theme). RFC 0016 §6.2.
fn ink_for(bg: Color) -> Color {
    let l = 0.2126 * bg.r + 0.7152 * bg.g + 0.0722 * bg.b;
    if l > 0.55 {
        Color::from_rgb(0.08, 0.08, 0.10)
    } else {
        Color::from_rgb(0.97, 0.97, 0.99)
    }
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// Days to advance from `d` to reach the Monday of its ISO week (0 if `d` is Monday).
fn days_to_monday(d: NaiveDate) -> i64 {
    ((7 - d.weekday().num_days_from_monday()) % 7) as i64
}

fn days_in_year(y: i32) -> u32 {
    NaiveDate::from_ymd_opt(y, 12, 31)
        .map(|d| d.ordinal())
        .unwrap_or(365)
}

fn weekday_order(sunday_first: bool) -> [Weekday; 7] {
    if sunday_first {
        [
            Weekday::Sun,
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
        ]
    } else {
        [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ]
    }
}

fn weekday_initial(wd: Weekday) -> &'static str {
    match wd {
        Weekday::Mon => "M",
        Weekday::Tue => "T",
        Weekday::Wed => "W",
        Weekday::Thu => "T",
        Weekday::Fri => "F",
        Weekday::Sat => "S",
        Weekday::Sun => "S",
    }
}

fn clock_stream(data: &(u64, String)) -> impl Stream<Item = ModMsg> {
    let fmt = data.1.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            // The finest field in `fmt` decides the wake cadence; computed once (the format
            // doesn't change for the life of the stream).
            let unit = granularity(&fmt);
            // Re-render only when the *rendered* string changes, never on a wake that
            // produces the identical text — so the watchdog wakes (minute/hour/day formats)
            // cost a `now()`+format+strcmp and nothing more.
            let mut last: Option<String> = None;
            loop {
                let now = Local::now();
                let s = now.format(&fmt).to_string();
                if last.as_deref() != Some(s.as_str()) {
                    last = Some(s.clone());
                    if out.send(ModMsg::new(Msg::Tick(s))).await.is_err() {
                        break;
                    }
                }
                let wait = until_next_boundary(unit, now).min(MAX_IDLE);
                tokio::time::sleep(wait).await;
            }
        },
    )
}

/// The finest time unit a `format` actually renders — the cadence at which its output can
/// change. Drives the boundary the clock sleeps to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Sub,
    Sec,
    Min,
    Hour,
    Day,
}

/// Scan a chrono strftime `format` for its finest field (RFC 0016 §9). A real specifier
/// walk, not `contains`: `%%` is an escaped literal (skipped), and composite specifiers
/// that *include* seconds (`%T %r %c %X %+ %s`) are classified `Sec`, not by their letter.
/// An unknown specifier defaults to `Sec` — a wasted 1 Hz wake is cheap; a seconds field
/// that updates once a minute is a bug.
fn granularity(fmt: &str) -> Unit {
    fn finest(a: Unit, b: Unit) -> Unit {
        // ordering: Sub < Sec < Min < Hour < Day; keep the smaller (finer).
        let rank = |u: Unit| match u {
            Unit::Sub => 0,
            Unit::Sec => 1,
            Unit::Min => 2,
            Unit::Hour => 3,
            Unit::Day => 4,
        };
        if rank(a) <= rank(b) {
            a
        } else {
            b
        }
    }
    let mut best = Unit::Day;
    let bytes = fmt.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        // a '%' specifier: skip flags/width/precision (`-_0^#:.` and digits), then read the
        // final letter. `%%` is a literal percent — no field.
        i += 1;
        while i < bytes.len()
            && matches!(
                bytes[i],
                b'-' | b'_' | b'0' | b'^' | b'#' | b':' | b'.' | b'0'..=b'9'
            )
        {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let spec = bytes[i];
        i += 1;
        let u = match spec {
            b'%' => continue,                                            // escaped literal
            b'f' => Unit::Sub,                                           // sub-second
            b'S' | b'T' | b'r' | b'c' | b'X' | b'+' | b's' => Unit::Sec, // include seconds
            b'M' | b'R' => Unit::Min,
            b'H' | b'I' | b'k' | b'l' | b'p' | b'P' => Unit::Hour, // hour / AM-PM (12h bound)
            _ => Unit::Day,                                        // date fields & everything else
        };
        best = finest(best, u);
    }
    best
}

/// Duration from `now` until the next start-of-`unit`, drift-free (recomputed from the real
/// clock each call) and panic-free across DST (RFC 0016 §3.1). Lands ~1 ms *past* the
/// boundary so timer slop can't wake us a hair early into the old unit.
///
/// `Sec`/`Min` use epoch-nanosecond arithmetic — every modern UTC offset is a whole number
/// of minutes, so UTC second/minute boundaries coincide with local ones; no timezone
/// conversion, no panic. `Hour`/`Day` need local civil time (fractional-offset zones:
/// +5:30, +5:45, +12:45) and resolve DST gaps/ambiguity explicitly. In practice
/// `Hour`/`Day` are clamped to `MAX_IDLE` except in the final ~30 s before the boundary.
fn until_next_boundary(unit: Unit, now: DateTime<Local>) -> Duration {
    let slop = Duration::from_millis(1);
    let sub_ns = now.timestamp_subsec_nanos() as u64;
    match unit {
        Unit::Sub => {
            // ~10 Hz floor (RFC 0011 MIN_TIMER spirit): a status bar never needs faster.
            const STEP: u64 = 100_000_000;
            Duration::from_nanos(STEP - sub_ns % STEP)
        }
        Unit::Sec => {
            // next whole second
            Duration::from_nanos(1_000_000_000u64.saturating_sub(sub_ns).max(1)) + slop
        }
        Unit::Min => {
            // next whole minute: (60 - secs_into_minute)*1e9 - sub_ns
            let secs = now.timestamp().rem_euclid(60) as u64;
            let ns = ((60 - secs) * 1_000_000_000).saturating_sub(sub_ns).max(1);
            Duration::from_nanos(ns) + slop
        }
        Unit::Hour | Unit::Day => {
            let naive = now.naive_local();
            let next: NaiveDateTime = if matches!(unit, Unit::Hour) {
                // truncate to the hour, +1h (carries date over midnight)
                naive
                    .date()
                    .and_hms_opt(naive.hour(), 0, 0)
                    .unwrap_or(naive)
                    + chrono::Duration::hours(1)
            } else {
                // next local midnight
                (now.date_naive() + chrono::Duration::days(1))
                    .and_hms_opt(0, 0, 0)
                    .unwrap_or(naive)
            };
            let at = resolve_local(next, now);
            (at - now).to_std().unwrap_or(MAX_IDLE) + slop
        }
    }
}

/// Convert a naive local boundary back to an instant, resolving DST edges instead of
/// panicking: a fall-back ambiguous time takes the earliest occurrence; a spring-forward
/// gap (the boundary doesn't exist locally) nudges forward to the first valid instant.
fn resolve_local(nd: NaiveDateTime, after: DateTime<Local>) -> DateTime<Local> {
    match Local.from_local_datetime(&nd) {
        LocalResult::Single(t) => t,
        LocalResult::Ambiguous(earliest, _) => earliest,
        LocalResult::None => {
            let mut probe = nd;
            for _ in 0..6 {
                probe += chrono::Duration::hours(1);
                match Local.from_local_datetime(&probe) {
                    LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => return t,
                    LocalResult::None => continue,
                }
            }
            after + chrono::Duration::days(1) // unreachable in practice; safe fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64, nanos: u32) -> DateTime<Local> {
        DateTime::from_timestamp(secs, nanos)
            .unwrap()
            .with_timezone(&Local)
    }

    #[test]
    fn granularity_classifies_finest_field() {
        assert!(matches!(granularity("%H:%M:%S"), Unit::Sec));
        assert!(matches!(granularity("%H:%M"), Unit::Min));
        assert!(matches!(granularity("%H"), Unit::Hour));
        assert!(matches!(granularity("%Y-%m-%d"), Unit::Day));
        // composites that *contain* seconds are Sec, not classified by their letter:
        assert!(matches!(granularity("%T"), Unit::Sec));
        assert!(matches!(granularity("%r"), Unit::Sec));
        assert!(matches!(granularity("%c"), Unit::Sec));
        assert!(matches!(granularity("%+"), Unit::Sec));
        // padding/precision flags don't fool the scanner:
        assert!(matches!(granularity("%-H:%M"), Unit::Min));
        assert!(matches!(granularity("%.3f"), Unit::Sub));
        // an escaped %% is a literal percent, not a seconds field:
        assert!(matches!(granularity("100%% load"), Unit::Day));
        assert!(matches!(granularity("%%S"), Unit::Day));
        // finest wins regardless of order:
        assert!(matches!(granularity("%A %H:%M:%S"), Unit::Sec));
    }

    #[test]
    fn minute_boundary_is_not_off_by_one() {
        // exactly on a minute boundary (ts % 60 == 0), no sub-second → next minute is ~60s
        // away, NOT ~61s (the v1 overshoot bug).
        let w = until_next_boundary(Unit::Min, at(1_700_000_040, 0)); // 1_700_000_040 % 60 == 0
        assert!(
            w.as_millis() >= 60_000 && w.as_millis() <= 60_010,
            "expected ~60s, got {w:?}"
        );
    }

    #[test]
    fn minute_boundary_midway() {
        // 30s + 0.25s into the minute → 29.75s to the next minute.
        let w = until_next_boundary(Unit::Min, at(1_700_000_040 + 30, 250_000_000));
        let ms = w.as_millis();
        assert!(
            (29_740..=29_760).contains(&ms),
            "expected ~29.75s, got {ms}ms"
        );
    }

    #[test]
    fn second_boundary() {
        // 0.5s into a second → 0.5s to the next, plus ~1ms slop.
        let w = until_next_boundary(Unit::Sec, at(1_700_000_000, 500_000_000));
        let ms = w.as_millis();
        assert!((500..=502).contains(&ms), "expected ~500ms, got {ms}ms");
    }

    #[test]
    fn boundary_never_zero_or_negative() {
        // a hair before the boundary must still yield a positive wait (never 0 → busy loop).
        for sub in [0u32, 1, 999_999_999] {
            for u in [Unit::Sec, Unit::Min] {
                assert!(until_next_boundary(u, at(1_700_000_000, sub)) > Duration::ZERO);
            }
        }
    }
}
