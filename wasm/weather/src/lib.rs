//! Example ezbar WASM plugin: a weather chip.
//!
//! Pulls REAL data from open-meteo (capability-gated) and renders it the way a
//! weather app should: a condition icon (day/night aware) + temperature in the
//! chip, and a clean forecast panel on hover — an hourly strip and a 4-day
//! outlook, all from the host icon set. No stock-style chart anywhere.
//!
//! Sources: open-meteo (primary) with a wttr.in fallback for when its daily quota
//! is spent — so grant BOTH hosts:
//!   [modules.weather]
//!   network = ["api.open-meteo.com", "wttr.in"]
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
    sun_label: String, // "06:14" — next sunrise or today's sunset
    before_dawn: bool, // show a sunrise icon vs a sunset icon
    hours: Vec<HourPt>,
    days: Vec<DayPt>,
    loaded: bool,
    // Throttle: the host ticks us ~every 2s, but weather changes slowly and
    // open-meteo's free tier rate-limits (HTTP 429). `cooldown` counts ticks left
    // before the next fetch — refresh on a ~15-min cadence, back off gently on error.
    cooldown: u32,
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
            sun_label: String::new(),
            before_dawn: false,
            hours: Vec::new(),
            days: Vec::new(),
            loaded: false,
            cooldown: 0, // fetch on the first tick
        }
    }
}

// Roughly 2s per host tick. ~15 min between good refreshes; ~2 min retry on error
// (gentle enough to let a tripped rate-limit recover instead of hammering it).
const REFRESH_TICKS: u32 = 450;
const RETRY_TICKS: u32 = 60;

