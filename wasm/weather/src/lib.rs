//! Example ezbar WASM plugin: a weather chip.
//!
//! Pulls REAL data from open-meteo (capability-gated), draws a GPU sparkline of
//! the hourly forecast in the chip, and the full smoothed area chart on hover.
//!
//! Note how little there is: a `Plugin` impl + `export_plugin!`. No wit-bindgen,
//! no generated-type glue — the SDK owns all of that.

use ezbar_plugin_wasm::{
    export_plugin, widget::*, Chart, Ctx, Event, Graph, GraphKind, Icon, Plugin, Render, Token,
};

struct Weather {
    temp: Option<f64>, // current temperature (°C)
    series: Vec<f64>,  // hourly forecast — the sparkline / popup chart
    lat: String,
    lon: String,
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            temp: None,
            series: Vec::new(),
            lat: "52.52".into(), // Berlin; override via [modules.weather].lat/lon
            lon: "13.41".into(),
        }
    }
}

impl Plugin for Weather {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            match k.as_str() {
                "lat" => self.lat = v.clone(),
                "lon" => self.lon = v.clone(),
                _ => {}
            }
        }
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
             &current=temperature_2m&hourly=temperature_2m&forecast_days=1",
            self.lat, self.lon
        );
        match ctx.http_get(&url) {
            Ok(bytes) => {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    self.temp = v["current"]["temperature_2m"].as_f64();
                    if let Some(a) = v["hourly"]["temperature_2m"].as_array() {
                        self.series = a.iter().filter_map(|x| x.as_f64()).collect();
                    }
                }
                true
            }
            Err(e) => {
                ctx.log(&format!("weather: {e}"));
                false
            }
        }
    }

    fn view(&self) -> Render {
        let label = self
            .temp
            .map(|t| format!("{t:.1}\u{b0}C"))
            .unwrap_or_else(|| "\u{2026}".into());
        let color = match self.temp {
            Some(t) if t >= 25.0 => Token::Urgent,
            Some(t) if t >= 15.0 => Token::Warn,
            Some(_) => Token::Accent,
            None => Token::FgDim,
        };
        let mut items = vec![Icon::Cloud.view(14.0, Token::Fg), text(label).color(color)];
        if self.series.len() >= 2 {
            items.push(
                Graph {
                    values: self.series.clone(),
                    kind: GraphKind::Temperature,
                    line: Token::Accent.into(),
                }
                .view(),
            );
        }
        row(items).spacing(6.0)
    }

    fn popup(&self) -> Option<Render> {
        if self.series.len() < 2 {
            return None;
        }
        Some(
            container(
                column([
                    text(format!("Weather \u{2014} next {}h", self.series.len())).color(Token::Fg),
                    Chart {
                        values: self.series.clone(),
                        line: Token::Accent.into(),
                        width: 280.0,
                        height: 100.0,
                    }
                    .view(),
                ])
                .spacing(6.0),
            )
            .padding(10.0),
        )
    }
}

export_plugin!(Weather);
