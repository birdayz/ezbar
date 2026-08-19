//! Pure, host-testable logic for the ezbar `calendar` WASM plugin.
//!
//! Parses a Google-style secret-iCal (RFC 5545) feed into *today's* events, derives the
//! next-meeting / countdown / urgency the chip shows, and (via [`zoom`]) turns each event's text
//! into a one-click web-client Zoom join URL.
//!
//! Everything is parameterized by an explicit `now: DateTime<Tz>` — the plugin gets UTC from
//! `SystemTime::now()` and the zone from the host (`local-timezone`, RFC 0019), since the WASI
//! sandbox has no local clock zone. No I/O, no `Local`, no system clock → it unit-tests on the
//! host even though the plugin itself only builds for `wasm32-wasip2`.
//!
//! Recurring events (`RRULE`) are expanded for the in-window occurrences (see [`expand_rrule`]):
//! a weekly 1:1 only has a concrete `VEVENT` in the feed for the day it was first created (or a
//! day some instance was rescheduled) — every other occurrence exists purely as the `RRULE`, so
//! without expansion it silently never appears once its literal `DTSTART` ages out of the window.

use std::collections::HashSet;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{RRuleSet, Tz as RTz};

pub mod meeting;
pub use meeting::{best_meeting, join_url, zoom_join_url, ZoomMeeting};

/// One concrete event in today's window, with times in the display timezone.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
    pub is_all_day: bool,
    pub location: String,
    /// A fully-assembled Zoom web-client join URL derived from the event text, if any. This is
    /// what the chip's clickable row hands to `xdg-open`.
    pub join_url: Option<String>,
}

/// The chip/popup model: today's events plus the derived "next meeting" summary.
#[derive(Debug, Clone, Default)]
pub struct CalendarData {
    pub today_events: Vec<CalendarEvent>,
    pub has_next: bool,
    pub next_title: String,
    pub display_text: String,
    pub time_until_next: String,
    pub is_urgent: bool,
    pub is_overdue: bool,
}

fn truncate_title(title: &str, max_len: usize) -> String {
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= max_len {
        return title.to_string();
    }
    let mut s: String = chars[..max_len - 2].iter().collect();
    s.push_str("..");
    s
}

fn prop<'a>(
    ev: &'a ical::parser::ical::component::IcalEvent,
    name: &str,
) -> Option<&'a ical::property::Property> {
    ev.properties.iter().find(|p| p.name == name)
}

fn prop_value(ev: &ical::parser::ical::component::IcalEvent, name: &str) -> String {
    prop(ev, name)
        .and_then(|p| p.value.clone())
        .unwrap_or_default()
}

fn param_has(p: &ical::property::Property, key: &str, val: &str) -> bool {
    if let Some(params) = &p.params {
        for (k, vs) in params {
            if k.eq_ignore_ascii_case(key) && vs.iter().any(|v| v.eq_ignore_ascii_case(val)) {
                return true;
            }
        }
    }
    false
}

/// First value of parameter `key` (e.g. `TZID`), surrounding quotes stripped.
fn param_value<'a>(p: &'a ical::property::Property, key: &str) -> Option<&'a str> {
    let params = p.params.as_ref()?;
    for (k, vs) in params {
        if k.eq_ignore_ascii_case(key) {
            return vs.first().map(|s| s.trim_matches('"'));
        }
    }
    None
}

/// Resolve a wall-clock `NaiveDateTime` in zone `tz`, picking the earlier instant across a DST
/// fold (deterministic) rather than dropping the event when the local time is ambiguous.
fn resolve(tz: Tz, ndt: &NaiveDateTime) -> Option<DateTime<Tz>> {
    tz.from_local_datetime(ndt).earliest()
}

/// Parse an iCal date/datetime value into the display zone `tz`. Returns (datetime, is_all_day).
///
/// Three datetime forms per RFC 5545: a trailing `Z` is UTC; a `TZID=<zone>` parameter names an
/// IANA zone the wall time is expressed in (the common Google Calendar case); bare values are
/// "floating" and read as the display zone. Honoring `TZID` is what keeps a meeting set in
/// another timezone from showing at the wrong hour.
fn parse_dt(p: &ical::property::Property, tz: Tz) -> Option<(DateTime<Tz>, bool)> {
    let value = p.value.as_ref()?;
    let is_date = param_has(p, "VALUE", "DATE") || value.len() == 8;
    if is_date {
        let d = NaiveDate::parse_from_str(&value[..value.len().min(8)], "%Y%m%d").ok()?;
        let ndt = d.and_hms_opt(0, 0, 0)?;
        return Some((resolve(tz, &ndt)?, true));
    }
    if let Some(stripped) = value.strip_suffix('Z') {
        let ndt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some((Utc.from_utc_datetime(&ndt).with_timezone(&tz), false));
    }
    let ndt = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    if let Some(evtz) = param_value(p, "TZID").and_then(|t| t.parse::<Tz>().ok()) {
        if let Some(dt) = evtz.from_local_datetime(&ndt).earliest() {
            return Some((dt.with_timezone(&tz), false));
        }
    }
    Some((resolve(tz, &ndt)?, false))
}

