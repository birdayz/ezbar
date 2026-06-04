//! ezbar WASM plugin: quakes — recent-earthquake monitor.
//!
//! Pulls REAL data from the USGS GeoJSON summary feed (capability-gated, no
//! auth), shows the quake count + largest magnitude in the chip with a sparkline
//! of recent magnitudes, and a list of the strongest recent quakes on hover.
//!
//! Note how little there is: a `Plugin` impl + `export_plugin!`. No wit-bindgen,
//! no generated-type glue — the SDK owns all of that.

use ezbar_plugin_wasm::prelude::*;

/// One recent earthquake, distilled from a GeoJSON feature.
#[derive(Clone)]
struct Quake {
    mag: f64,
    place: String,
}

struct Quakes {
    feed: String,       // USGS feed id: significant | 4.5 | 2.5 | 1.0 | all
    quakes: Vec<Quake>, // most-recent-first, as the feed delivers them
    mags: Vec<f64>,     // chronological magnitudes — the sparkline series
    err: bool,          // last fetch failed
}

impl Default for Quakes {
    fn default() -> Self {
        Quakes {
            feed: "2.5".into(), // M2.5+/day: enough events for a sparkline
            quakes: Vec::new(),
            mags: Vec::new(),
            err: false,
        }
    }
}

impl Quakes {
    /// Largest magnitude in the current window, if any.
    fn peak(&self) -> Option<f64> {
        self.quakes
            .iter()
            .map(|q| q.mag)
            .fold(None, |acc, m| Some(acc.map_or(m, |a: f64| a.max(m))))
    }
}

/// Magnitude → theme token (calm Accent up to a loud Urgent for the big ones).
fn mag_color(mag: f64) -> Token {
    if mag >= 6.0 {
        Token::Urgent
    } else if mag >= 4.5 {
        Token::Warn
    } else {
        Token::Accent
    }
}

impl Plugin for Quakes {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            if k == "feed" {
                self.feed = v.clone();
            }
        }
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        let url = format!(
            "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/{}_day.geojson",
            self.feed
        );
        match ctx.http_get(&url) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(v) => {
                    let mut quakes = Vec::new();
                    if let Some(features) = v["features"].as_array() {
                        for f in features {
                            let p = &f["properties"];
                            // `mag` can be null for some events — skip those.
                            let Some(mag) = p["mag"].as_f64() else { continue };
                            let place = p["place"].as_str().unwrap_or("unknown").to_string();
                            quakes.push(Quake { mag, place });
                        }
                    }
                    // Feed is newest-first; reverse for a left-to-right-in-time spark.
                    self.mags = quakes.iter().rev().map(|q| q.mag).collect();
                    self.quakes = quakes;
                    self.err = false;
                }
                Err(_) => {
                    ctx.log("quakes: malformed GeoJSON");
                    self.err = true;
                }
            },
            Err(e) => {
                ctx.log(&format!("quakes: {e}"));
                self.err = true;
            }
        }
        // Drive our own poll clock (RFC 0011) rather than the host's 2 s heartbeat — quakes
        // trickle in, so refresh ~every 2 min and retry sooner on error. Re-armed on every
        // path (one-shot timer); we always re-render (the chip shows a `!` on error).
        ctx.set_timeout(if self.err { 60_000 } else { 120_000 });
        true
    }

    fn view(&self) -> Render {
        // No data yet, or the fetch failed: a quiet placeholder.
        if self.quakes.is_empty() {
            let label = if self.err { "!" } else { "\u{2026}" };
            return row([
                Icon::Alert.view(14.0, Token::FgDim),
                text(label).color(Token::FgDim),
            ])
            .spacing(5.0);
        }

        let peak = self.peak().unwrap_or(0.0);
        let color = mag_color(peak);
        let label = format!("{} \u{2022} M{:.1}", self.quakes.len(), peak);

        let mut items = vec![Icon::Alert.view(14.0, color), text(label).color(color)];
        if self.mags.len() >= 2 {
            items.push(
                Graph {
                    values: self.mags.clone(),
                    kind: GraphKind::Generic, // magnitudes: let the host fit min/max
                    line: color.into(),
                }
                .view(),
            );
        }
        row(items).spacing(6.0)
    }

    fn popup(&self) -> Option<Render> {
        if self.quakes.is_empty() {
            return None;
        }
        // Strongest few, by magnitude.
        let mut top = self.quakes.clone();
        top.sort_by(|a, b| b.mag.partial_cmp(&a.mag).unwrap_or(std::cmp::Ordering::Equal));
        top.truncate(6);

        let mut rows = vec![text(format!(
            "Quakes \u{2014} {} in last 24h (M{}+)",
            self.quakes.len(),
            self.feed
        ))
        .color(Token::Fg)];

        for q in &top {
            rows.push(
                row([
                    text(format!("M{:.1}", q.mag))
                        .color(mag_color(q.mag))
                        .size(13.0),
                    text(q.place.clone()).color(Token::FgDim).size(13.0),
                ])
                .spacing(8.0),
            );
        }

        if self.mags.len() >= 2 {
            rows.push(
                Chart {
                    values: self.mags.clone(),
                    line: Token::Accent.into(),
                    width: 280.0,
                    height: 90.0,
                }
                .view(),
            );
        }

        Some(container(column(rows).spacing(6.0)).padding(10.0))
    }
}

export_plugin!(Quakes);
