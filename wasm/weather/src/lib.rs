//! Example ezbar WASM plugin: a weather chip.
//!
//! Pulls REAL data from open-meteo (capability-gated) and renders it the way a
//! weather app should: a condition icon (day/night aware) + temperature in the
//! chip, and a clean forecast panel on hover — an hourly strip and a 4-day
//! outlook, all from the host icon set. No stock-style chart anywhere.
//!
//! Note how little there is: a `Plugin` impl + `export_plugin!`. No wit-bindgen,
//! no generated-type glue — the SDK owns all of that.

use ezbar_plugin_wasm::prelude::*;
use serde_json::Value;

struct HourPt {
    label: String, // "15"
    temp: f64,
    code: u8,
    pop: u8,
    is_day: bool,
}

struct DayPt {
    label: String, // "Today" / "Tue"
    hi: f64,
    lo: f64,
    code: u8,
    pop: u8,
}

struct Weather {
    place: String,
    lat: String,
    lon: String,
    // current conditions
    temp: f64,
    feels: f64,
    code: u8,
    is_day: bool,
    wind: f64,
    humidity: f64,
    precip_now: f64,
    sun_label: String, // "06:14" — next sunrise or today's sunset
    before_dawn: bool, // show a sunrise icon vs a sunset icon
    hours: Vec<HourPt>,
    days: Vec<DayPt>,
    loaded: bool,
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            place: String::new(),
            lat: "52.52".into(), // Berlin; override via [modules.weather].lat/lon
            lon: "13.41".into(),
            temp: 0.0,
            feels: 0.0,
            code: 0,
            is_day: true,
            wind: 0.0,
            humidity: 0.0,
            precip_now: 0.0,
            sun_label: String::new(),
            before_dawn: false,
            hours: Vec::new(),
            days: Vec::new(),
            loaded: false,
        }
    }
}

impl Plugin for Weather {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            match k.as_str() {
                "lat" => self.lat = v.clone(),
                "lon" => self.lon = v.clone(),
                "name" => self.place = v.clone(),
                _ => {}
            }
        }
        if self.place.is_empty() {
            self.place = format!("{}, {}", self.lat, self.lon);
        }
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
             &current=temperature_2m,apparent_temperature,weathercode,is_day,windspeed_10m,relative_humidity_2m,precipitation\
             &hourly=temperature_2m,weathercode,precipitation_probability\
             &daily=weathercode,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset\
             &forecast_days=4&timezone=auto",
            self.lat, self.lon
        );
        match ctx.http_get(&url) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => {
                    self.ingest(&v);
                    true
                }
                Err(e) => {
                    ctx.log(&format!("weather: parse {e}"));
                    false
                }
            },
            Err(e) => {
                ctx.log(&format!("weather: {e}"));
                false
            }
        }
    }

    fn view(&self) -> Render {
        if !self.loaded {
            return row([
                Icon::Cloud.view(15.0, Token::FgDim),
                text("\u{2026}").color(Token::FgDim),
            ])
            .spacing(6.0);
        }
        let mut items = vec![
            wmo_icon(self.code, self.is_day).view(15.0, sky_tint(self.code, self.is_day)),
            text(format!("{:.0}\u{b0}", self.temp)).color(temp_color(self.temp)),
        ];
        // precip cluster: only when it's raining now or imminent — keeps the chip
        // clean on dry days, a heads-up on wet ones.
        let next_pop = self.hours.first().map(|h| h.pop).unwrap_or(0);
        if self.precip_now > 0.0 || next_pop >= 30 {
            items.push(
                row([
                    Icon::Droplets.view(10.0, Token::Accent),
                    text(format!("{next_pop}%")).color(Token::Accent).size(11.0),
                ])
                .spacing(2.0),
            );
        }
        row(items).spacing(6.0)
    }

    fn popup(&self) -> Option<Render> {
        if !self.loaded {
            return None;
        }
        Some(
            container(
                column([
                    self.header(),
                    self.hourly_strip(),
                    divider(),
                    self.daily_strip(),
                ])
                .spacing(11.0),
            )
            .padding(14.0),
        )
    }
}

