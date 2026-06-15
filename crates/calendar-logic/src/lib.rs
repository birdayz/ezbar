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
//! Recurring-event (RRULE) expansion is not performed; concrete VEVENT instances within today's
//! window are shown (matches the native module this replaces).

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

pub mod zoom;
pub use zoom::{best_meeting, zoom_join_url, ZoomMeeting};

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

/// Parse an iCal feed into today's events + the next-meeting summary, in `now`'s timezone.
pub fn parse_calendar(body: &str, now: DateTime<Tz>) -> CalendarData {
    let tz = now.timezone();
    let start_of_day = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| resolve(tz, &d))
        .unwrap_or(now);
    let end_of_day = start_of_day + Duration::hours(24);

    let mut today: Vec<CalendarEvent> = Vec::new();
    let parser = ical::IcalParser::new(body.as_bytes());
    for cal in parser.flatten() {
        for ev in cal.events {
            let start = match prop(&ev, "DTSTART").and_then(|p| parse_dt(p, tz)) {
                Some(s) => s,
                None => continue,
            };
            let end = prop(&ev, "DTEND")
                .and_then(|p| parse_dt(p, tz))
                .map(|(d, _)| d)
                .unwrap_or_else(|| start.0 + Duration::hours(1));
            let title = prop_value(&ev, "SUMMARY");
            let location = prop_value(&ev, "LOCATION");

            // Filter to today's window (matches the native module's Start/End filter).
            if end <= start_of_day || start.0 >= end_of_day {
                continue;
            }

            // The Zoom link usually lives in DESCRIPTION; LOCATION/URL are fallbacks. Scanning all
            // three in order lets `best_meeting` still prefer a `pwd`-bearing link wherever it is.
            let scan = format!(
                "{}\n{}\n{}",
                prop_value(&ev, "DESCRIPTION"),
                location,
                prop_value(&ev, "URL"),
            );

            today.push(CalendarEvent {
                title,
                start: start.0,
                end,
                is_all_day: start.1,
                location,
                join_url: zoom_join_url(&scan),
            });
        }
    }

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
    fn event_without_zoom_has_no_join_url() {
        let now = UTC.with_ymd_and_hms(2026, 5, 31, 13, 0, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000Z", "20260531T143000Z", "Lunch"),
            now,
        );
        assert_eq!(d.today_events.len(), 1);
        assert_eq!(d.today_events[0].join_url, None);
    }
}