// ── type/icon scale ─────────────────────────────────────────────────────────
// One base unit drives the whole widget; every icon and text size below is a
// ratio of it, so changing BASE rescales the chip and popup coherently. The
// ratios are tuned so BASE = 14 reproduces the hand-tuned look exactly.
const BASE: f32 = 14.0; // chip icon + temperature — the unit everything scales from
const HERO_ICON: f32 = BASE * 2.43; // ≈34  popup condition hero
const HERO_TEMP: f32 = BASE * 2.14; // ≈30  popup big temperature
const HOUR_ICON: f32 = BASE * 1.43; // ≈20  hourly-strip icon
const DAY_ICON: f32 = BASE * 1.29; // ≈18  daily-row icon
const BODY: f32 = BASE * 0.93; // ≈13  condition label, row temperatures
const LABEL: f32 = BASE * 0.86; // ≈12  daily weekday + daily precip
const SMALL: f32 = BASE * 0.79; // ≈11  secondary text + metric icons
const TINY: f32 = BASE * 0.71; // ≈10  hourly precip %
const HAIR: f32 = BASE * 0.5; // ≈7   divider hairline

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
        // Throttle: only fetch when the cooldown has elapsed (weather is slow and
        // the APIs rate-limit). Every other tick is a no-op.
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return false;
        }
        // Primary source is open-meteo (richer data, WMO codes). Fall back to
        // wttr.in when it's unavailable — e.g. open-meteo's daily quota is spent.
        if self.fetch_open_meteo(ctx) || self.fetch_wttr(ctx) {
            self.cooldown = REFRESH_TICKS;
            true
        } else {
            self.cooldown = RETRY_TICKS;
            false
        }
    }

    fn view(&self) -> Render {
        if !self.loaded {
            return row([
                Icon::Cloud.view(BASE, Token::FgDim),
                text("\u{2026}").color(Token::FgDim),
            ])
            .spacing(6.0);
        }
        // One coherent look: the condition icon + temp, and (when rain is likely) a
        // precip cluster that MATCHES the condition icon — same 14px size, same
        // tint — so the two icons read as a set, not a mismatch.
        let tint = sky_tint(self.code, self.is_day);
        let mut items = vec![
            wmo_icon(self.code, self.is_day).view(BASE, tint),
            text(format!("{:.0}\u{b0}", self.temp))
                .color(temp_color(self.temp))
                .size(BASE),
        ];
        let next_pop = self.hours.first().map(|h| h.pop).unwrap_or(0);
        if next_pop > 0 {
            items.push(
                row([
                    Icon::Droplets.view(BASE, tint),
                    text(format!("{next_pop}%")).color(tint).size(BASE),
                ])
                .spacing(4.0),
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
    /// Fetch + parse open-meteo (the primary source). Returns false (so the caller
    /// can fall back) on any network error, including a 429 daily-quota response.
    fn fetch_open_meteo(&mut self, ctx: &mut dyn Ctx) -> bool {
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
                Ok(v) if v["current"].is_object() => {
                    self.ingest(&v);
                    true
                }
                _ => false, // error body (e.g. quota exceeded) — let the fallback try
            },
            Err(e) => {
                ctx.log(&format!("weather: open-meteo {e}"));
                false
            }
        }
    }

    /// Fallback source: wttr.in (`j1` JSON). Different shape — WWO codes, string
    /// values, AM/PM times, 3-hour hourly steps — mapped onto the same struct.
    fn fetch_wttr(&mut self, ctx: &mut dyn Ctx) -> bool {
        let url = format!("https://wttr.in/{},{}?format=j1", self.lat, self.lon);
        match ctx.http_get(&url) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) if v["current_condition"].is_array() => {
                    self.ingest_wttr(&v);
                    true
                }
                _ => {
                    ctx.log("weather: wttr.in parse failed");
                    false
                }
            },
            Err(e) => {
                ctx.log(&format!("weather: wttr.in {e}"));
                false
            }
        }
    }

    /// Parse wttr.in's `j1` payload into the same fields `ingest` fills.
    fn ingest_wttr(&mut self, v: &Value) {
        let cur = &v["current_condition"][0];
        self.temp = sf(&cur["temp_C"]);
        self.feels = sf(&cur["FeelsLikeC"]);
        self.code = wwo_to_wmo(su(&cur["weatherCode"]));
        self.wind = sf(&cur["windspeedKmph"]);
        self.humidity = sf(&cur["humidity"]);
        let now_h = ampm_hour(cur["observation_time"].as_str().unwrap_or("12:00 PM"));

        // daily (today + up to 3) + a date→(sunrise,sunset hour) table.
        let days = v["weather"].as_array();
        let mut sun: Vec<(u32, u32)> = Vec::new(); // per-day (sunrise_h, sunset_h)
        self.days.clear();
        if let Some(ds) = days {
            for (i, d) in ds.iter().enumerate() {
                let date = d["date"].as_str().unwrap_or("");
                let astro = &d["astronomy"][0];
                let sr = ampm_hour(astro["sunrise"].as_str().unwrap_or("06:00 AM"));
                let ss = ampm_hour(astro["sunset"].as_str().unwrap_or("06:00 PM"));
                sun.push((sr, ss));
                let hourly = d["hourly"].as_array();
                let day_code = hourly
                    .and_then(|h| h.iter().find(|x| x["time"].as_str() == Some("1200")))
                    .map(|x| wwo_to_wmo(su(&x["weatherCode"])))
                    .unwrap_or(self.code);
                let pop = hourly
                    .map(|h| {
                        h.iter()
                            .filter_map(|x| x["chanceofrain"].as_str()?.parse::<u8>().ok())
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                self.days.push(DayPt {
                    label: if i == 0 { "Today".into() } else { weekday(date).into() },
                    hi: sf(&d["maxtempC"]),
                    lo: sf(&d["mintempC"]),
                    code: day_code,
                    pop,
                });
            }
        }

        // today's sun for the metric line + the chip's day/night icon.
        if let Some((sr, ss)) = sun.first() {
            self.before_dawn = now_h < *sr;
            let h = if self.before_dawn { *sr } else { *ss };
            self.sun_label = format!("{h:02}:00");
            self.is_day = now_h >= *sr && now_h < *ss;
        }

        // hourly strip: the 3-hour slots from the current slot onward, next 6.
        self.hours.clear();
        if let Some(ds) = days {
            let mut slots: Vec<(usize, u32, &Value)> = Vec::new(); // (day, hour, slot)
            for (di, d) in ds.iter().enumerate() {
                if let Some(h) = d["hourly"].as_array() {
                    for slot in h {
                        slots.push((di, su(&slot["time"]) / 100, slot));
                    }
                }
            }
            let start = slots
                .iter()
                .position(|(di, hour, _)| *di == 0 && *hour >= now_h)
                .unwrap_or(0);
            for (di, hour, slot) in slots.into_iter().skip(start).take(6) {
                let is_day = sun.get(di).map(|(sr, ss)| hour >= *sr && hour < *ss).unwrap_or(true);
                self.hours.push(HourPt {
                    label: format!("{hour:02}"),
                    temp: sf(&slot["tempC"]),
                    code: wwo_to_wmo(su(&slot["weatherCode"])),
                    pop: slot["chanceofrain"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
                    is_day,
                });
            }
        }
        self.loaded = true;
    }

    fn ingest(&mut self, v: &Value) {
        let cur = &v["current"];
        self.temp = cur["temperature_2m"].as_f64().unwrap_or(0.0);
        self.feels = cur["apparent_temperature"].as_f64().unwrap_or(self.temp);
        self.code = cur["weathercode"].as_u64().unwrap_or(0) as u8;
        self.is_day = cur["is_day"].as_i64().unwrap_or(1) != 0;
        self.wind = cur["windspeed_10m"].as_f64().unwrap_or(0.0);
        self.humidity = cur["relative_humidity_2m"].as_f64().unwrap_or(0.0);
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
            text(format!("{:.0}\u{b0}", self.temp)).color(temp_color(self.temp)).size(HERO_TEMP),
            text(condition_label(self.code)).color(Token::FgDim).size(BODY),
        ])
        .spacing(6.0)
        .align(Align::End);

        let hero = row([
            wmo_icon(self.code, self.is_day).view(HERO_ICON, sky_tint(self.code, self.is_day)),
            column([
                temp_line,
                text(format!("Feels {:.0}\u{b0}  \u{b7}  {}", self.feels, self.place))
                    .color(Token::FgDim)
                    .size(SMALL),
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
                    text(h.label.clone()).color(Token::FgDim).size(SMALL),
                    wmo_icon(h.code, h.is_day).view(HOUR_ICON, sky_tint(h.code, h.is_day)),
                    text(format!("{:.0}\u{b0}", h.temp)).color(temp_color(h.temp)).size(BODY),
                    text(pop_str(h.pop)).color(Token::Accent).size(TINY),
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
                    text(fig_temp(d.hi)).color(temp_color(d.hi)).size(BODY),
                    text("/").color(Token::FgDim).size(BODY),
                    text(fig_temp(d.lo)).color(Token::FgDim).size(BODY),
                ])
                .spacing(1.0)
                .align(Align::Center);

                let mut cells = vec![
                    text(pad_right(&d.label, 5)).color(Token::Fg).size(LABEL),
                    wmo_icon(d.code, true).view(DAY_ICON, sky_tint(d.code, true)),
                    range,
                ];
                // precip demoted to the trailing edge (no second water glyph — the
                // condition icon already says rain); raggedness hides off the right.
                if d.pop >= 20 {
                    cells.push(spacer(8.0));
                    cells.push(text(format!("{}%", d.pop)).color(Token::Accent).size(LABEL));
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
    text("\u{2500}".repeat(48)).color(Token::FgDim).size(HAIR)
}

fn metric(icon: Icon, tint: Token, label: String) -> Render {
    row([icon.view(SMALL, tint), text(label).color(Token::FgDim).size(SMALL)]).spacing(4.0)
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

// ── wttr.in helpers (its j1 values are strings; codes are WWO, not WMO) ──────

/// Parse a stringy JSON number (wttr.in encodes everything as strings).
fn sf(v: &Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0)
}
fn su(v: &Value) -> u32 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// "05:17 AM" / "09:08 PM" → hour of day (0–23).
fn ampm_hour(s: &str) -> u32 {
    let s = s.trim();
    let hour: u32 = s
        .split(':')
        .next()
        .and_then(|h| h.trim().parse().ok())
        .unwrap_or(12);
    let pm = s.to_uppercase().contains("PM");
    match (hour % 12, pm) {
        (h, true) => h + 12,
        (h, false) => h,
    }
}

/// Map a WWO weather code (wttr.in) onto the closest WMO code, so the existing
/// `wmo_icon`/`condition_label` logic applies unchanged.
fn wwo_to_wmo(code: u32) -> u8 {
    match code {
        113 => 0,                                  // clear / sunny
        116 => 2,                                  // partly cloudy
        119 | 122 => 3,                            // cloudy / overcast
        143 | 248 | 260 => 45,                     // mist / fog
        176 | 263 | 266 | 293 | 296 | 353 => 61,   // patchy/light rain & drizzle
        299 | 302 | 356 => 63,                     // moderate rain
        305 | 308 | 359 => 65,                     // heavy rain
        // sleet / freezing rain / ice pellets
        182 | 185 | 281 | 284 | 311 | 314 | 317 | 320 | 350 | 362 | 365 | 374 | 377 => 66,
        179 | 227 | 323 | 326 | 329 | 332 | 368 | 371 => 71, // snow
        230 | 335 | 338 => 75,                     // heavy snow / blizzard
        200 | 386 | 389 | 392 | 395 => 95,         // thunder
        _ => 3,                                    // default: cloudy
    }
}

export_plugin!(Weather);
