//! wasm32-only glue (RFC 0006): runs `wit_bindgen::generate!`, implements the
//! generated component `Guest` once, and bridges it to the author's [`Plugin`]
//! via a linked constructor (`__ezbar_plugin_new`, supplied by `export_plugin!`).
//! A plugin author never sees any of this — they write their `Plugin` impl and
//! `export_plugin!(MyThing)`. This is the Zed `register_extension!` architecture.

use crate::{Align, GraphKind, Icon, Paint, Plugin, Render, Token, WireNode};
use core::cell::RefCell;

wit_bindgen::generate!({
    world: "plugin",
    path: "../../wit/since-v0.1.0",
});

use ezbar::plugin as p;

// `export_plugin!` defines this symbol in the author's cdylib; we call it to
// construct their plugin without knowing its type (Zed's runtime-global trick,
// resolved by the linker instead of a separately-called init export).
extern "Rust" {
    fn __ezbar_plugin_new() -> Box<dyn Plugin>;
}

thread_local! {
    static STATE: RefCell<Option<Box<dyn Plugin>>> = const { RefCell::new(None) };
}

/// Host services bridged to the generated imports (capability-gated by the host).
struct HostCtx;
impl crate::Ctx for HostCtx {
    fn http_get(&mut self, url: &str) -> Result<Vec<u8>, String> {
        p::host::http_get(url)
    }
    fn log(&mut self, msg: &str) {
        p::host::log(msg);
    }
    fn set_timeout(&mut self, ms: u32) {
        p::host::set_timeout(ms);
    }
    fn feed_subscribe(&mut self, feed: crate::Feed, min_period_ms: u32) {
        p::host::feed_subscribe(feed_kind(feed), min_period_ms);
    }
}

fn feed_kind(f: crate::Feed) -> p::types::FeedKind {
    use crate::Feed as F;
    use p::types::FeedKind as W;
    match f {
        F::Cpu => W::Cpu,
        F::Memory => W::Memory,
        F::Temperature => W::Temperature,
        F::Ping => W::Ping,
        F::Battery => W::Battery,
        F::Net => W::Net,
    }
}

struct Component;
export!(Component);

impl Guest for Component {
    fn init(config: Vec<(String, String)>) {
        let mut plugin = unsafe { __ezbar_plugin_new() };
        plugin.load(config);
        STATE.with_borrow_mut(|s| *s = Some(plugin));
    }
    fn update(ev: p::events::Event) -> bool {
        STATE.with_borrow_mut(|s| match s {
            Some(pl) => pl.update(&mut HostCtx, from_event(ev)),
            None => false,
        })
    }
    fn view() -> Tree {
        STATE.with_borrow(|s| s.as_ref().map(|pl| to_tree(&pl.view())).unwrap_or_default())
    }
    fn popup() -> Option<Tree> {
        STATE.with_borrow(|s| s.as_ref().and_then(|pl| pl.popup()).map(|r| to_tree(&r)))
    }
    fn save_state() -> Vec<u8> {
        STATE.with_borrow(|s| s.as_ref().map(|pl| pl.save_state()).unwrap_or_default())
    }
    fn restore(state: Vec<u8>) {
        STATE.with_borrow_mut(|s| {
            if let Some(pl) = s {
                pl.restore(state)
            }
        });
    }
}

impl Default for Tree {
    fn default() -> Self {
        Tree {
            nodes: Vec::new(),
            root: 0,
        }
    }
}

// ── SDK Render -> generated WIT tree (mechanical, identical for every plugin) ─

fn to_tree(r: &Render) -> Tree {
    let (nodes, root) = crate::lower(r);
    Tree {
        nodes: nodes.iter().map(to_node).collect(),
        root,
    }
}

fn to_node(n: &WireNode) -> p::ui::Node {
    use p::ui::Node as N;
    match n {
        WireNode::Text {
            content,
            color,
            size,
        } => N::Text(p::ui::TextNode {
            content: content.clone(),
            color: paint(*color),
            size: *size,
        }),
        WireNode::Row {
            children,
            spacing,
            align,
        } => N::Row(p::ui::LayoutNode {
            children: children.clone(),
            spacing: *spacing,
            align: align_(*align),
        }),
        WireNode::Column {
            children,
            spacing,
            align,
        } => N::Column(p::ui::LayoutNode {
            children: children.clone(),
            spacing: *spacing,
            align: align_(*align),
        }),
        WireNode::Container { child, padding } => N::Container(p::ui::BoxNode {
            child: *child,
            padding: *padding,
        }),
        WireNode::MouseArea { child, id } => N::MouseArea(p::ui::HitNode {
            child: *child,
            id: id.clone(),
        }),
        WireNode::Icon { id, color, size } => N::Icon(p::ui::IconNode {
            id: icon(*id),
            color: paint(*color),
            size: *size,
        }),
        WireNode::Graph { values, kind, line } => N::Graph(p::ui::GraphNode {
            values: values.clone(),
            kind: graph_kind(*kind),
            line: paint(*line),
        }),
        WireNode::Chart {
            values,
            line,
            width,
            height,
        } => N::Chart(p::ui::ChartNode {
            values: values.clone(),
            line: paint(*line),
            width: *width,
            height: *height,
        }),
        WireNode::Spacer(px) => N::Spacer(*px),
    }
}

