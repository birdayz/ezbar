//! Author SDK for ezbar **WASM plugins** (RFC 0006, guest side).
//!
//! You write a `Plugin`: an Elm loop (`load`/`update`/`view`/`popup`) that
//! builds its chip from the [`widget`] DSL and our [`Icon`]/[`Graph`]
//! components. The host renders the description with real iced and themes it.
//! This is the bounded vocabulary of RFC 0006 §2a — *not* arbitrary iced: there
//! is deliberately no `canvas`/`Shader` (those are compile-in modules).
//!
//! The per-plugin glue (`wit-bindgen` + the generated `Guest` impl) lowers a
//! [`Render`] to the WIT `tree` arena via [`lower`]; see the `weather` example.

// ── theme & components ──────────────────────────────────────────────────────

/// A colour the host resolves. Plugins describe intent; the host owns the
/// palette, so a plugin looks right under any theme (RFC 0006 §2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Token(Token),
    Rgba(u8, u8, u8, u8),
}

/// Semantic theme tokens (resolved to the user's palette by the host).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Token {
    Fg,
    FgDim,
    Accent,
    Ok,
    Warn,
    Urgent,
    Bg,
}

impl From<Token> for Paint {
    fn from(t: Token) -> Self {
        Paint::Token(t)
    }
}

/// The host-rendered icon set (our embedded SVGs). Additive only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    Cpu,
    Memory,
    Temperature,
    Ping,
    VolumeHigh,
    VolumeMedium,
    VolumeMute,
    Battery,
    BatteryCharging,
    BatteryWarning,
    Bot,
    Github,
    Spotify,
    Kubernetes,
    Clock,
    Calendar,
    Disk,
    Net,
    Ip,
    Updates,
    Keyboard,
    Cloud,
    Sun,
    Moon,
    Alert,
    Dot,
    // weather conditions (WMO-coded; map via the weather plugin's wmo_icon)
    CloudSun,
    CloudMoon,
    CloudFog,
    CloudDrizzle,
    CloudRain,
    CloudRainWind,
    CloudSnow,
    CloudHail,
    CloudLightning,
    Droplets,
    Wind,
    Sunrise,
    Sunset,
    Snowflake,
}

impl Icon {
    /// A `size`×`size` icon tinted `color`. Mirrors the host-side
    /// `ezbar_plugin::icons::Icon::view` so the author API is the same shape.
    pub fn view(self, size: f32, color: impl Into<Paint>) -> Render {
        Render::Icon {
            id: self,
            color: color.into(),
            size,
        }
    }
}

/// Kind of host-drawn sparkline (drives the threshold colouring).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GraphKind {
    Cpu,
    Memory,
    Temperature,
    Ping,
    Generic,
}

/// A host-drawn sparkline over the plugin's own data — the thing a `custom`
/// script cannot produce (RFC 0006 motivation #2).
pub struct Graph {
    pub values: Vec<f64>,
    pub kind: GraphKind,
    pub line: Paint,
}

impl Graph {
    pub fn view(self) -> Render {
        Render::Graph {
            values: self.values,
            kind: self.kind,
            line: self.line,
        }
    }
}

/// A high-fidelity smoothed gradient **area chart** — the same renderer the
/// built-in `stock` popup uses. Sized `width`×`height`; ideal for a popup.
pub struct Chart {
    pub values: Vec<f64>,
    pub line: Paint,
    pub width: f32,
    pub height: f32,
}

impl Chart {
    pub fn view(self) -> Render {
        Render::Chart {
            values: self.values,
            line: self.line,
            width: self.width,
            height: self.height,
        }
    }
}

/// Cross-axis alignment of a row/column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
}

// ── the widget DSL (a recursive tree; lowered to the WIT arena by `lower`) ───

/// A node in the bounded widget vocabulary. Build it with the [`widget`]
/// builders. The host caps depth/count during the lift (RFC 0006 §1a).
#[derive(Clone, Debug, PartialEq)]
pub enum Render {
    Text {
        content: String,
        color: Paint,
        size: Option<f32>,
    },
    Row {
        children: Vec<Render>,
        spacing: f32,
        align: Align,
    },
    Column {
        children: Vec<Render>,
        spacing: f32,
        align: Align,
    },
    Container {
        child: Box<Render>,
        padding: f32,
    },
    MouseArea {
        child: Box<Render>,
        id: String,
    },
    Icon {
        id: Icon,
        color: Paint,
        size: f32,
    },
    Graph {
        values: Vec<f64>,
        kind: GraphKind,
        line: Paint,
    },
    Chart {
        values: Vec<f64>,
        line: Paint,
        width: f32,
        height: f32,
    },
    Spacer(f32),
}