/// A **streaming** slimmer: feed it the iCal feed in arbitrary byte chunks ([`Slimmer::push`]),
/// and [`Slimmer::finish`] returns a tiny VCALENDAR containing only the VEVENT blocks that can
/// affect `[today - days_back, today + days_fwd]`: a concrete event whose own `DTSTART` (or, for
/// a rescheduled/cancelled recurring instance, `RECURRENCE-ID`) falls in that window, or a
/// recurring master (`RRULE`) whose series hasn't ended before the window starts — its own
/// `DTSTART` is typically ancient, but its *occurrences* keep landing in the window every week.
///
/// A secret Google feed is the user's *entire* calendar history — tens of MB, thousands of events
/// — but a status bar only cares about the next day or two, and the WASM sandbox can't hold the
/// whole feed (RFC 0020). So the plugin pulls the body in chunks (`ctx.http_read`) and pushes each
/// straight in here; the slimmer tracks the current VEVENT *across chunk boundaries* and keeps
/// only relevant ones, so resident memory stays bounded — the in-window events plus the (small,
/// bounded-by-number-of-still-active-series) set of recurring masters, never `O(feed)`.
/// The window spans a couple of days so the chip rolls over at midnight without a refetch.
pub struct Slimmer {
    lo: NaiveDate,
    hi: NaiveDate,
    out: String,
    /// Bytes after the last newline of the previous chunk — a line split across a chunk boundary.
    pending: String,
    in_event: bool,
    /// The current VEVENT's raw lines (only flushed to `out` if it's in-window).
    event_buf: String,
    /// Whether any property seen so far in the current event justifies keeping it (OR'd as each
    /// qualifying line — `DTSTART`, `RECURRENCE-ID`, `RRULE` — is seen; an event can qualify more
    /// than one way, e.g. a rescheduled recurring instance).
    keep: bool,
}

impl Slimmer {
    pub fn new(today: NaiveDate, days_back: i64, days_fwd: i64) -> Self {
        let mut out = String::with_capacity(8192);
        out.push_str("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//ezbar//slim//EN\r\n");
        Slimmer {
            lo: today - Duration::days(days_back.max(0)),
            hi: today + Duration::days(days_fwd.max(0)),
            out,
            pending: String::new(),
            in_event: false,
            event_buf: String::new(),
            keep: false,
        }
    }

    /// Feed the next raw chunk of the feed. iCal is ASCII/UTF-8; invalid bytes are lossily decoded.
    pub fn push(&mut self, chunk: &[u8]) {
        self.pending.push_str(&String::from_utf8_lossy(chunk));
        while let Some(nl) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=nl).collect(); // keeps the trailing '\n'
            self.feed_line(&line);
        }
    }

    /// Flush any trailing newline-less line and close the VCALENDAR; returns the slim feed.
    pub fn finish(mut self) -> String {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.feed_line(&line);
        }
        self.out.push_str("END:VCALENDAR\r\n");
        self.out
    }

    fn feed_line(&mut self, line: &str) {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        // VALARM uses its own BEGIN/END:VALARM, so this only ever matches a real event boundary.
        if trimmed.starts_with("BEGIN:VEVENT") {
            self.in_event = true;
            self.keep = false;
            self.event_buf.clear();
            self.event_buf.push_str(line);
            return;
        }
        if !self.in_event {
            return; // VCALENDAR-level line (VERSION/PRODID/VTIMEZONE/…) — dropped; we emit our own.
        }
        self.event_buf.push_str(line);
        // Property lines begin at column 0; a folded `DESCRIPTION` continuation starts with a
        // space, so `starts_with` won't false-match one. Any one of these justifies keeping the
        // event, so they OR together rather than deciding once (an event can qualify more than
        // one way — e.g. `RECURRENCE-ID` on a rescheduled instance of an otherwise-old series).
        if trimmed.starts_with("DTSTART") || trimmed.starts_with("RECURRENCE-ID") {
            if dtstart_in_window(trimmed, self.lo, self.hi) {
                self.keep = true;
            }
        } else if trimmed.starts_with("RRULE") && rrule_may_reach_window(trimmed, self.lo) {
            self.keep = true;
        }
        if trimmed.starts_with("END:VEVENT") {
            if self.keep {
                self.out.push_str(&self.event_buf);
            }
            self.in_event = false;
            self.event_buf.clear();
        }
    }
}

