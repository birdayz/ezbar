//! Example ezbar WASM plugin (RFC 0006 PoC): a weather chip.
//!
//! Author code = the `Plugin` impl + the `widget` DSL. The glue below bridges
//! the generated component `Guest` to that `Plugin` and lowers the DSL tree to
//! the WIT arena. (In the shipping SDK this glue becomes an `export_plugin!`
//! macro; spelled out here so the PoC is legible.)

use ezbar_plugin_wasm as sdk;
use sdk::{lower, widget::*, Chart, Graph, GraphKind, Icon, Plugin, Render, Token, WireNode};
use std::cell::RefCell;

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/since-v0.1.0",
});

/// The generated component bindings, namespaced (they name-clash with the SDK).
mod wit {
    pub use crate::ezbar::plugin::events::{Event, FeedKind, PointerKind};
    pub use crate::ezbar::plugin::types::{Align, IconId, Paint, Rgba8, ThemeToken};
    pub use crate::ezbar::plugin::ui::{
        BoxNode, ChartNode, GraphKind, GraphNode, HitNode, IconNode, LayoutNode, Node, TextNode,
        Tree,
    };
}

// ── the actual plugin (pure author code) ────────────────────────────────────

struct Weather {
    temp: Option<f64>,    // current temperature (°C) from the API
    series: Vec<f64>,     // hourly forecast — drawn as the sparkline / popup chart
    lat: String,
    lon: String,
    spin: bool, // demo hooks for the host's safety tests (`demo = spin|huge`)
    huge: bool,
}

impl Default for Weather {
    fn default() -> Self {
        Weather {
            temp: None,
            series: Vec::new(),
            lat: "52.52".into(), // Berlin, overridable via [modules.weather].lat/lon
            lon: "13.41".into(),
            spin: false,
            huge: false,
        }
    }
}