/// The `widget` builders — the iced-shaped author surface.
pub mod widget {
    use super::{Align, Paint, Render, Token};

    pub fn text(content: impl Into<String>) -> Render {
        Render::Text {
            content: content.into(),
            color: Paint::Token(Token::Fg),
            size: None,
        }
    }

    pub fn row(children: impl IntoIterator<Item = Render>) -> Render {
        Render::Row {
            children: children.into_iter().collect(),
            spacing: 0.0,
            align: Align::Center,
        }
    }

    pub fn column(children: impl IntoIterator<Item = Render>) -> Render {
        Render::Column {
            children: children.into_iter().collect(),
            spacing: 0.0,
            align: Align::Start,
        }
    }

    pub fn container(child: Render) -> Render {
        Render::Container {
            child: Box::new(child),
            padding: 0.0,
        }
    }

    /// Tag an interactive region; the host sends `Event::Pointer { id, .. }`.
    pub fn mouse_area(id: impl Into<String>, child: Render) -> Render {
        Render::MouseArea {
            child: Box::new(child),
            id: id.into(),
        }
    }

    pub fn spacer(px: f32) -> Render {
        Render::Spacer(px)
    }
}

impl Render {
    /// Fluent setters. **Each applies only to the node kinds where it makes
    /// sense and is a no-op elsewhere** (e.g. `.padding()` on a `text` does
    /// nothing) — so put the setter on the right builder.
    ///
    /// Sets the colour of a `text`/`icon`, or the line colour of a `graph`/`chart`.
    pub fn color(mut self, c: impl Into<Paint>) -> Self {
        let c = c.into();
        match &mut self {
            Render::Text { color, .. } | Render::Icon { color, .. } => *color = c,
            Render::Graph { line, .. } | Render::Chart { line, .. } => *line = c,
            _ => {}
        }
        self
    }
    pub fn size(mut self, px: f32) -> Self {
        if let Render::Text { size, .. } = &mut self {
            *size = Some(px);
        }
        self
    }
    pub fn spacing(mut self, px: f32) -> Self {
        match &mut self {
            Render::Row { spacing, .. } | Render::Column { spacing, .. } => *spacing = px,
            _ => {}
        }
        self
    }
    pub fn align(mut self, a: Align) -> Self {
        match &mut self {
            Render::Row { align, .. } | Render::Column { align, .. } => *align = a,
            _ => {}
        }
        self
    }
    pub fn padding(mut self, px: f32) -> Self {
        if let Render::Container { padding, .. } = &mut self {
            *padding = px;
        }
        self
    }
}

// ── events & the Plugin trait ───────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Event {
    Timer,
    Pointer {
        id: String,
        kind: PointerKind,
        delta: f32,
    },
    Feed {
        feed: Feed,
        value: f64,
    },
    Config(Vec<(String, String)>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Press,
    RightPress,
    Scroll,
    Enter,
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feed {
    Cpu,
    Memory,
    Temperature,
    Ping,
    Battery,
    Net,
}

/// One workspace, as read via [`Ctx::sway_snapshot`] (RFC 0013).
#[derive(Clone, Debug, PartialEq)]
pub struct SwayWorkspace {
    pub name: String,
    /// the active workspace on the focused output
    pub focused: bool,
    /// visible on *some* output (focused, or active on another monitor)
    pub visible: bool,
    /// flagged urgent by a client
    pub urgent: bool,
}

/// A read-only sway snapshot: the workspace list + the focused window title (RFC 0013).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SwayState {
    pub workspaces: Vec<SwayWorkspace>,
    pub title: String,
}