/// Is a `DTSTART…:YYYYMMDD…`-shaped property line's date within `[lo, hi]`? Parses the leading 8
/// digits of the value (after the first `:`), so it handles `DTSTART:`, `DTSTART;TZID=…:`,
/// `;VALUE=DATE:` — and, since `RECURRENCE-ID` values have the identical shape, that property too.
fn dtstart_in_window(line: &str, lo: NaiveDate, hi: NaiveDate) -> bool {
    let Some(colon) = line.find(':') else {
        return false;
    };
    let digits: String = line[colon + 1..].chars().take(8).collect();
    matches!(NaiveDate::parse_from_str(&digits, "%Y%m%d"), Ok(d) if d >= lo && d <= hi)
}

/// Could this `RRULE` line still produce an occurrence on or after `lo`? True unless it carries an
/// `UNTIL` that's already before `lo` — an open-ended or `COUNT`-limited rule can't be date-bounded
/// without full expansion, so it's kept (a live weekly 1:1 is small; the memory cost is one master
/// VEVENT per still-active series, not per occurrence).
fn rrule_may_reach_window(line: &str, lo: NaiveDate) -> bool {
    let Some(colon) = line.find(':') else {
        return true;
    };
    let value = &line[colon + 1..];
    let Some(until_at) = value.find("UNTIL=") else {
        return true;
    };
    let digits: String = value[until_at + "UNTIL=".len()..].chars().take(8).collect();
    !matches!(NaiveDate::parse_from_str(&digits, "%Y%m%d"), Ok(d) if d < lo)
}

/// Slice a whole in-memory iCal feed to the `[today-back, today+fwd]` window — a thin wrapper over
/// the streaming [`Slimmer`] (one `push` of the whole body). The plugin uses the `Slimmer`
/// directly so it never holds the full feed; this stays for callers/tests with the body in hand.
pub fn slim_ical(body: &str, today: NaiveDate, days_back: i64, days_fwd: i64) -> String {
    let mut s = Slimmer::new(today, days_back, days_fwd);
    s.push(body.as_bytes());
    s.finish()
}

/// Collapse content-identical events to one row.
///
/// A Google secret-iCal feed routinely carries the *same* meeting as two VEVENT blocks: a
/// recurring master (`RRULE`) plus an override for the modified/accepted instance
/// (`RECURRENCE-ID`, same `UID`), or a second copy that arrived as a meeting invite. Google's web
/// UI collapses them, so the user sees one event — but the raw feed has both, and since we don't
/// expand RRULE we'd render each in-window VEVENT, showing the same meeting twice at the same
/// time with an identical countdown.
///
/// We key on what the user actually sees — `(start, end, title, is_all_day)` — so true duplicates
/// merge regardless of whether their UIDs match (the master/override and the cross-invite cases
/// have different UIDs). When copies collide, we keep the first but **adopt a join link** from any
/// copy that has one, so the surviving row stays click-to-join even if the master lacked the link.
fn dedupe_events(events: Vec<CalendarEvent>) -> Vec<CalendarEvent> {
    use std::collections::HashMap;
    let mut seen: HashMap<(i64, i64, String, bool), usize> = HashMap::with_capacity(events.len());
    let mut out: Vec<CalendarEvent> = Vec::with_capacity(events.len());
    for ev in events {
        let key = (
            ev.start.timestamp(),
            ev.end.timestamp(),
            ev.title.clone(),
            ev.is_all_day,
        );
        match seen.get(&key) {
            Some(&i) => {
                if out[i].join_url.is_none() {
                    out[i].join_url = ev.join_url;
                }
            }
            None => {
                seen.insert(key, out.len());
                out.push(ev);
            }
        }
    }
    out
}

