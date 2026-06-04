//! ezbar WASM plugin: a crypto price ticker.
//!
//! Polls Coinbase's public spot-price endpoint for a pair (default BTC-USD),
//! draws a sparkline of recent samples in the chip, colours the price green/red
//! by the last move, and shows a bigger area chart on hover. Scroll the chip to
//! cycle the watchlist; click to toggle the % change readout.

use ezbar_plugin_wasm::{
    export_plugin, widget::*, Chart, Ctx, Event, Graph, GraphKind, Icon, PointerKind, Plugin,
    Render, Token,
};

struct Ticker {
    pairs: Vec<String>, // watchlist, e.g. ["BTC-USD", "ETH-USD"]
    idx: usize,         // currently shown pair
    price: Option<f64>, // latest spot price
    prev: Option<f64>,  // previous price (for up/down colouring)
    history: Vec<f64>,  // recent samples -> sparkline + popup chart
    show_pct: bool,     // click toggles the % readout
}

impl Default for Ticker {
    fn default() -> Self {
        Ticker {
            pairs: vec!["BTC-USD".into()],
            idx: 0,
            price: None,
            prev: None,
            history: Vec::new(),
            show_pct: false,
        }
    }
}

impl Ticker {
    fn pair(&self) -> &str {
        &self.pairs[self.idx]
    }

    fn fetch(&mut self, ctx: &mut dyn Ctx) -> bool {
        let url = format!(
            "https://api.coinbase.com/v2/prices/{}/spot",
            self.pair()
        );
        match ctx.http_get(&url) {
            Ok(bytes) => {
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    return false;
                };
                // { "data": { "amount": "67000.12", "base": "BTC", ... } }
                let amount = v["data"]["amount"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok());
                if let Some(p) = amount {
                    self.prev = self.price;
                    self.price = Some(p);
                    self.history.push(p);
                    if self.history.len() > 64 {
                        self.history.remove(0);
                    }
                    return true;
                }
                false
            }
            Err(e) => {
                ctx.log(&format!("btc: {e}"));
                false
            }
        }
    }
}

impl Plugin for Ticker {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            if k == "pairs" {
                let list: Vec<String> = v
                    .split(',')
                    .map(|s| s.trim().to_uppercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !list.is_empty() {
                    self.pairs = list;
                }
            }
        }
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            // Drive our own poll clock (RFC 0011) instead of the host's 2 s heartbeat —
            // a spot price every 30 s is plenty and stops us hammering Coinbase. Re-arm
            // unconditionally (one-shot timer): a gentle 60 s backoff on a failed fetch.
            Event::Timer => {
                let ok = self.fetch(ctx);
                ctx.set_timeout(if ok { 30_000 } else { 60_000 });
                ok
            }
            Event::Pointer { id, kind, delta } if id == "chip" => match kind {
                PointerKind::Scroll => {
                    let n = self.pairs.len();
                    if delta > 0.0 {
                        self.idx = (self.idx + 1) % n;
                    } else {
                        self.idx = (self.idx + n - 1) % n;
                    }
                    // reset series for the new pair, then fetch immediately
                    self.price = None;
                    self.prev = None;
                    self.history.clear();
                    self.fetch(ctx);
                    true
                }
                PointerKind::Press => {
                    self.show_pct = !self.show_pct;
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn view(&self) -> Render {
        let base = self.pair().split('-').next().unwrap_or(self.pair());

        let (label, color) = match self.price {
            Some(p) => {
                let color = match (self.prev, self.price) {
                    (Some(a), Some(b)) if b > a => Token::Ok,
                    (Some(a), Some(b)) if b < a => Token::Urgent,
                    _ => Token::Fg,
                };
                let text = if self.show_pct {
                    match self.prev {
                        Some(a) if a != 0.0 => format!("{:+.2}%", (p - a) / a * 100.0),
                        _ => format!("${p:.0}"),
                    }
                } else {
                    format!("${p:.0}")
                };
                (text, color)
            }
            None => ("\u{2026}".into(), Token::FgDim),
        };

        let mut items = vec![
            Icon::Dot.view(14.0, color),
            text(base.to_string()).color(Token::FgDim),
            text(label).color(color),
        ];
        if self.history.len() >= 2 {
            items.push(
                Graph {
                    values: self.history.clone(),
                    kind: GraphKind::Generic,
                    line: color.into(),
                }
                .view(),
            );
        }

        mouse_area("chip", row(items).spacing(6.0))
    }

    fn popup(&self) -> Option<Render> {
        if self.history.len() < 2 {
            return None;
        }
        Some(
            container(
                column([
                    text(format!("{} \u{2014} {} samples", self.pair(), self.history.len()))
                        .color(Token::Fg),
                    Chart {
                        values: self.history.clone(),
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

export_plugin!(Ticker);
