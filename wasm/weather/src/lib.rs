//! Example ezbar WASM plugin: a weather chip.
//!
//! Pulls REAL data from open-meteo (capability-gated) and renders it the way a
//! weather app should: a condition icon (day/night aware) + temperature in the
//! chip, and a clean forecast panel on hover — an hourly strip and a 4-day
//! outlook, all from the host icon set. No stock-style chart anywhere.
//!
//! Sources: open-meteo (primary) with a wttr.in fallback for when its daily quota is spent. The
//! location is `[modules.weather].city` — a name geocoded via open-meteo's geocoding API, or
//! "auto" (the default) which IP-geolocates via wttr.in; explicit `lat`/`lon` override. So grant
//! all three hosts:
//!   [modules.weather]
//!   network = ["api.open-meteo.com", "geocoding-api.open-meteo.com", "wttr.in"]
//!   city = "Berlin"   # optional; default "auto"
//!
//! Note how little there is: a `Plugin` impl + `export_plugin!`. No wit-bindgen,
//! no generated-type glue — the SDK owns all of that.

use ezbar_plugin_wasm::prelude::*;
use serde_json::Value;
use std::collections::HashMap;

struct HourPt {
    label: String,    // "15"
    day: String,      // "Mo" — two-letter weekday, for the day marker above the strip
    temp: f64,
    uv: f64,          // UV index — the dual-chart's right-axis series
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
    /// `[modules.weather].city`: "auto" (IP-geolocate, the default) or a place name to geocode.
    /// Ignored when explicit `lat`/`lon` are configured.
    city: String,
    /// `true` once lat/lon are known (explicit config, or resolved from `city`).
    located: bool,
    /// `[modules.weather].name` was set — an explicit label that the resolver must not overwrite.
    name_override: bool,
    /// `[modules.weather].model`: the open-meteo forecast model. Default `icon_seamless` (DWD's
    /// global ICON blend, high-res over central Europe) — the `best_match` blend was demonstrably
    /// wrong here (invented a storm). Override e.g. `best_match`, `gfs_seamless`, `ecmwf_ifs025`.
    model: String,
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
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            city: "auto".into(), // IP-geolocate unless [modules.weather].city / lat+lon say otherwise
            located: false,
            name_override: false,
            model: "icon_seamless".into(), // DWD ICON forecast model (best_match was unreliable)
            place: String::new(),
            lat: String::new(), // resolved from `city`/auto on the first tick — no hardcoded default
            lon: String::new(),
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
        }
    }
}