/// Parse an iCal feed into today's events + the next-meeting summary, in `now`'s timezone.
pub fn parse_calendar(body: &str, now: DateTime<Tz>) -> CalendarData {
    let tz = now.timezone();
    let start_of_day = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| resolve(tz, &d))
        .unwrap_or(now);
    let end_of_day = start_of_day + Duration::hours(24);

    let parser = ical::IcalParser::new(body.as_bytes());
    let events: Vec<ical::parser::ical::component::IcalEvent> =
        parser.flatten().flat_map(|cal| cal.events).collect();

    // A rescheduled/cancelled recurring instance is a separate VEVENT carrying the SAME UID as its
    // series master plus a `RECURRENCE-ID` naming the *original* (un-modified) occurrence it
    // replaces. Google does not reliably also EXDATE that original slot (see `expand_rrule`), so
    // this set is what stops the master's RRULE from generating a ghost duplicate alongside it.
    let mut overridden: HashSet<(String, i64)> = HashSet::new();
    for ev in &events {
        if let Some((rid, _)) = prop(ev, "RECURRENCE-ID").and_then(|p| parse_dt(p, tz)) {
            overridden.insert((prop_value(ev, "UID"), rid.timestamp()));
        }
    }

    let mut today: Vec<CalendarEvent> = Vec::new();
    for ev in &events {
        // A cancelled single instance of a recurring series is a VEVENT like any other — skip it
        // so it doesn't render as a real meeting (its slot is already freed via `overridden` above).
        if prop_value(ev, "STATUS").eq_ignore_ascii_case("CANCELLED") {
            continue;
        }

        if prop(ev, "RRULE").is_some() {
            today.extend(expand_rrule(ev, tz, start_of_day, end_of_day, &overridden));
            continue;
        }

        let start = match prop(ev, "DTSTART").and_then(|p| parse_dt(p, tz)) {
            Some(s) => s,
            None => continue,
        };
        let end = prop(ev, "DTEND")
            .and_then(|p| parse_dt(p, tz))
            .map(|(d, _)| d)
            .unwrap_or_else(|| start.0 + Duration::hours(1));
        let title = prop_value(ev, "SUMMARY");
        let location = prop_value(ev, "LOCATION");

        // Filter to today's window (matches the native module's Start/End filter).
        if end <= start_of_day || start.0 >= end_of_day {
            continue;
        }

        // The meeting link usually lives in DESCRIPTION; LOCATION/URL/X-GOOGLE-CONFERENCE are
        // fallbacks (Google Calendar puts the Meet link in X-GOOGLE-CONFERENCE). Scanning them
        // together lets `join_url` pick the best click-to-join link wherever it appears.
        let scan = format!(
            "{}\n{}\n{}\n{}",
            prop_value(ev, "DESCRIPTION"),
            location,
            prop_value(ev, "URL"),
            prop_value(ev, "X-GOOGLE-CONFERENCE"),
        );

        today.push(CalendarEvent {
            title,
            start: start.0,
            end,
            is_all_day: start.1,
            location,
            join_url: join_url(&scan),
        });
    }

    today = dedupe_events(today);
    today.sort_by_key(|a| a.start);

    let mut data = CalendarData {
        today_events: today.clone(),
        ..Default::default()
    };

    // Find next upcoming or ongoing (non-all-day) event.
    let mut next: Option<CalendarEvent> = None;
    for e in &today {
        if !e.is_all_day && e.end > now && next.as_ref().map(|n| e.start < n.start).unwrap_or(true)
        {
            next = Some(e.clone());
        }
    }

    let mut next = match next {
        None => {
            data.display_text = "No meetings".to_string();
            return data;
        }
        Some(n) => n,
    };

    let mut time_until = next.start - now;
    if time_until < Duration::zero() {
        if now < next.end {
            data.is_overdue = true;
            data.display_text = format!("NOW: {}", truncate_title(&next.title, 20));
            data.time_until_next = "ongoing".to_string();
            data.has_next = true;
            data.next_title = next.title.clone();
            return data;
        } else {
            // find actually-next future event
            let mut actual: Option<CalendarEvent> = None;
            for e in &today {
                if !e.is_all_day
                    && e.start > now
                    && actual.as_ref().map(|n| e.start < n.start).unwrap_or(true)
                {
                    actual = Some(e.clone());
                }
            }
            match actual {
                Some(a) => {
                    time_until = a.start - now;
                    next = a;
                }
                None => {
                    data.display_text = "No more meetings".to_string();
                    return data;
                }
            }
        }
    }

    data.has_next = true;
    data.next_title = next.title.clone();
    if time_until <= Duration::minutes(5) {
        data.is_urgent = true;
        data.display_text = format!("SOON: {}", truncate_title(&next.title, 18));
    } else if time_until <= Duration::minutes(15) {
        data.is_urgent = true;
        data.display_text = truncate_title(&next.title, 25);
    } else {
        data.display_text = truncate_title(&next.title, 25);
    }

    if time_until < Duration::hours(1) {
        data.time_until_next = format!("{}m", time_until.num_minutes());
    } else {
        let hours = time_until.num_hours();
        let mins = time_until.num_minutes() % 60;
        data.time_until_next = if mins > 0 {
            format!("{}h{}m", hours, mins)
        } else {
            format!("{}h", hours)
        };
    }

    data
}

/// Reconstruct a property's raw iCal content line (`NAME;PARAM=VAL;…:value`) from its parsed
/// form, so it can be fed straight to [`rrule`]'s own iCal-text parser instead of us re-deriving
/// timezone/date handling it already does correctly. `ical` has already unfolded any line
/// continuations into `value`, so the reconstructed line is always single-line and complete.
fn raw_prop_line(p: &ical::property::Property) -> String {
    let mut line = p.name.clone();
    if let Some(params) = &p.params {
        for (k, vs) in params {
            line.push(';');
            line.push_str(k);
            line.push('=');
            line.push_str(&vs.join(","));
        }
    }
    line.push(':');
    if let Some(v) = &p.value {
        line.push_str(v);
    }
    line
}