/// Host services available inside `update` — capability-gated (RFC 0006 §3,§5).
/// The host performs the call; an ungranted one returns an error.
pub trait Ctx {
    /// HTTP GET `url` (gated by a `network { host }` capability). Returns the body.
    /// The host runs it async; from your plugin it's a normal blocking call. Keep it on
    /// the **poll/timer** path, NOT in a pointer handler: while a fetch is in flight your
    /// plugin processes no further input (one drive task per plugin), so a click handler
    /// that fetches makes the widget unresponsive for the duration. Pointer handlers must
    /// be cheap; kick I/O to the next `Event::Timer`.
    fn http_get(&mut self, url: &str) -> Result<Vec<u8>, String>;
    /// Append a line to the bar's log.
    fn log(&mut self, msg: &str);
    /// Ask the host to deliver the next [`Event::Timer`] after `ms` milliseconds.
    ///
    /// **One-shot:** this schedules exactly ONE timer. To keep a cadence, call it again
    /// from each `Event::Timer` (e.g. `ctx.set_timeout(1000)` every tick for a 1 Hz clock)
    /// — if you don't re-arm, the timer goes silent after firing once. `set_timeout(0)`
    /// **cancels**: no timer until you arm one again (a purely reactive plugin that only
    /// redraws on pointer/feed events should call this once so it costs zero). Values below
    /// 100 ms are floored to 100 ms.
    ///
    /// A plugin that *never* calls this keeps a legacy ~2 s heartbeat (the zero-config
    /// default), so a trivial poller Just Works without arming anything.
    fn set_timeout(&mut self, ms: u32);
    /// Subscribe to a host-sampled system [`Feed`]; the host then delivers
    /// [`Event::Feed`] `{ feed, value }` no faster than `min_period_ms` (clamped to ≥ 1 s).
    ///
    /// **Capability-gated:** only feeds the user granted in `[modules.<id>].feeds` are
    /// delivered (the names are lowercase-exact: `cpu`, `memory`, `temperature`,
    /// `battery`, `net`). **Fire-and-forget:** this returns nothing and gives **no delivery
    /// guarantee** — an ungranted feed, or a deferred one (`Feed::Ping` has no target in
    /// v1), is silently never delivered (the host logs which kind). Do not busy-wait on a
    /// feed that may never arrive; just render whatever samples you do get. Re-subscribing
    /// is idempotent (it only updates the period), so calling it once on your first
    /// `Event::Timer` is the norm. (Unlike [`http_get`](Ctx::http_get), which returns
    /// `Err` on a denied capability, the frozen `feed-subscribe` ABI has no result and
    /// can't signal denial synchronously.)
    fn feed_subscribe(&mut self, feed: Feed, min_period_ms: u32);
    /// Read the current sway state — the workspace list + focused window title (RFC 0013).
    ///
    /// **Read-only** (there's deliberately no way to drive sway from a plugin) and
    /// **capability-gated** by `[modules.<id>].sway = true`; returns `Err` if unset (a
    /// synchronous denial, unlike fire-and-forget feeds). It's a *pull* get-current — call it
    /// in `update` (e.g. on your `Event::Timer`) and render from the result; sway state is a
    /// snapshot, not a stream.
    fn sway_snapshot(&mut self) -> Result<SwayState, String>;
}

