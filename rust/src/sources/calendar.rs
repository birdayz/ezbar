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

fn prop<'a>(ev: &'a ical::parser::ical::component::IcalEvent, name: &str) -> Option<&'a ical::property::Property> {
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
    Ok(parse_calendar(&body))
}

fn parse_calendar(body: &str) -> CalendarData {
    let now = Local::now();
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

    today.sort_by(|a, b| a.start.cmp(&b.start));

    let mut data = CalendarData {
        today_events: today.clone(),
        ..Default::default()
    };

    // Find next upcoming or ongoing (non-all-day) event.
    let mut next: Option<CalendarEvent> = None;
    for e in &today {
        if !e.is_all_day && e.end > now {
            if next.as_ref().map(|n| e.start < n.start).unwrap_or(true) {
                next = Some(e.clone());
            }
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
                if !e.is_all_day && e.start > now {
                    if actual.as_ref().map(|n| e.start < n.start).unwrap_or(true) {
                        actual = Some(e.clone());
                    }
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