// Event-driven cadence (RFC 0011): weather changes slowly and open-meteo's free tier
// rate-limits (HTTP 429), so we drive our own clock instead of the host heartbeat —
// ~15 min between good refreshes, ~2 min retry on error (gentle enough to let a tripped
// rate-limit recover instead of hammering it).
const REFRESH_MS: u32 = 15 * 60 * 1000;
const RETRY_MS: u32 = 2 * 60 * 1000;

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
        let (mut lat_set, mut lon_set) = (false, false);
        for (k, v) in &config {
            match k.as_str() {
                "lat" => {
                    self.lat = v.clone();
                    lat_set = true;
                }
                "lon" => {
                    self.lon = v.clone();
                    lon_set = true;
                }
                "city" => self.city = v.clone(),
                "model" => self.model = v.clone(),
                "name" => {
                    self.place = v.clone();
                    self.name_override = true;
                }
                _ => {}
            }
        }
        if lat_set && lon_set {
            // Explicit coordinates win — skip geocoding/geolocation.
            self.located = true;
            if !self.name_override {
                self.place = format!("{}, {}", self.lat, self.lon);
            }
        }
        // Else `located` stays false: the first tick resolves `city` (default "auto") into lat/lon
        // and a place label (unless `name` already pinned one).
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        // Resolve lat/lon first: geocode the configured `city`, or IP-geolocate when it's "auto".
        // Until that succeeds we have no coordinates to query, so retry on the short cadence.
        if !self.located && !self.resolve_location(ctx) {
            ctx.set_timeout(RETRY_MS);
            return false;
        }
        // Primary source is open-meteo (richer data, WMO codes). Fall back to wttr.in when
        // it's unavailable — e.g. open-meteo's daily quota is spent. Re-arm the next tick
        // *unconditionally* (RFC 0011 one-shot timer): a longer cadence on good data, a
        // shorter retry on error — never leave ourselves un-armed.
        let ok = self.fetch_open_meteo(ctx) || self.fetch_wttr(ctx);
        // Override the CURRENT block with the nearest DWD station's MEASURED observation (Bright
        // Sky). Models forecast a *grid point* and can be plain wrong for "now"; this is a real
        // station reading. Best-effort: a no-op where there's no nearby station (outside DE/EU),
        // leaving the model's current in place.
        self.fetch_measured_current(ctx);
        ctx.set_timeout(if ok { REFRESH_MS } else { RETRY_MS });
        ok
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
                    self.temp_uv_chart(),
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
    /// Resolve `lat`/`lon` once: geocode the configured `city`, or — for "auto" — let the source
    /// detect our location from our IP. Sets `place` to the resolved name unless `name` pinned an
    /// explicit label. Returns whether we now have coordinates.
    fn resolve_location(&mut self, ctx: &mut dyn Ctx) -> bool {
        let ok = if self.city.eq_ignore_ascii_case("auto") {
            self.geolocate_auto(ctx)
        } else {
            self.geocode(ctx)
        };
        self.located = ok;
        ok
    }

    /// Geocode `self.city` to coordinates via open-meteo's geocoding API.
    fn geocode(&mut self, ctx: &mut dyn Ctx) -> bool {
        let url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
            urlencode(&self.city)
        );
        let Ok(bytes) = ctx.http_get(&url) else {
            return false;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let r = &v["results"][0];
        let (Some(lat), Some(lon)) = (r["latitude"].as_f64(), r["longitude"].as_f64()) else {
            ctx.log(&format!("weather: city {:?} not found", self.city));
            return false;
        };
        self.lat = format!("{lat:.4}");
        self.lon = format!("{lon:.4}");
        if !self.name_override {
            self.place = r["name"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| self.city.clone());
        }
        true
    }

    /// IP-geolocate via wttr.in (no location ⇒ it uses our IP), reading the nearest area's name +
    /// coordinates; the richer open-meteo fetch then runs against those coordinates.
    fn geolocate_auto(&mut self, ctx: &mut dyn Ctx) -> bool {
        let Ok(bytes) = ctx.http_get("https://wttr.in/?format=j1") else {
            return false;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let area = &v["nearest_area"][0];
        let (Some(lat), Some(lon)) = (area["latitude"].as_str(), area["longitude"].as_str()) else {
            return false;
        };
        self.lat = lat.to_string();
        self.lon = lon.to_string();
        if !self.name_override {
            self.place = area["areaName"][0]["value"]
                .as_str()
                .unwrap_or("Current location")
                .to_string();
        }
        true
    }

    /// Fetch + parse open-meteo (the primary source). Returns false (so the caller
    /// can fall back) on any network error, including a 429 daily-quota response.
    fn fetch_open_meteo(&mut self, ctx: &mut dyn Ctx) -> bool {
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
             &current=temperature_2m,apparent_temperature,weathercode,is_day,windspeed_10m,relative_humidity_2m,precipitation\
             &hourly=temperature_2m,weathercode,precipitation_probability,uv_index\
             &daily=weathercode,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset\
             &forecast_days=4&models={}&timezone=auto",
            self.lat, self.lon, self.model
        );
        match ctx.http_get(&url) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) if v["current"].is_object() => {
                    let uv = self.fetch_uv(ctx);
                    self.ingest(&v, &uv);
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

    /// UV index `timestamp -> value`, fetched WITHOUT a model pin. The chip's pinned regional model
    /// (DWD ICON) doesn't compute UV — it's a global product — so a tiny separate request to
    /// open-meteo's default model fills it. Keyed by ISO timestamp so it aligns with whatever hours
    /// `ingest` picks. Empty map on any failure (the chart just omits the UV line).
    fn fetch_uv(&self, ctx: &mut dyn Ctx) -> HashMap<String, f64> {
        let mut map = HashMap::new();
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
             &hourly=uv_index&forecast_days=4&timezone=auto",
            self.lat, self.lon
        );
        if let Ok(bytes) = ctx.http_get(&url) {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                let h = &v["hourly"];
                if let (Some(times), Some(uvs)) = (h["time"].as_array(), h["uv_index"].as_array()) {
                    for (t, u) in times.iter().zip(uvs.iter()) {
                        if let (Some(ts), Some(uv)) = (t.as_str(), u.as_f64()) {
                            map.insert(ts.to_string(), uv);
                        }
                    }
                }
            }
        }
        map
    }

    /// Override the current block with the nearest DWD station's MEASURED reading via Bright Sky
    /// (api.brightsky.dev / DWD open data). A no-op (keeps the model's current) when there's no
    /// station near `lat/lon` — Bright Sky covers Germany and parts of Europe. This is the
    /// "actually measured, not modelled" current the chip shows.
    fn fetch_measured_current(&mut self, ctx: &mut dyn Ctx) -> bool {
        let url = format!(
            "https://api.brightsky.dev/current_weather?lat={}&lon={}",
            self.lat, self.lon
        );
        let Ok(bytes) = ctx.http_get(&url) else {
            return false;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let w = &v["weather"];
        let Some(temp) = w["temperature"].as_f64() else {
            return false; // no nearby station → keep the model's current
        };
        self.temp = temp;
        if let Some(h) = w["relative_humidity"].as_f64() {
            self.humidity = h;
        }
        if let Some(ws) = w["wind_speed_10"].as_f64() {
            self.wind = ws;
        }
        let icon = w["icon"].as_str().unwrap_or("");
        if !icon.is_empty() {
            self.is_day = !icon.ends_with("-night");
        }
        self.code = brightsky_code(
            w["condition"].as_str().unwrap_or("dry"),
            w["cloud_cover"].as_f64().unwrap_or(0.0),
        );
        // Bright Sky reports no apparent temperature; keep the model's `feels` (a close estimate).
        true
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
                    label: if i == 0 {
                        "Today".into()
                    } else {
                        weekday(date).into()
                    },
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
                let is_day = sun
                    .get(di)
                    .map(|(sr, ss)| hour >= *sr && hour < *ss)
                    .unwrap_or(true);
                self.hours.push(HourPt {
                    label: format!("{hour:02}"),
                    day: short_weekday(ds.get(di).and_then(|d| d["date"].as_str()).unwrap_or("")),
                    temp: sf(&slot["tempC"]),
                    uv: sf(&slot["uvIndex"]),
                    code: wwo_to_wmo(su(&slot["weatherCode"])),
                    pop: slot["chanceofrain"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    is_day,
                });
            }
        }
        self.loaded = true;
    }

    fn ingest(&mut self, v: &Value, uv_by_time: &HashMap<String, f64>) {
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
                    label: if i == 0 {
                        "Today".into()
                    } else {
                        weekday(&date).into()
                    },
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

        // hourly: 6 slots at 2-hour spacing from now (≈12h of coverage, not 6).
        let h = &v["hourly"];
        self.hours.clear();
        if let Some(htime) = h["time"].as_array() {
            let start = htime
                .iter()
                .position(|t| t.as_str().unwrap_or("") > now)
                .unwrap_or(0);
            for i in (start..htime.len()).step_by(2).take(6) {
                let t = htime[i].as_str().unwrap_or("");
                self.hours.push(HourPt {
                    label: t.get(11..13).unwrap_or("").to_string(),
                    day: short_weekday(t.get(0..10).unwrap_or("")),
                    temp: arr_f64(h, "temperature_2m", i),
                    uv: uv_by_time.get(t).copied().unwrap_or(0.0),
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
            text(format!("{:.0}\u{b0}", self.temp))
                .color(temp_color(self.temp))
                .size(HERO_TEMP),
            text(condition_label(self.code))
                .color(Token::FgDim)
                .size(BODY),
        ])
        .spacing(6.0)
        .align(Align::End);

        let hero = row([
            wmo_icon(self.code, self.is_day).view(HERO_ICON, sky_tint(self.code, self.is_day)),
            column([
                temp_line,
                text(format!(
                    "Feels {:.0}\u{b0}  \u{b7}  {}",
                    self.feels, self.place
                ))
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
            metric(
                Icon::Droplets,
                Token::Accent,
                format!("{:.0}%", self.humidity),
            ),
            metric(Icon::Wind, Token::FgDim, format!("{:.0} km/h", self.wind)),
            metric(sun_icon, sun_tint, self.sun_label.clone()),
        ])
        .spacing(14.0)
        .align(Align::Center);

        column([hero, metrics]).spacing(8.0)
    }

    /// Temperature (yellow, left axis) + UV index (blue, right axis) across the hourly window — a
    /// dual-axis line chart above the hourly strip, each series auto-scaled to its own range so the
    /// trends read at a glance. The x-axis lines up with the hours below.
    fn temp_uv_chart(&self) -> Render {
        let temps: Vec<f64> = self.hours.iter().map(|h| h.temp).collect();
        let uvs: Vec<f64> = self.hours.iter().map(|h| h.uv).collect();
        let n = temps.len();
        // Label a handful of points on each band so the values read off the curve like a weather app
        // (not just the one endpoint). Temp keeps the °; UV tags its first mark "UV n" for identity,
        // then bare numbers.
        let mut a_labels = vec![String::new(); n];
        let mut b_labels = vec![String::new(); n];
        let mut first_uv = true;
        let mut zero_labelled = false;
        for i in label_marks(n) {
            a_labels[i] = format!("{:.0}\u{b0}", temps[i]);
            let uv = uvs[i];
            // Don't re-label a flat run of zeros (the evening tail) — one 0 says it all.
            if uv < 0.5 && zero_labelled {
                continue;
            }
            if uv < 0.5 {
                zero_labelled = true;
            }
            b_labels[i] = if first_uv {
                first_uv = false;
                format!("UV {uv:.0}")
            } else {
                format!("{uv:.0}")
            };
        }
        DualChart {
            a_values: temps,
            a_line: Paint::Rgba(245, 205, 90, 255), // temperature — warm yellow (top band)
            a_labels,
            b_values: uvs,
            b_line: Paint::Rgba(185, 135, 255, 255), // UV index — violet (bottom band)
            b_labels,
            width: 232.0,
            height: 88.0,
        }
        .view()
    }

    fn hourly_strip(&self) -> Render {
        let cols: Vec<Render> = self
            .hours
            .iter()
            .enumerate()
            .map(|(i, h)| {
                // Day marker above the hour — shown for the first hour and at each day rollover, so
                // a strip that crosses midnight reads "Mo … Tu …" instead of an ambiguous run of
                // hours. (A blank space on same-day columns keeps every column the same height.)
                let new_day = i == 0 || self.hours[i - 1].day != h.day;
                let day = if new_day { h.day.clone() } else { " ".to_string() };
                column([
                    // A quiet wayfinding label, not data: muted fg (NOT accent — accent is reserved
                    // for the precip% below, and the icons are already blue), centered over the
                    // column like the rest of the cell.
                    text(day).color(Token::FgDim).size(TINY),
                    text(h.label.clone()).color(Token::FgDim).size(SMALL),
                    wmo_icon(h.code, h.is_day).view(HOUR_ICON, sky_tint(h.code, h.is_day)),
                    text(format!("{:.0}\u{b0}", h.temp))
                        .color(temp_color(h.temp))
                        .size(BODY),
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

/// Percent-encode a query value (e.g. a city name with spaces or umlauts) for the geocoding URL.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Map a Bright Sky `condition` (+ cloud-cover %) to the WMO weather code the icon set expects,
/// so a measured "dry/rain/…" observation picks the same glyph as the open-meteo forecast path.
fn brightsky_code(condition: &str, cloud_cover: f64) -> u8 {
    match condition {
        "fog" => 45,
        "rain" => 61,
        "sleet" => 66,
        "snow" => 71,
        "hail" => 96,
        "thunderstorm" => 95,
        // "dry": clear / partly cloudy / overcast, by cloud cover.
        _ if cloud_cover < 12.5 => 0,
        _ if cloud_cover < 50.0 => 2,
        _ => 3,
    }
}

fn metric(icon: Icon, tint: Token, label: String) -> Render {
    row([
        icon.view(SMALL, tint),
        text(label).color(Token::FgDim).size(SMALL),
    ])
    .spacing(4.0)
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
/// Indices to label on the chart — three evenly-spaced marks (start / third / two-thirds), an even
/// rhythm with no crammed endpoint pair. The leftmost (index 0) is "now".
fn label_marks(n: usize) -> Vec<usize> {
    match n {
        0 => vec![],
        1 | 2 => (0..n).collect(),
        _ => {
            let mut m = vec![0, n / 3, (2 * n) / 3];
            m.dedup();
            m
        }
    }
}

fn temp_color(t: f64) -> Token {
    // Colour the *displayed* (rounded) value — otherwise 25.6 and 26.4 both render "26°" but in
    // different colours (white vs warn), which reads as a bug side by side.
    let t = t.round();
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
    obj[key]
        .as_array()
        .and_then(|a| a.get(i))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0)
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
    let pad = if n < 0 { format!("-{digits}") } else { digits };
    if pad.chars().count() < 2 {
        format!("\u{2007}{pad}\u{b0}")
    } else {
        format!("{pad}\u{b0}")
    }
}

/// Two-letter weekday ("Mo", "Tu", …) from an ISO date — the compact day marker for the hourly
/// strip (which can cross midnight, so each day's first hour gets labelled).
fn short_weekday(date: &str) -> String {
    weekday(date).chars().take(2).collect()
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
        113 => 0,                                // clear / sunny
        116 => 2,                                // partly cloudy
        119 | 122 => 3,                          // cloudy / overcast
        143 | 248 | 260 => 45,                   // mist / fog
        176 | 263 | 266 | 293 | 296 | 353 => 61, // patchy/light rain & drizzle
        299 | 302 | 356 => 63,                   // moderate rain
        305 | 308 | 359 => 65,                   // heavy rain
        // sleet / freezing rain / ice pellets
        182 | 185 | 281 | 284 | 311 | 314 | 317 | 320 | 350 | 362 | 365 | 374 | 377 => 66,
        179 | 227 | 323 | 326 | 329 | 332 | 368 | 371 => 71, // snow
        230 | 335 | 338 => 75,                               // heavy snow / blizzard
        200 | 386 | 389 | 392 | 395 => 95,                   // thunder
        _ => 3,                                              // default: cloudy
    }
}

export_plugin!(Weather);