/// What a plugin implements. The host drives the Elm loop; `view`/`popup` are
/// **pure** (no host calls — build the description), while `update` may use the
/// gated host services on `ctx`. `update` returns `true` when the chip changed.
///
/// Pair your `impl Plugin` with [`export_plugin!`] — that's the only glue.
pub trait Plugin {
    fn load(&mut self, _config: Vec<(String, String)>) {}
    fn update(&mut self, _ctx: &mut dyn Ctx, _ev: Event) -> bool {
        false
    }
    fn view(&self) -> Render;
    fn popup(&self) -> Option<Render> {
        None
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn restore(&mut self, _state: Vec<u8>) {}
}

// ── lowering: recursive Render → the flat WIT arena (RFC 0006 §2) ────────────

/// A flat node, 1:1 with the WIT `ui.node` variant. The per-plugin glue maps
/// each `WireNode` to the generated binding type (mechanical), keeping
/// wit-bindgen out of this crate so it stays native-testable.
#[derive(Clone, Debug, PartialEq)]
pub enum WireNode {
    Text {
        content: String,
        color: Paint,
        size: Option<f32>,
    },
    Row {
        children: Vec<u32>,
        spacing: f32,
        align: Align,
    },
    Column {
        children: Vec<u32>,
        spacing: f32,
        align: Align,
    },
    Container {
        child: u32,
        padding: f32,
    },
    MouseArea {
        child: u32,
        id: String,
    },
    Icon {
        id: Icon,
        color: Paint,
        size: f32,
    },
    Graph {
        values: Vec<f64>,
        kind: GraphKind,
        line: Paint,
    },
    Chart {
        values: Vec<f64>,
        line: Paint,
        width: f32,
        height: f32,
    },
    Spacer(f32),
}

/// Flatten a [`Render`] tree into `(nodes, root)` — the WIT `tree`. Post-order,
/// so children precede parents and indices are stable.
pub fn lower(r: &Render) -> (Vec<WireNode>, u32) {
    let mut nodes = Vec::new();
    let root = push(r, &mut nodes);
    (nodes, root)
}

fn push(r: &Render, out: &mut Vec<WireNode>) -> u32 {
    let node = match r {
        Render::Text {
            content,
            color,
            size,
        } => WireNode::Text {
            content: content.clone(),
            color: *color,
            size: *size,
        },
        Render::Row {
            children,
            spacing,
            align,
        } => {
            let kids = children.iter().map(|c| push(c, out)).collect();
            WireNode::Row {
                children: kids,
                spacing: *spacing,
                align: *align,
            }
        }
        Render::Column {
            children,
            spacing,
            align,
        } => {
            let kids = children.iter().map(|c| push(c, out)).collect();
            WireNode::Column {
                children: kids,
                spacing: *spacing,
                align: *align,
            }
        }
        Render::Container { child, padding } => {
            let c = push(child, out);
            WireNode::Container {
                child: c,
                padding: *padding,
            }
        }
        Render::MouseArea { child, id } => {
            let c = push(child, out);
            WireNode::MouseArea {
                child: c,
                id: id.clone(),
            }
        }
        Render::Icon { id, color, size } => WireNode::Icon {
            id: *id,
            color: *color,
            size: *size,
        },
        Render::Graph { values, kind, line } => WireNode::Graph {
            values: values.clone(),
            kind: *kind,
            line: *line,
        },
        Render::Chart {
            values,
            line,
            width,
            height,
        } => WireNode::Chart {
            values: values.clone(),
            line: *line,
            width: *width,
            height: *height,
        },
        Render::Spacer(px) => WireNode::Spacer(*px),
    };
    out.push(node);
    (out.len() - 1) as u32
}

// ── component export (wasm32 only) ───────────────────────────────────────────

/// The generated `Guest` + all the wit-bindgen glue live here, compiled only for
/// the wasm target. Authors never touch it.
#[cfg(target_arch = "wasm32")]
mod glue;

/// Construct a plugin as a trait object. Used by [`export_plugin!`]; hidden.
#[doc(hidden)]
pub fn __new<P: Plugin + Default + 'static>() -> Box<dyn Plugin> {
    Box::new(P::default())
}

/// Export a [`Plugin`] type as an ezbar WASM component — the **only** glue an
/// author writes (with `crate-type = ["cdylib"]` and a `Default` impl):
///
/// ```ignore
/// #[derive(Default)]
/// struct MyWidget { /* … */ }
/// impl ezbar_plugin_wasm::Plugin for MyWidget { /* view/update/popup */ }
/// ezbar_plugin_wasm::export_plugin!(MyWidget);
/// ```
#[macro_export]
macro_rules! export_plugin {
    ($t:ty) => {
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        fn __ezbar_plugin_new() -> ::std::boxed::Box<dyn $crate::Plugin> {
            $crate::__new::<$t>()
        }
    };
}

/// One-import convenience: `use ezbar_plugin_wasm::prelude::*;` brings the
/// `Plugin` trait, the `widget` builders, every component (`Icon`/`Graph`/
/// `Chart`), the event/theme types, and the `export_plugin!` macro — so a plugin
/// needs exactly one `use`.
pub mod prelude {
    pub use crate::widget::*;
    pub use crate::{
        export_plugin, Align, Chart, Ctx, Event, Feed, Graph, GraphKind, Icon, Paint, Plugin,
        PointerKind, Render, SwayState, SwayWorkspace, Token,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use widget::*;

    #[test]
    fn lowers_post_order_root_last() {
        let r = row([Icon::Cloud.view(14.0, Token::Fg), text("21°C")]).spacing(5.0);
        let (nodes, root) = lower(&r);
        // 2 leaves + 1 row, root is the row (pushed last)
        assert_eq!(nodes.len(), 3);
        assert_eq!(root, 2);
        match &nodes[2] {
            WireNode::Row {
                children, spacing, ..
            } => {
                assert_eq!(children, &[0, 1]);
                assert_eq!(*spacing, 5.0);
            }
            other => panic!("root not a row: {other:?}"),
        }
    }

    #[test]
    fn fluent_text_setters() {
        let t = text("hi").color(Token::Accent).size(16.0);
        assert_eq!(
            t,
            Render::Text {
                content: "hi".into(),
                color: Paint::Token(Token::Accent),
                size: Some(16.0)
            }
        );
    }
}
