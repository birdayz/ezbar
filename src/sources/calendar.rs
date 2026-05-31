//! Google Calendar via secret iCal URL. Port of pkg/datasource/calendar.go.
//! Note: recurring-event (RRULE) expansion is not performed; concrete VEVENT
//! instances within today's window are shown.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[allow(dead_code)] // location/fields mirror the Go model; not all are rendered
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub is_all_day: bool,
    pub location: String,
}

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

fn config_url() -> Result<String, String> {
    if let Ok(url) = std::env::var("GOOGLE_CALENDAR_ICAL_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    let path = format!("{home}/.config/ezbar/calendar_url");
    let data = std::fs::read_to_string(&path).map_err(|_| {
        "calendar URL not configured. Save your secret iCal URL to ~/.config/ezbar/calendar_url"
            .to_string()
    })?;
    let url = data.trim().to_string();
    if url.is_empty() {
        return Err("calendar_url file is empty".to_string());
    }
    Ok(url)
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

/// Parses an iCal date/datetime value into local time. Returns (datetime, is_all_day).
fn parse_dt(p: &ical::property::Property) -> Option<(DateTime<Local>, bool)> {
    let value = p.value.as_ref()?;
    let is_date = param_has(p, "VALUE", "DATE") || value.len() == 8;
    if is_date {
        let d = NaiveDate::parse_from_str(&value[..value.len().min(8)], "%Y%m%d").ok()?;
        let ndt = d.and_hms_opt(0, 0, 0)?;
        return Some((Local.from_local_datetime(&ndt).single()?, true));
    }
    if let Some(stripped) = value.strip_suffix('Z') {
        let ndt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some((Utc.from_utc_datetime(&ndt).with_timezone(&Local), false));
    }
    let ndt = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Some((Local.from_local_datetime(&ndt).single()?, false))
}

pub async fn get_events() -> Result<CalendarData, String> {
    let url = config_url()?;
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&url)
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} fetching calendar", resp.status().as_u16()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    Ok(parse_calendar(&body, Local::now()))
}

fn parse_calendar(body: &str, now: DateTime<Local>) -> CalendarData {
    let start_of_day = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|d| Local.from_local_datetime(&d).single().unwrap_or(now))
        .unwrap_or(now);
    let end_of_day = start_of_day + Duration::hours(24);

    let mut today: Vec<CalendarEvent> = Vec::new();
    let parser = ical::IcalParser::new(body.as_bytes());
    for cal in parser.flatten() {
        for ev in cal.events {
            let start = match prop(&ev, "DTSTART").and_then(parse_dt) {
                Some(s) => s,
                None => continue,
            };
            let end = prop(&ev, "DTEND")
                .and_then(parse_dt)
                .map(|(d, _)| d)
                .unwrap_or((start.0 + Duration::hours(1), false).0);
            let title = prop(&ev, "SUMMARY")
                .and_then(|p| p.value.clone())
                .unwrap_or_default();
            let location = prop(&ev, "LOCATION")
                .and_then(|p| p.value.clone())
                .unwrap_or_default();

            // Filter to today's window (matches gocal Start/End filter).
            if end <= start_of_day || start.0 >= end_of_day {
                continue;
            }
            today.push(CalendarEvent {
                title,
                start: start.0,
                end,
                is_all_day: start.1,
                location,
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
    fn param_has_is_case_insensitive() {
        let p = prop(
            "DTSTART",
            "20260531",
            Some(vec![("VALUE".into(), vec!["DATE".into()])]),
        );
        assert!(param_has(&p, "VALUE", "date"));
        assert!(param_has(&p, "value", "DATE"));
        assert!(!param_has(&p, "TZID", "DATE"));
    }

    #[test]
    fn parse_dt_all_day_date() {
        let (dt, all_day) = parse_dt(&prop("DTSTART", "20260531", None)).unwrap();
        assert!(all_day);
        assert_eq!(
            dt.date_naive(),
            NaiveDate::from_ymd_opt(2026, 5, 31).unwrap()
        );
    }

    #[test]
    fn parse_dt_value_date_param() {
        let p = prop(
            "DTSTART",
            "20260531T000000",
            Some(vec![("VALUE".into(), vec!["DATE".into()])]),
        );
        assert!(parse_dt(&p).unwrap().1);
    }

    #[test]
    fn parse_dt_utc_z_converts() {
        let (dt, all_day) = parse_dt(&prop("DTSTART", "20260531T120000Z", None)).unwrap();
        assert!(!all_day);
        assert_eq!(
            dt.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 5, 31, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_dt_floating_is_local() {
        let (dt, all_day) = parse_dt(&prop("DTSTART", "20260531T140000", None)).unwrap();
        assert!(!all_day);
        assert_eq!(dt, Local.with_ymd_and_hms(2026, 5, 31, 14, 0, 0).unwrap());
    }

    #[test]
    fn no_meetings_when_empty() {
        let now = Local.with_ymd_and_hms(2026, 5, 31, 9, 0, 0).unwrap();
        let d = parse_calendar("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n", now);
        assert!(!d.has_next);
        assert_eq!(d.display_text, "No meetings");
    }

    #[test]
    fn next_meeting_soon() {
        let now = Local.with_ymd_and_hms(2026, 5, 31, 13, 57, 0).unwrap();
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
        let now = Local.with_ymd_and_hms(2026, 5, 31, 14, 10, 0).unwrap();
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
        let now = Local.with_ymd_and_hms(2026, 5, 31, 9, 0, 0).unwrap();
        let d = parse_calendar(
            &ical_with("20260531T140000", "20260531T143000", "Review"),
            now,
        );
        assert!(d.has_next);
        assert!(!d.is_urgent);
        assert_eq!(d.display_text, "Review");
        assert_eq!(d.time_until_next, "5h");
    }
}