/// Expand a recurring master `VEVENT` (one carrying an `RRULE`) into its concrete occurrences
/// within `[start_of_day, end_of_day)`.
///
/// Built from the event's own raw `DTSTART`/`RRULE`/`EXDATE` lines rather than re-deriving them,
/// so `rrule` handles the timezone/DST-correct expansion (its own tests cover the DST edge cases
/// that would be easy to get subtly wrong by hand). Occurrences whose original slot is claimed by
/// a separate rescheduled/cancelled override VEVENT (`overridden`) are dropped — that override, if
/// still live, renders through the normal per-event path instead so the meeting doesn't double up
/// at two different times.
fn expand_rrule(
    ev: &ical::parser::ical::component::IcalEvent,
    tz: Tz,
    start_of_day: DateTime<Tz>,
    end_of_day: DateTime<Tz>,
    overridden: &HashSet<(String, i64)>,
) -> Vec<CalendarEvent> {
    let Some(dtstart_prop) = prop(ev, "DTSTART") else {
        return Vec::new();
    };
    let Some(rrule_prop) = prop(ev, "RRULE") else {
        return Vec::new();
    };
    let Some((master_start, is_all_day)) = parse_dt(dtstart_prop, tz) else {
        return Vec::new();
    };
    if is_all_day {
        return Vec::new(); // all-day recurring series: not expanded (rare; unchanged limitation).
    }
    let duration = prop(ev, "DTEND")
        .and_then(|p| parse_dt(p, tz))
        .map(|(d, _)| d - master_start)
        .unwrap_or_else(|| Duration::hours(1));

    let mut text = raw_prop_line(dtstart_prop);
    text.push('\n');
    text.push_str(&raw_prop_line(rrule_prop));
    for exdate in ev.properties.iter().filter(|p| p.name == "EXDATE") {
        text.push('\n');
        text.push_str(&raw_prop_line(exdate));
    }
    let Ok(set) = text.parse::<RRuleSet>() else {
        return Vec::new(); // malformed RRULE text: skip rather than crash the whole feed.
    };

    // Pad a day either side so a display-zone occurrence right at the window edge (whose event-zone
    // instant sits just outside it, or vice versa) is never missed by the rrule crate's own bound.
    let pad = Duration::hours(24);
    let after = (start_of_day - pad).with_timezone(&RTz::UTC);
    let before = (end_of_day + pad).with_timezone(&RTz::UTC);
    let occurrences = set.after(after).before(before).all(64).dates;

    let uid = prop_value(ev, "UID");
    let title = prop_value(ev, "SUMMARY");
    let location = prop_value(ev, "LOCATION");
    let scan = format!(
        "{}\n{}\n{}\n{}",
        prop_value(ev, "DESCRIPTION"),
        location,
        prop_value(ev, "URL"),
        prop_value(ev, "X-GOOGLE-CONFERENCE"),
    );
    let join = join_url(&scan);

    occurrences
        .into_iter()
        .filter_map(|d| {
            let start = d.with_timezone(&tz);
            if overridden.contains(&(uid.clone(), start.timestamp())) {
                return None;
            }
            let end = start + duration;
            if end <= start_of_day || start >= end_of_day {
                return None;
            }
            Some(CalendarEvent {
                title: title.clone(),
                start,
                end,
                is_all_day: false,
                location: location.clone(),
                join_url: join.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::UTC;

    fn prop(
        name: &str,
        value: &str,
        params: Option<Vec<(String, Vec<String>)>>,
    ) -> ical::property::Property {
        ical::property::Property {
            name: name.to_string(),
            params,
            value: Some(value.to_string()),
        }
    }

    fn ical_with(dtstart: &str, dtend: &str, summary: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:1@test\r\nSUMMARY:{summary}\r\nDTSTART:{dtstart}\r\nDTEND:{dtend}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn slim_keeps_only_the_window_and_stays_parseable() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 16).unwrap();
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:old@x\r\nSUMMARY:OldOne\r\nDTSTART:20200101T100000Z\r\nDTEND:20200101T110000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:t@x\r\nSUMMARY:TodayMtg\r\nDTSTART:20260616T140000Z\r\nDTEND:20260616T150000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:m@x\r\nSUMMARY:TmrwMtg\r\nDTSTART;TZID=America/New_York:20260617T090000\r\nDTEND;TZID=America/New_York:20260617T100000\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:f@x\r\nSUMMARY:FarOne\r\nDTSTART;VALUE=DATE:20270101\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let slim = slim_ical(body, today, 1, 2);
        assert!(slim.contains("TodayMtg"));
        assert!(slim.contains("TmrwMtg"));
        assert!(!slim.contains("OldOne"), "out-of-window past event dropped");
        assert!(!slim.contains("FarOne"), "far-future event dropped");
        assert!(slim.len() < body.len());
        // the slim output is still valid iCal that parse_calendar consumes
        let now = UTC.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        let d = parse_calendar(&slim, now);
        assert_eq!(d.next_title, "TodayMtg");
    }

    #[test]
    fn slimmer_handles_events_split_across_chunk_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 16).unwrap();
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:old@x\r\nSUMMARY:OldOne\r\nDTSTART:20200101T100000Z\r\nDTEND:20200101T110000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:t@x\r\nSUMMARY:TodayMtg\r\nDTSTART:20260616T140000Z\r\nDTEND:20260616T150000Z\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        // 7-byte chunks split lines (and the DTSTART value) across boundaries.
        let mut s = Slimmer::new(today, 1, 2);
        for chunk in body.as_bytes().chunks(7) {
            s.push(chunk);
        }
        let slim = s.finish();
        assert!(slim.contains("TodayMtg"));
        assert!(!slim.contains("OldOne"));
        // chunking is transparent: identical to feeding the whole body at once.
        assert_eq!(slim, slim_ical(body, today, 1, 2));
        let now = UTC.with_ymd_and_hms(2026, 6, 16, 12, 0, 0).unwrap();
        assert_eq!(parse_calendar(&slim, now).next_title, "TodayMtg");
    }

    #[test]
    fn truncate_title_ellipsizes() {
        assert_eq!(truncate_title("short", 10), "short");
        assert_eq!(truncate_title("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate_title("abcdefghijk", 10), "abcdefgh..");
    }

    #[test]
    fn parse_dt_all_day_date() {
        let (dt, all_day) = parse_dt(&prop("DTSTART", "20260531", None), UTC).unwrap();
        assert!(all_day);
        assert_eq!(
            dt.date_naive(),
            NaiveDate::from_ymd_opt(2026, 5, 31).unwrap()
        );
    }

    #[test]
    fn parse_dt_utc_z_converts() {
        let (dt, all_day) = parse_dt(&prop("DTSTART", "20260531T120000Z", None), UTC).unwrap();
        assert!(!all_day);
        assert_eq!(
            dt.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 5, 31, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_dt_honors_tzid() {
        // 09:00 in New York is a fixed instant regardless of the display zone — the bug we guard
        // against is treating this wall time as the display zone and showing 09:00.
        let p = prop(
            "DTSTART",
            "20260531T090000",
            Some(vec![("TZID".into(), vec!["America/New_York".into()])]),
        );
        let (dt, _) = parse_dt(&p, UTC).unwrap();
        // 2026-05-31 is EDT (UTC-4) → 13:00 UTC.
        assert_eq!(
            dt.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_dt_quoted_tzid() {
        let p = prop(
            "DTSTART",
            "20260531T090000",
            Some(vec![("TZID".into(), vec!["\"America/New_York\"".into()])]),
        );
        let dt = parse_dt(&p, UTC).unwrap().0;
        assert_eq!(
            dt.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_dt_unknown_tzid_falls_back_to_display_zone() {
        let p = prop(
            "DTSTART",
            "20260531T140000",
            Some(vec![("TZID".into(), vec!["Mars/Olympus".into()])]),
        );
        let dt = parse_dt(&p, UTC).unwrap().0;
        assert_eq!(dt, UTC.with_ymd_and_hms(2026, 5, 31, 14, 0, 0).unwrap());
    }

    #[test]
    fn no_meetings_when_empty() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 9, 0, 0).unwrap();
        let d = parse_calendar("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n", now);
        assert!(!d.has_next);
        assert_eq!(d.display_text, "No meetings");
    }

    #[test]
    fn next_meeting_soon() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 57, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000", "20260531T143000", "Standup"),
            now,
        );
        assert!(d.has_next);
        assert_eq!(d.next_title, "Standup");
        assert!(d.is_urgent);
        assert!(!d.is_overdue);
        assert_eq!(d.time_until_next, "3m");
        assert_eq!(d.display_text, "SOON: Standup");
    }

    #[test]
    fn ongoing_meeting_is_overdue() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 14, 10, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000", "20260531T143000", "Standup"),
            now,
        );
        assert!(d.is_overdue);
        assert_eq!(d.time_until_next, "ongoing");
        assert_eq!(d.display_text, "NOW: Standup");
    }

    #[test]
    fn far_future_meeting_not_urgent() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 9, 0, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000", "20260531T143000", "Review"),
            now,
        );
        assert!(d.has_next);
        assert!(!d.is_urgent);
        assert_eq!(d.display_text, "Review");
        assert_eq!(d.time_until_next, "5h");
    }

    #[test]
    fn event_with_zoom_description_gets_join_url() {
        // SANITIZED: fabricated meeting id + token (no real meeting encoded).
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1@test\r\n\
            SUMMARY:Debrief\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\n\
            DESCRIPTION:Join Zoom Meeting https://acme.zoom.us/j/12345678901?pwd=tok.1\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(
            d.today_events[0].join_url.as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=tok.1")
        );
    }

    #[test]
    fn folded_description_keeps_zoom_token_intact() {
        // iCal folds long lines: CRLF + a leading space mid-value. The parser MUST unfold so the
        // pwd token isn't split (a split token = web client prompts for the passcode). The token
        // here is broken across a fold right in the middle ("...KlMnOp" | "QrStUv.1").
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1@test\r\n\
            SUMMARY:Debrief\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\n\
            DESCRIPTION:Join https://acme.zoom.us/j/12345678901?pwd=AbCdEfGhIjKlMnOp\r\n QrStUv.1\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(
            d.today_events[0].join_url.as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=AbCdEfGhIjKlMnOpQrStUv.1")
        );
    }

    #[test]
    fn event_with_google_meet_gets_join_url() {
        // Meet link in DESCRIPTION (escaped newline) + the X-GOOGLE-CONFERENCE property Google adds.
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1@test\r\n\
            SUMMARY:Standup\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\n\
            DESCRIPTION:Join with Google Meet\\nhttps://meet.google.com/abc-defg-hij\r\n\
            X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(
            d.today_events[0].join_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }

    #[test]
    fn duplicate_vevents_collapse_to_one_row() {
        // Google secret-iCal: a recurring master + its modified-instance override land as two
        // VEVENTs with the SAME start/title (different UIDs). The web UI shows one; the raw feed
        // has both. We must render one row — the bug was a recurring meeting appearing twice.
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:series@google\r\nSUMMARY:Team Sync\r\n\
            DTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:override@google\r\nSUMMARY:Team Sync\r\n\
            RECURRENCE-ID:20260531T140000Z\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 12, 10, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(
            d.today_events.len(),
            1,
            "identical copies must merge to one"
        );
        assert_eq!(d.next_title, "Team Sync");
    }

    #[test]
    fn dedupe_keeps_the_join_link_from_either_copy() {
        // The copy carrying the Meet link may not be the first one seen — the survivor must still
        // be click-to-join.
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:a@x\r\nSUMMARY:Standup\r\n\
            DTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:b@x\r\nSUMMARY:Standup\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\n\
            X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(
            d.today_events[0].join_url.as_deref(),
            Some("https://meet.google.com/abc-defg-hij"),
        );
    }

    #[test]
    fn distinct_meetings_at_same_time_are_kept() {
        // Same slot, different titles = two real parallel meetings. Must NOT be collapsed.
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
            BEGIN:VEVENT\r\nUID:a@x\r\nSUMMARY:Team Sync\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:b@x\r\nSUMMARY:1:1 with Sam\r\nDTSTART:20260531T140000Z\r\nDTEND:20260531T143000Z\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(body, now);
        assert_eq!(d.today_events.len(), 2);
    }

    #[test]
    fn event_without_zoom_has_no_join_url() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000Z", "20260531T143000Z", "Lunch"),
            now,
        );
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(d.today_events[0].join_url, None);
    }

    // ── RRULE expansion — regression coverage for a real bug: a years-old weekly 1:1 whose
    // literal DTSTART had long aged out of the window silently stopped appearing, because only
    // concrete VEVENTs were ever shown and the RRULE was never expanded. ──

    /// A weekly-Thursday master starting long before `now`, mirroring the real feed: `DTSTART`
    /// in `Europe/London` (so an in-window occurrence lands in BST, UTC+1) and no `UNTIL` (open).
    fn weekly_1on1(extra: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1on1@google.com\r\n\
            SUMMARY:Johannes / Gavin 1:1\r\n\
            DTSTART;TZID=Europe/London:20240201T110000\r\nDTEND;TZID=Europe/London:20240201T112500\r\n\
            RRULE:FREQ=WEEKLY;BYDAY=TH\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[test]
    fn weekly_rrule_expands_to_far_future_occurrence() {
        // 2026-07-02 is a Thursday, 2.5 years after DTSTART — nothing but the RRULE says it recurs.
        let now = UTC.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let d = parse_calendar(&weekly_1on1(""), now);
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(d.today_events[0].title, "Johannes / Gavin 1:1");
        // 11:00 in Europe/London is BST (UTC+1) in July → 10:00 UTC.
        assert_eq!(
            d.today_events[0].start.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 7, 2, 10, 0, 0).unwrap()
        );
        assert_eq!(
            d.today_events[0].end.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 7, 2, 10, 25, 0).unwrap()
        );
        assert_eq!(d.next_title, "Johannes / Gavin 1:1");
    }

    #[test]
    fn rrule_exdate_excludes_that_occurrence() {
        let now = UTC.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let body = weekly_1on1("EXDATE;TZID=Europe/London:20260702T110000\r\n");
        let d = parse_calendar(&body, now);
        assert_eq!(d.today_events.len(), 0, "EXDATE'd instance must not appear");
    }

    #[test]
    fn rrule_respects_until_and_stops_generating() {
        let now = UTC.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:old@google.com\r\n\
            SUMMARY:Retired 1:1\r\n\
            DTSTART;TZID=Europe/London:20240201T110000\r\nDTEND;TZID=Europe/London:20240201T112500\r\n\
            RRULE:FREQ=WEEKLY;UNTIL=20240301T000000Z;BYDAY=TH\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let d = parse_calendar(body, now);
        assert_eq!(
            d.today_events.len(),
            0,
            "series ended in 2024, no 2026 occurrence"
        );
    }

    #[test]
    fn rrule_override_replaces_generated_occurrence_not_duplicates_it() {
        // A same-UID VEVENT with RECURRENCE-ID = today's *original* slot, rescheduled to a new
        // time. Must render ONCE, at the rescheduled time — not twice (generated + override).
        let now = UTC.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let mut body = weekly_1on1("");
        body.truncate(body.len() - "END:VCALENDAR\r\n".len());
        body.push_str(
            "BEGIN:VEVENT\r\nUID:1on1@google.com\r\nSUMMARY:Johannes / Gavin 1:1\r\n\
            RECURRENCE-ID;TZID=Europe/London:20260702T110000\r\n\
            DTSTART;TZID=Europe/London:20260702T140000\r\nDTEND;TZID=Europe/London:20260702T142500\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n",
        );
        let d = parse_calendar(&body, now);
        assert_eq!(
            d.today_events.len(),
            1,
            "override replaces, doesn't duplicate, the generated slot"
        );
        // rescheduled to 14:00 Europe/London (BST) = 13:00 UTC.
        assert_eq!(
            d.today_events[0].start.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 7, 2, 13, 0, 0).unwrap()
        );
    }

    #[test]
    fn cancelled_recurring_instance_is_hidden_and_suppresses_the_generated_one() {
        let now = UTC.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
        let mut body = weekly_1on1("");
        body.truncate(body.len() - "END:VCALENDAR\r\n".len());
        body.push_str(
            "BEGIN:VEVENT\r\nUID:1on1@google.com\r\nSUMMARY:Johannes / Gavin 1:1\r\n\
            RECURRENCE-ID;TZID=Europe/London:20260702T110000\r\n\
            DTSTART;TZID=Europe/London:20260702T110000\r\nDTEND;TZID=Europe/London:20260702T112500\r\n\
            STATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        );
        let d = parse_calendar(&body, now);
        assert_eq!(
            d.today_events.len(),
            0,
            "cancelled instance: neither shown nor duplicated"
        );
    }

    #[test]
    fn slimmer_keeps_an_ancient_open_recurring_master() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let slim = slim_ical(&weekly_1on1(""), today, 1, 2);
        assert!(
            slim.contains("Johannes / Gavin 1:1"),
            "an open (no UNTIL) RRULE master must survive slimming regardless of its own ancient DTSTART"
        );
    }

    #[test]
    fn slimmer_drops_a_finished_recurring_master() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:old@google.com\r\n\
            SUMMARY:Retired 1:1\r\n\
            DTSTART;TZID=Europe/London:20240201T110000\r\nDTEND;TZID=Europe/London:20240201T112500\r\n\
            RRULE:FREQ=WEEKLY;UNTIL=20240301T000000Z;BYDAY=TH\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let slim = slim_ical(body, today, 1, 2);
        assert!(
            !slim.contains("Retired 1:1"),
            "a series whose UNTIL is long past the window must not be kept"
        );
    }

    #[test]
    fn slimmer_keeps_a_rescheduled_override_by_its_original_slot() {
        // The override's OWN new time is far outside the window; only its RECURRENCE-ID (the slot
        // it vacates) is inside it. It must still be kept, or the master would wrongly regenerate
        // a ghost at the original slot with no way to know it was moved away.
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let body = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1on1@google.com\r\n\
            SUMMARY:Moved Far Away\r\nRECURRENCE-ID;TZID=Europe/London:20260702T110000\r\n\
            DTSTART;TZID=Europe/London:20261225T110000\r\nDTEND;TZID=Europe/London:20261225T112500\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let slim = slim_ical(body, today, 1, 2);
        assert!(slim.contains("Moved Far Away"));
    }
}