impl Weather {
    fn ingest(&mut self, v: &Value) {
        let cur = &v["current"];
        self.temp = cur["temperature_2m"].as_f64().unwrap_or(0.0);
        self.feels = cur["apparent_temperature"].as_f64().unwrap_or(self.temp);
        self.code = cur["weathercode"].as_u64().unwrap_or(0) as u8;
        self.is_day = cur["is_day"].as_i64().unwrap_or(1) != 0;
        self.wind = cur["windspeed_10m"].as_f64().unwrap_or(0.0);
        self.humidity = cur["relative_humidity_2m"].as_f64().unwrap_or(0.0);
        self.precip_now = cur["precipitation"].as_f64().unwrap_or(0.0);
        let now = cur["time"].as_str().unwrap_or("");

        // daily (today + 3): build the day cards and a date→(sunrise,sunset) lookup.
        let d = &v["daily"];
        let dtime = d["time"].as_array();
        let mut sun_by_date: Vec<(String, String, String)> = Vec::new(); // (date, sunrise, sunset)
        self.days.clear();
        if let Some(times) = dtime {
            for i in 0..times.len() {
                let date = times[i].as_str().unwrap_or("").to_string();
                let sunrise = arr_str(d, "sunrise", i);
                let sunset = arr_str(d, "sunset", i);
                sun_by_date.push((date.clone(), sunrise.clone(), sunset.clone()));
                self.days.push(DayPt {
                    label: if i == 0 { "Today".into() } else { weekday(&date).into() },
                    hi: arr_f64(d, "temperature_2m_max", i),
                    lo: arr_f64(d, "temperature_2m_min", i),
                    code: arr_f64(d, "weathercode", i) as u8,
                    pop: arr_f64(d, "precipitation_probability_max", i) as u8,
                });
            }
        }

        // today's sun: sunrise vs sunset depending on the time of day.
        if let Some((_, sunrise, sunset)) = sun_by_date.first() {
            self.before_dawn = !now.is_empty() && now < sunrise.as_str();
            let pick = if self.before_dawn { sunrise } else { sunset };
            self.sun_label = hhmm(pick).to_string();
        }

        // hourly: the next 6 whole hours from now.
        let h = &v["hourly"];
        self.hours.clear();
        if let Some(htime) = h["time"].as_array() {
            let start = htime.iter().position(|t| t.as_str().unwrap_or("") > now).unwrap_or(0);
            for i in start..(start + 6).min(htime.len()) {
                let t = htime[i].as_str().unwrap_or("");
                self.hours.push(HourPt {
                    label: t.get(11..13).unwrap_or("").to_string(),
                    temp: arr_f64(h, "temperature_2m", i),
                    code: arr_f64(h, "weathercode", i) as u8,
                    pop: arr_f64(h, "precipitation_probability", i) as u8,
                    is_day: day_at(t, &sun_by_date),
                });
            }
        }
        self.loaded = true;
    }

    fn header(&self) -> Render {
        let temp_line = row([
            text(format!("{:.0}\u{b0}", self.temp)).color(temp_color(self.temp)).size(30.0),
            text(condition_label(self.code)).color(Token::FgDim).size(13.0),
        ])
        .spacing(6.0)
        .align(Align::End);

        let hero = row([
            wmo_icon(self.code, self.is_day).view(34.0, sky_tint(self.code, self.is_day)),
            column([
                temp_line,
                text(format!("Feels {:.0}\u{b0}  \u{b7}  {}", self.feels, self.place))
                    .color(Token::FgDim)
                    .size(11.0),
            ])
            .spacing(1.0),
        ])
        .spacing(10.0)
        .align(Align::Center);

        let (sun_icon, sun_tint) = if self.before_dawn {
            (Icon::Sunrise, Token::Warn)
        } else {
            (Icon::Sunset, Token::Warn)
        };
        let metrics = row([
            metric(Icon::Droplets, Token::Accent, format!("{:.0}%", self.humidity)),
            metric(Icon::Wind, Token::FgDim, format!("{:.0} km/h", self.wind)),
            metric(sun_icon, sun_tint, self.sun_label.clone()),
        ])
        .spacing(14.0)
        .align(Align::Center);

        column([hero, metrics]).spacing(8.0)
    }

    fn hourly_strip(&self) -> Render {
        let cols: Vec<Render> = self
            .hours
            .iter()
            .map(|h| {
                column([
                    text(h.label.clone()).color(Token::FgDim).size(11.0),
                    wmo_icon(h.code, h.is_day).view(20.0, sky_tint(h.code, h.is_day)),
                    text(format!("{:.0}\u{b0}", h.temp)).color(temp_color(h.temp)).size(13.0),
                    text(pop_str(h.pop)).color(Token::Accent).size(10.0),
                ])
                .spacing(4.0)
                .align(Align::Center)
            })
            .collect();
        row(cols).spacing(14.0)
    }

    fn daily_strip(&self) -> Render {
        let rows: Vec<Render> = self
            .days
            .iter()
            .map(|d| {
                // hi/lo fused into one atomic "26°/12°" chunk — keeps the temp-colour
                // vs muted-low hierarchy while reading as a single range (the slash
                // does the column work that proportional text can't).
                let range = row([
                    text(fig_temp(d.hi)).color(temp_color(d.hi)).size(13.0),
                    text("/").color(Token::FgDim).size(13.0),
                    text(fig_temp(d.lo)).color(Token::FgDim).size(13.0),
                ])
                .spacing(1.0)
                .align(Align::Center);

                let mut cells = vec![
                    text(pad_right(&d.label, 5)).color(Token::Fg).size(12.0),
                    wmo_icon(d.code, true).view(18.0, sky_tint(d.code, true)),
                    range,
                ];
                // precip demoted to the trailing edge (no second water glyph — the
                // condition icon already says rain); raggedness hides off the right.
                if d.pop >= 20 {
                    cells.push(spacer(8.0));
                    cells.push(text(format!("{}%", d.pop)).color(Token::Accent).size(12.0));
                }
                row(cells).spacing(10.0).align(Align::Center)
            })
            .collect();
        column(rows).spacing(8.0)
    }
}