impl Plugin for Weather {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            match k.as_str() {
                "lat" => self.lat = v.clone(),
                "lon" => self.lon = v.clone(),
                "demo" => {
                    self.spin = v == "spin";
                    self.huge = v == "huge";
                }
                _ => {}
            }
        }
    }

    fn update(&mut self, ctx: &mut dyn sdk::Ctx, ev: sdk::Event) -> bool {
        match ev {
            sdk::Event::Timer => {
                // REAL data from the internet, from inside the sandbox — the host
                // performs the (capability-gated) fetch on the plugin's thread.
                let url = format!(
                    "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
                     &current=temperature_2m&hourly=temperature_2m&forecast_days=1",
                    self.lat, self.lon
                );
                match ctx.http_get(&url) {
                    Ok(bytes) => {
                        self.parse(&bytes);
                        true
                    }
                    Err(e) => {
                        ctx.log(&format!("weather: {e}"));
                        false
                    }
                }
            }
            _ => false,
        }
    }

    fn view(&self) -> Render {
        if self.spin {
            loop {
                std::hint::spin_loop();
            }
        }
        if self.huge {
            return column((0..5_000).map(|i| text(format!("{i}"))));
        }
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
            // a GPU sparkline of the forecast — the thing a shell script can't do.
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
        // on hover: the full-fidelity smoothed gradient area chart (stock-popup grade)
        Some(
            container(
                column([
                    text(format!("Weather \u{2014} next {}h", self.series.len()))
                        .color(Token::Fg),
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

impl Weather {
    fn parse(&mut self, bytes: &[u8]) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
            self.temp = v["current"]["temperature_2m"].as_f64();
            if let Some(a) = v["hourly"]["temperature_2m"].as_array() {
                self.series = a.iter().filter_map(|x| x.as_f64()).collect();
            }
        }
    }
}

/// Bridges the SDK `Ctx` to the generated host imports (capability-gated).
struct HostCtx;
impl sdk::Ctx for HostCtx {
    fn http_get(&mut self, url: &str) -> Result<Vec<u8>, String> {
        crate::ezbar::plugin::host::http_get(url)
    }
    fn log(&mut self, msg: &str) {
        crate::ezbar::plugin::host::log(msg);
    }
}

// ── glue: generated Guest -> Plugin, and SDK Render -> WIT tree ──────────────

thread_local! {
    static STATE: RefCell<Weather> = RefCell::new(Weather::default());
}

struct Component;

impl Guest for Component {
    fn init(config: Vec<(String, String)>) {
        STATE.with_borrow_mut(|w| w.load(config));
    }
    fn update(ev: wit::Event) -> bool {
        STATE.with_borrow_mut(|w| w.update(&mut HostCtx, from_wit_event(ev)))
    }
    fn view() -> wit::Tree {
        STATE.with_borrow(|w| to_wit_tree(&w.view()))
    }
    fn popup() -> Option<wit::Tree> {
        STATE.with_borrow(|w| w.popup().map(|r| to_wit_tree(&r)))
    }
    fn save_state() -> Vec<u8> {
        STATE.with_borrow(|w| w.save_state())
    }
    fn restore(state: Vec<u8>) {
        STATE.with_borrow_mut(|w| w.restore(state));
    }
}

export!(Component);

// ── type maps (mechanical SDK <-> generated bindings) ───────────────────────

fn to_wit_tree(r: &Render) -> wit::Tree {
    let (nodes, root) = lower(r);
    wit::Tree {
        nodes: nodes.iter().map(to_wit_node).collect(),
        root,
    }
}

fn to_wit_node(n: &WireNode) -> wit::Node {
    match n {
        WireNode::Text {
            content,
            color,
            size,
        } => wit::Node::Text(wit::TextNode {
            content: content.clone(),
            color: paint(*color),
            size: *size,
        }),
        WireNode::Row {
            children,
            spacing,
            align,
        } => wit::Node::Row(wit::LayoutNode {
            children: children.clone(),
            spacing: *spacing,
            align: align_(*align),
        }),
        WireNode::Column {
            children,
            spacing,
            align,
        } => wit::Node::Column(wit::LayoutNode {
            children: children.clone(),
            spacing: *spacing,
            align: align_(*align),
        }),
        WireNode::Container { child, padding } => wit::Node::Container(wit::BoxNode {
            child: *child,
            padding: *padding,
        }),
        WireNode::MouseArea { child, id } => wit::Node::MouseArea(wit::HitNode {
            child: *child,
            id: id.clone(),
        }),
        WireNode::Icon { id, color, size } => wit::Node::Icon(wit::IconNode {
            id: icon(*id),
            color: paint(*color),
            size: *size,
        }),
        WireNode::Graph { values, kind, line } => wit::Node::Graph(wit::GraphNode {
            values: values.clone(),
            kind: graph_kind(*kind),
            line: paint(*line),
        }),
        WireNode::Chart {
            values,
            line,
            width,
            height,
        } => wit::Node::Chart(wit::ChartNode {
            values: values.clone(),
            line: paint(*line),
            width: *width,
            height: *height,
        }),
        WireNode::Spacer(px) => wit::Node::Spacer(*px),
    }
}

fn paint(p: sdk::Paint) -> wit::Paint {
    match p {
        sdk::Paint::Token(t) => wit::Paint::Token(token(t)),
        sdk::Paint::Rgba(r, g, b, a) => wit::Paint::Rgba(wit::Rgba8 { r, g, b, a }),
    }
}
fn token(t: Token) -> wit::ThemeToken {
    match t {
        Token::Fg => wit::ThemeToken::Fg,
        Token::FgDim => wit::ThemeToken::FgDim,
        Token::Accent => wit::ThemeToken::Accent,
        Token::Ok => wit::ThemeToken::Ok,
        Token::Warn => wit::ThemeToken::Warn,
        Token::Urgent => wit::ThemeToken::Urgent,
        Token::Bg => wit::ThemeToken::Bg,
    }
}
fn align_(a: sdk::Align) -> wit::Align {
    match a {
        sdk::Align::Start => wit::Align::Start,
        sdk::Align::Center => wit::Align::Center,
        sdk::Align::End => wit::Align::End,
    }
}
fn icon(i: Icon) -> wit::IconId {
    use Icon::*;
    match i {
        Cpu => wit::IconId::Cpu,
        Memory => wit::IconId::Memory,
        Temperature => wit::IconId::Temperature,
        Ping => wit::IconId::Ping,
        VolumeHigh => wit::IconId::VolumeHigh,
        VolumeMedium => wit::IconId::VolumeMedium,
        VolumeMute => wit::IconId::VolumeMute,
        Battery => wit::IconId::Battery,
        BatteryCharging => wit::IconId::BatteryCharging,
        BatteryWarning => wit::IconId::BatteryWarning,
        Bot => wit::IconId::Bot,
        Github => wit::IconId::Github,
        Spotify => wit::IconId::Spotify,
        Kubernetes => wit::IconId::Kubernetes,
        Clock => wit::IconId::Clock,
        Calendar => wit::IconId::Calendar,
        Disk => wit::IconId::Disk,
        Net => wit::IconId::Net,
        Ip => wit::IconId::Ip,
        Updates => wit::IconId::Updates,
        Keyboard => wit::IconId::Keyboard,
        Cloud => wit::IconId::Cloud,
        Sun => wit::IconId::Sun,
        Moon => wit::IconId::Moon,
        Alert => wit::IconId::Alert,
        Dot => wit::IconId::Dot,
    }
}
fn graph_kind(k: sdk::GraphKind) -> wit::GraphKind {
    use sdk::GraphKind::*;
    match k {
        Cpu => wit::GraphKind::Cpu,
        Memory => wit::GraphKind::Memory,
        Temperature => wit::GraphKind::Temperature,
        Ping => wit::GraphKind::Ping,
        Generic => wit::GraphKind::Generic,
    }
}

fn from_wit_event(ev: wit::Event) -> sdk::Event {
    match ev {
        wit::Event::Timer => sdk::Event::Timer,
        wit::Event::Pointer(p) => sdk::Event::Pointer {
            id: p.id,
            kind: match p.kind {
                wit::PointerKind::Press => sdk::PointerKind::Press,
                wit::PointerKind::RightPress => sdk::PointerKind::RightPress,
                wit::PointerKind::Scroll => sdk::PointerKind::Scroll,
                wit::PointerKind::Enter => sdk::PointerKind::Enter,
                wit::PointerKind::Leave => sdk::PointerKind::Leave,
            },
            delta: p.delta,
        },
        wit::Event::Feed(s) => sdk::Event::Feed {
            feed: match s.feed {
                wit::FeedKind::Cpu => sdk::Feed::Cpu,
                wit::FeedKind::Memory => sdk::Feed::Memory,
                wit::FeedKind::Temperature => sdk::Feed::Temperature,
                wit::FeedKind::Ping => sdk::Feed::Ping,
                wit::FeedKind::Battery => sdk::Feed::Battery,
                wit::FeedKind::Net => sdk::Feed::Net,
            },
            value: s.value,
        },
        wit::Event::Config(c) => sdk::Event::Config(c),
    }
}