fn paint(p_: Paint) -> p::types::Paint {
    use p::types::{Paint as WP, ThemeToken as WT};
    match p_ {
        Paint::Token(t) => WP::Token(match t {
            Token::Fg => WT::Fg,
            Token::FgDim => WT::FgDim,
            Token::Accent => WT::Accent,
            Token::Ok => WT::Ok,
            Token::Warn => WT::Warn,
            Token::Urgent => WT::Urgent,
            Token::Bg => WT::Bg,
        }),
        Paint::Rgba(r, g, b, a) => WP::Rgba(p::types::Rgba8 { r, g, b, a }),
    }
}

fn align_(a: Align) -> p::types::Align {
    match a {
        Align::Start => p::types::Align::Start,
        Align::Center => p::types::Align::Center,
        Align::End => p::types::Align::End,
    }
}

fn icon(i: Icon) -> p::types::IconId {
    use p::types::IconId as W;
    use Icon::*;
    match i {
        Cpu => W::Cpu,
        Memory => W::Memory,
        Temperature => W::Temperature,
        Ping => W::Ping,
        VolumeHigh => W::VolumeHigh,
        VolumeMedium => W::VolumeMedium,
        VolumeMute => W::VolumeMute,
        Battery => W::Battery,
        BatteryCharging => W::BatteryCharging,
        BatteryWarning => W::BatteryWarning,
        Bot => W::Bot,
        Github => W::Github,
        Spotify => W::Spotify,
        Kubernetes => W::Kubernetes,
        Clock => W::Clock,
        Calendar => W::Calendar,
        Disk => W::Disk,
        Net => W::Net,
        Ip => W::Ip,
        Updates => W::Updates,
        Keyboard => W::Keyboard,
        Cloud => W::Cloud,
        Sun => W::Sun,
        Moon => W::Moon,
        Alert => W::Alert,
        Dot => W::Dot,
        CloudSun => W::CloudSun,
        CloudMoon => W::CloudMoon,
        CloudFog => W::CloudFog,
        CloudDrizzle => W::CloudDrizzle,
        CloudRain => W::CloudRain,
        CloudRainWind => W::CloudRainWind,
        CloudSnow => W::CloudSnow,
        CloudHail => W::CloudHail,
        CloudLightning => W::CloudLightning,
        Droplets => W::Droplets,
        Wind => W::Wind,
        Sunrise => W::Sunrise,
        Sunset => W::Sunset,
        Snowflake => W::Snowflake,
    }
}

fn graph_kind(k: GraphKind) -> p::types::GraphKind {
    use p::types::GraphKind as W;
    match k {
        GraphKind::Cpu => W::Cpu,
        GraphKind::Memory => W::Memory,
        GraphKind::Temperature => W::Temperature,
        GraphKind::Ping => W::Ping,
        GraphKind::Generic => W::Generic,
    }
}

fn from_event(ev: p::events::Event) -> crate::Event {
    use crate::{Event as E, Feed, PointerKind as PK};
    match ev {
        p::events::Event::Timer => E::Timer,
        p::events::Event::Pointer(pe) => E::Pointer {
            id: pe.id,
            kind: match pe.kind {
                p::events::PointerKind::Press => PK::Press,
                p::events::PointerKind::RightPress => PK::RightPress,
                p::events::PointerKind::Scroll => PK::Scroll,
                p::events::PointerKind::Enter => PK::Enter,
                p::events::PointerKind::Leave => PK::Leave,
            },
            delta: pe.delta,
        },
        p::events::Event::Feed(s) => E::Feed {
            feed: match s.feed {
                p::types::FeedKind::Cpu => Feed::Cpu,
                p::types::FeedKind::Memory => Feed::Memory,
                p::types::FeedKind::Temperature => Feed::Temperature,
                p::types::FeedKind::Ping => Feed::Ping,
                p::types::FeedKind::Battery => Feed::Battery,
                p::types::FeedKind::Net => Feed::Net,
            },
            value: s.value,
        },
        p::events::Event::Config(c) => E::Config(c),
    }
}
