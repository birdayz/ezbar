//! Example ezbar WASM plugin (RFC 0006 PoC): a weather chip.
//!
//! Author code = the `Plugin` impl + the `widget` DSL. The glue below bridges
//! the generated component `Guest` to that `Plugin` and lowers the DSL tree to
//! the WIT arena. (In the shipping SDK this glue becomes an `export_plugin!`
//! macro; spelled out here so the PoC is legible.)

use ezbar_plugin_wasm as sdk;
use sdk::{lower, widget::*, Icon, Plugin, Render, Token, WireNode};
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
        BoxNode, GraphKind, GraphNode, HitNode, IconNode, LayoutNode, Node, TextNode, Tree,
    };
}

// ── the actual plugin (pure author code) ────────────────────────────────────

#[derive(Default)]
struct Weather {
    temp_c: Option<f32>,
    // PoC demo hooks for the host's safety tests (`demo = spin|huge` in config):
    spin: bool,  // view() spins forever  -> host epoch-interruption trap
    huge: bool,  // view() returns a giant tree -> host node-cap rejection
    fetch: bool, // update() calls the network host import -> capability gate
}

impl Plugin for Weather {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            if k == "demo" {
                self.spin = v == "spin";
                self.huge = v == "huge";
                self.fetch = v == "fetch";
            }
        }
    }

    fn view(&self) -> Render {
        if self.spin {
            // a runaway plugin: the host's epoch deadline must trap this.
            loop {
                std::hint::spin_loop();
            }
        }
        if self.huge {
            // a large tree (fits memory, but over the host's node cap): the host
            // must reject it during the lift — StoreLimits alone wouldn't.
            return column((0..5_000).map(|i| text(format!("{i}"))));
        }
        let label = self
            .temp_c
            .map(|t| format!("{t:.0}\u{b0}C"))
            .unwrap_or_else(|| "\u{2014}".into());
        row([Icon::Cloud.view(14.0, Token::Fg), text(label).color(Token::Fg)]).spacing(5.0)
    }

    fn update(&mut self, ev: sdk::Event) -> bool {
        match ev {
            // a real plugin would `ctx.http(...)` here; the PoC just ticks.
            sdk::Event::Timer => {
                self.temp_c = Some(21.0);
                true
            }
            _ => false,
        }
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
        STATE.with_borrow_mut(|w| {
            let dirty = w.update(from_wit_event(ev));
            if w.fetch {
                // call the gated network host import; the host enforces the
                // `network { host }` capability (RFC 0006 §5).
                match crate::ezbar::plugin::host::http_get("https://api.weather.example/now") {
                    Ok(_) => {}
                    Err(e) => crate::ezbar::plugin::host::log(&format!("fetch denied: {e}")),
                }
            }
            dirty
        })
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