/// A thin, dim full-width rule that splits the popup into "next hours" / "next
/// days" chapters (the DSL has no border node, so a hairline of light box-rule
/// glyphs at a small size stands in).
fn divider() -> Render {
    text("\u{2500}".repeat(48)).color(Token::FgDim).size(7.0)
}

fn metric(icon: Icon, tint: Token, label: String) -> Render {
    row([icon.view(11.0, tint), text(label).color(Token::FgDim).size(11.0)]).spacing(4.0)
}

// ── WMO weathercode → icon / label / colour ─────────────────────────────────

fn wmo_icon(code: u8, is_day: bool) -> Icon {
    match code {
        0 => {
            if is_day {
                Icon::Sun
            } else {
                Icon::Moon
            }
        }
        1 | 2 => {
            if is_day {
                Icon::CloudSun
            } else {
                Icon::CloudMoon
            }
        }
        3 => Icon::Cloud,
        45 | 48 => Icon::CloudFog,
        51 | 53 | 55 => Icon::CloudDrizzle,
        56 | 57 | 66 | 67 => Icon::CloudHail, // freezing drizzle / rain
        61 | 63 | 80 | 81 => Icon::CloudRain,
        65 | 82 => Icon::CloudRainWind, // heavy rain / violent showers
        71 | 73 | 75 | 77 | 85 | 86 => Icon::CloudSnow,
        95 | 96 | 99 => Icon::CloudLightning,
        _ => Icon::Cloud,
    }
}

fn condition_label(code: u8) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 | 77 => "Snow",
        80 | 81 => "Rain showers",
        82 => "Heavy showers",
        85 | 86 => "Snow showers",
        95 | 96 | 99 => "Thunderstorm",
        _ => "—",
    }
}

/// The temperature value's colour — only the extremes earn a colour; the
/// comfortable band stays neutral so the chip isn't a christmas tree.
fn temp_color(t: f64) -> Token {
    if t < 0.0 {
        Token::Accent
    } else if t < 26.0 {
        Token::Fg
    } else if t < 32.0 {
        Token::Warn
    } else {
        Token::Urgent
    }
}

/// The condition icon's tint — sky-blue for clear days, muted for grey skies,
/// neutral for precipitation, alert for storms.
fn sky_tint(code: u8, is_day: bool) -> Token {
    match code {
        0 | 1 | 2 => {
            if is_day {
                Token::Accent
            } else {
                Token::FgDim
            }
        }
        3 | 45 | 48 => Token::FgDim,
        95 | 96 | 99 => Token::Warn,
        _ => Token::Fg,
    }
}

// ── small helpers ───────────────────────────────────────────────────────────

fn arr_f64(obj: &Value, key: &str, i: usize) -> f64 {
    obj[key].as_array().and_then(|a| a.get(i)).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn arr_str(obj: &Value, key: &str, i: usize) -> String {
    obj[key]
        .as_array()
        .and_then(|a| a.get(i))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// "HH:MM" out of an ISO "YYYY-MM-DDTHH:MM".
fn hhmm(iso: &str) -> &str {
    iso.get(11..16).unwrap_or(iso)
}

/// Is the given hour timestamp during daylight? Match its date to the daily
/// sunrise/sunset and compare lexically (same ISO format → string order works).
fn day_at(hour: &str, sun_by_date: &[(String, String, String)]) -> bool {
    let date = hour.get(0..10).unwrap_or("");
    for (d, sunrise, sunset) in sun_by_date {
        if d == date {
            return hour >= sunrise.as_str() && hour < sunset.as_str();
        }
    }
    true
}

fn pop_str(pop: u8) -> String {
    if pop >= 20 {
        format!("{pop}%")
    } else {
        String::new()
    }
}

fn pad_right(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    while s.chars().count() < width {
        s.push(' ');
    }
    s
}

/// Temperature padded with a figure space so single- and double-digit values
/// right-align into a clean column. e.g. 9 → "\u{2007}9°", 19 → "19°".
fn fig_temp(t: f64) -> String {
    let n = t.round() as i64;
    let digits = n.abs().to_string();
    let pad = if n < 0 {
        format!("-{digits}")
    } else {
        digits
    };
    if pad.chars().count() < 2 {
        format!("\u{2007}{pad}\u{b0}")
    } else {
        format!("{pad}\u{b0}")
    }
}

/// Weekday abbreviation from an ISO date "YYYY-MM-DD" (Sakamoto's algorithm).
fn weekday(date: &str) -> &'static str {
    let y: i32 = date.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let m: usize = date.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let d: i32 = date.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = y;
    if m < 3 {
        y -= 1;
    }
    let w = (y + y / 4 - y / 100 + y / 400 + t[m - 1] + d).rem_euclid(7) as usize;
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][w]
}

export_plugin!(Weather);
