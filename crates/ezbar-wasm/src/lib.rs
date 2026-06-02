//! Host runtime for ezbar WASM plugins (RFC 0006 phase 2).
//!
//! [`WasmModule`] turns a `.wasm` component into a regular bar [`Module`]: an
//! off-GUI **actor thread** owns the wasmtime `Store`, drives the plugin's
//! `update`/`view` loop, lifts the returned widget tree (capped + validated) into
//! a `Send` arena, and parks it in a shared slot. The bar's `view` only ever
//! reads that cached slot and renders it as **real iced widgets** — it never
//! calls into the store. A trap (epoch/OOM) disables the plugin; the bar is
//! untouched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::add_to_linker_sync;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use ezbar_plugin::iced::widget::{canvas, column, container, mouse_area, row, text};
use ezbar_plugin::iced::{alignment, Color, Element, Length, Subscription};
use ezbar_plugin::ui::graph::{Graph, GraphKind, StockChart};
use ezbar_plugin::{icons, Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit/since-v0.1.0",
});

// `Tree` is re-exported at the bindgen root by the world's `use`.
use ezbar::plugin::ui::Node;

// resource bounds (RFC 0006 §1a, v2.1: fixed constants)
const EPOCH_TICK: Duration = Duration::from_millis(10);
const DEADLINE_TICKS: u64 = 20; // ~200ms per guest call
const MEM_LIMIT: usize = 8 << 20; // 8 MiB per plugin store
const MAX_NODES: usize = 2_000;
const MAX_DEPTH: usize = 32;
const POLL: Duration = Duration::from_secs(2); // v1 timer cadence

// ── host store data + the gated import interface ─────────────────────────────

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    limits: StoreLimits,
    granted_network: Vec<String>,
    // Set while the guest is parked in a blocking host call (e.g. http_get) so the
    // epoch ticker pauses — the deadline bounds GUEST cpu, not time spent waiting
    // on the network, which has its own timeout.
    epoch_paused: Arc<AtomicBool>,
}

/// Resets the epoch-pause flag on drop, so every `http_get` return path (incl.
/// the `?` early-exits) re-arms the ticker.
struct PauseGuard(Arc<AtomicBool>);
impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl ezbar::plugin::host::Host for Host {
    fn log(&mut self, msg: String) {
        log::info!("[wasm plugin] {msg}");
    }
    fn text_size(&mut self) -> f32 {
        14.0
    }
    fn fg(&mut self) -> ezbar::plugin::types::Paint {
        ezbar::plugin::types::Paint::Token(ezbar::plugin::types::ThemeToken::Fg)
    }
    fn set_timeout(&mut self, _ms: u32) {}
    fn subscribe(&mut self, _kinds: Vec<ezbar::plugin::types::EventKind>) {}
    fn http_get(&mut self, url: String) -> Result<Vec<u8>, String> {
        let h = url.split("://").nth(1).unwrap_or(&url);
        let h = h.split('/').next().unwrap_or(h);
        if !self.granted_network.iter().any(|g| g == h) {
            return Err(format!("capability denied: network host '{h}' not granted"));
        }
        // We're on the plugin's off-GUI actor thread, so a blocking fetch is fine.
        // Pause the epoch ticker for the duration: a slow network response must not
        // burn the guest's per-call deadline and trap it on resume. The reqwest
        // timeout bounds the wait independently.
        self.epoch_paused.store(true, Ordering::Relaxed);
        let _resume = PauseGuard(self.epoch_paused.clone());
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .user_agent("ezbar-wasm")
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(&url).send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("http {}", resp.status()));
        }
        resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
    }
    fn read_file(&mut self, _path: String) -> Result<Vec<u8>, String> {
        Err("capability denied: read-file not granted".into())
    }
    fn feed_subscribe(&mut self, _feed: ezbar::plugin::types::FeedKind, _min: u32) {}
}

impl ezbar::plugin::types::Host for Host {}
impl ezbar::plugin::ui::Host for Host {}
impl ezbar::plugin::events::Host for Host {}

// ── the lifted (Send) widget arena, decoupled from the wasmtime types ────────

#[derive(Clone, Debug)]
enum Paint {
    Token(u8), // index into the theme tokens, see `paint_color`
    Rgba(u8, u8, u8, u8),
}

#[derive(Clone, Debug)]
enum LNode {
    Text {
        content: String,
        color: Paint,
        size: Option<f32>,
    },
    Row {
        children: Vec<u32>,
        spacing: f32,
        align: u8,
    },
    Column {
        children: Vec<u32>,
        spacing: f32,
        align: u8,
    },
    Container {
        child: u32,
        padding: f32,
    },
    MouseArea {
        child: u32,
    },
    Icon {
        id: icons::Icon,
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
        width: f32,
        height: f32,
    },
    Spacer(f32),
}

#[derive(Clone, Debug, Default)]
struct Lifted {
    nodes: Vec<LNode>,
    root: u32,
}

/// Lift the wasmtime `Tree` into a `Send` arena, enforcing the node cap and
/// validating the arena is forward-referencing (a DAG) so the bar's render
/// recursion is bounded (RFC 0006 §1a / v2.1).
fn lift(t: &Tree) -> Result<Lifted, String> {
    if t.nodes.len() > MAX_NODES {
        return Err(format!(
            "node cap exceeded: {} > {MAX_NODES}",
            t.nodes.len()
        ));
    }
    let mut nodes = Vec::with_capacity(t.nodes.len());
    for (i, n) in t.nodes.iter().enumerate() {
        nodes.push(lift_node(n, i as u32)?);
    }
    if t.root as usize >= nodes.len() {
        return Err("root out of range".into());
    }
    Ok(Lifted {
        nodes,
        root: t.root,
    })
}

fn check_fwd(parent: u32, c: u32) -> Result<u32, String> {
    // `lower()` emits post-order, so a child always precedes its parent.
    if c >= parent {
        Err(format!("malformed arena: non-forward ref {c} >= {parent}"))
    } else {
        Ok(c)
    }
}

fn lift_node(n: &Node, idx: u32) -> Result<LNode, String> {
    use ezbar::plugin::ui::Node as N;
    Ok(match n {
        N::Text(t) => LNode::Text {
            content: t.content.clone(),
            color: paint(&t.color),
            size: t.size,
        },
        N::Row(l) => LNode::Row {
            children: l
                .children
                .iter()
                .map(|&c| check_fwd(idx, c))
                .collect::<Result<Vec<u32>, String>>()?,
            spacing: l.spacing,
            align: align_u8(l.align),
        },
        N::Column(l) => LNode::Column {
            children: l
                .children
                .iter()
                .map(|&c| check_fwd(idx, c))
                .collect::<Result<Vec<u32>, String>>()?,
            spacing: l.spacing,
            align: align_u8(l.align),
        },
        N::Container(b) => LNode::Container {
            child: check_fwd(idx, b.child)?,
            padding: b.padding,
        },
        N::MouseArea(m) => LNode::MouseArea {
            child: check_fwd(idx, m.child)?,
        },
        N::Icon(i) => LNode::Icon {
            id: icon(i.id),
            color: paint(&i.color),
            size: i.size,
        },
        N::Graph(g) => LNode::Graph {
            values: g.values.clone(),
            kind: graph_kind(g.kind),
            line: paint(&g.line),
        },
        N::Chart(c) => LNode::Chart {
            values: c.values.clone(),
            width: c.width,
            height: c.height,
        },
        N::Spacer(px) => LNode::Spacer(*px),
    })
}

fn align_u8(a: ezbar::plugin::types::Align) -> u8 {
    use ezbar::plugin::types::Align::*;
    match a {
        Start => 0,
        Center => 1,
        End => 2,
    }
}

fn paint(p: &ezbar::plugin::types::Paint) -> Paint {
    use ezbar::plugin::types::{Paint as P, ThemeToken as T};
    match p {
        P::Token(t) => Paint::Token(match t {
            T::Fg => 0,
            T::FgDim => 1,
            T::Accent => 2,
            T::Ok => 3,
            T::Warn => 4,
            T::Urgent => 5,
            T::Bg => 6,
        }),
        P::Rgba(c) => Paint::Rgba(c.r, c.g, c.b, c.a),
    }
}

fn icon(i: ezbar::plugin::types::IconId) -> icons::Icon {
    use ezbar::plugin::types::IconId as W;
    use icons::Icon as I;
    match i {
        W::Cpu => I::Cpu,
        W::Memory => I::Memory,
        W::Temperature => I::Temperature,
        W::Ping => I::Ping,
        W::VolumeHigh => I::VolumeHigh,
        W::VolumeMedium => I::VolumeMedium,
        W::VolumeMute => I::VolumeMute,
        W::Battery => I::Battery,
        W::BatteryCharging => I::BatteryCharging,
        W::BatteryWarning => I::BatteryWarning,
        W::Bot => I::Bot,
        W::Github => I::Github,
        W::Spotify => I::Spotify,
        W::Kubernetes => I::Kubernetes,
        W::Clock => I::Clock,
        W::Calendar => I::Calendar,
        W::Disk => I::Disk,
        W::Net => I::Net,
        W::Ip => I::Ip,
        W::Updates => I::Updates,
        W::Keyboard => I::Keyboard,
        W::Cloud => I::Cloud,
        W::Sun => I::Sun,
        W::Moon => I::Moon,
        W::Alert => I::Alert,
        W::Dot => I::Dot,
        W::CloudSun => I::CloudSun,
        W::CloudMoon => I::CloudMoon,
        W::CloudFog => I::CloudFog,
        W::CloudDrizzle => I::CloudDrizzle,
        W::CloudRain => I::CloudRain,
        W::CloudRainWind => I::CloudRainWind,
        W::CloudSnow => I::CloudSnow,
        W::CloudHail => I::CloudHail,
        W::CloudLightning => I::CloudLightning,
        W::Droplets => I::Droplets,
        W::Wind => I::Wind,
        W::Sunrise => I::Sunrise,
        W::Sunset => I::Sunset,
        W::Snowflake => I::Snowflake,
    }
}

fn graph_kind(k: ezbar::plugin::types::GraphKind) -> GraphKind {
    use ezbar::plugin::types::GraphKind as W;
    match k {
        W::Cpu => GraphKind::Cpu,
        W::Memory => GraphKind::Memory,
        W::Temperature => GraphKind::Temperature,
        W::Ping => GraphKind::Ping,
        W::Generic => GraphKind::Cpu, // line colour is overridden anyway
    }
}

// ── the off-GUI actor: owns the Store, drives the plugin, fills the slot ─────

#[derive(Default)]
struct Slots {
    view: Option<Lifted>,
    popup: Option<Lifted>,
}
type Slot = Arc<Mutex<Slots>>;

fn spawn_actor(path: PathBuf, config: Vec<(String, String)>, slot: Slot, grants: Vec<String>) {
    std::thread::spawn(move || {
        if let Err(e) = run_actor(&path, config, &slot, grants) {
            log::warn!("ezbar-wasm: plugin {path:?} stopped: {e:#}");
        }
    });
}

fn run_actor(
    path: &Path,
    config: Vec<(String, String)>,
    slot: &Slot,
    grants: Vec<String>,
) -> Result<()> {
    let mut cfg = Config::new();
    cfg.epoch_interruption(true);
    let engine = Engine::new(&cfg)?;
    let component = Component::from_file(&engine, path)?;
    let mut linker: Linker<Host> = Linker::new(&engine);
    add_to_linker_sync(&mut linker)?;
    Plugin::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h: &mut Host| h)?;

    // epoch ticker for this plugin's engine — paused while the guest is parked in
    // a blocking host call so network waits don't burn the per-call deadline.
    let epoch_paused = Arc::new(AtomicBool::new(false));
    {
        let eng = engine.clone();
        let paused = epoch_paused.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(EPOCH_TICK);
            if !paused.load(Ordering::Relaxed) {
                eng.increment_epoch();
            }
        });
    }

    let wasi = WasiCtxBuilder::new().build();
    let mut store = Store::new(
        &engine,
        Host {
            table: ResourceTable::new(),
            wasi,
            limits: StoreLimitsBuilder::new().memory_size(MEM_LIMIT).build(),
            granted_network: grants,
            epoch_paused,
        },
    );
    store.limiter(|h| &mut h.limits);

    store.set_epoch_deadline(DEADLINE_TICKS);
    let plugin = Plugin::instantiate(&mut store, &component, &linker)?;
    plugin.call_init(&mut store, &config)?;

    loop {
        store.set_epoch_deadline(DEADLINE_TICKS); // re-arm per call
        let dirty = match plugin.call_update(&mut store, &ezbar::plugin::events::Event::Timer) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("ezbar-wasm: update trapped — disabling plugin: {e}");
                return Ok(()); // terminal for instance
            }
        };
        if dirty {
            store.set_epoch_deadline(DEADLINE_TICKS);
            let view = match plugin.call_view(&mut store) {
                Ok(tree) => match lift(&tree) {
                    Ok(l) => Some(l),
                    Err(e) => {
                        log::warn!("ezbar-wasm: view rejected: {e}");
                        None
                    }
                },
                Err(e) => {
                    log::warn!("ezbar-wasm: view trapped — disabling plugin: {e}");
                    return Ok(());
                }
            };
            store.set_epoch_deadline(DEADLINE_TICKS);
            let popup = match plugin.call_popup(&mut store) {
                Ok(Some(tree)) => lift(&tree).ok(),
                Ok(None) => None,
                Err(e) => {
                    log::warn!("ezbar-wasm: popup trapped — disabling plugin: {e}");
                    return Ok(());
                }
            };
            let mut s = slot.lock().unwrap();
            if view.is_some() {
                s.view = view;
            }
            s.popup = popup;
        }
        std::thread::sleep(POLL);
    }
}

// ── the bar Module ───────────────────────────────────────────────────────────

enum Msg {
    Tick,
    Hover,
    Leave,
}

/// A loaded WASM plugin, presented to the bar as a [`Module`].
pub struct WasmModule {
    id: String,
    instance: u64,
    slot: Slot,
}

impl WasmModule {
    /// Load `path` as a plugin with the placement `id`. `config` is the
    /// `[modules.<id>]` table flattened to string pairs; `grants` are the
    /// granted network hosts (capabilities).
    pub fn new(
        instance: u64,
        id: impl Into<String>,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
    ) -> Self {
        let slot: Slot = Arc::new(Mutex::new(Slots::default()));
        spawn_actor(path, config, slot.clone(), grants);
        WasmModule {
            id: id.into(),
            instance,
            slot,
        }
    }
}

impl WasmModule {
    /// Headless snapshot of the latest lifted view/popup node counts. Returns
    /// `(view_nodes, popup_nodes)` — both 0 until the actor has produced a frame
    /// (or if the plugin trapped). Used by the `preview --check` smoke test.
    pub fn debug_snapshot(&self) -> (usize, usize) {
        let s = self.slot.lock().unwrap();
        (
            s.view.as_ref().map_or(0, |l| l.nodes.len()),
            s.popup.as_ref().map_or(0, |l| l.nodes.len()),
        )
    }
}

impl Module for WasmModule {
    fn id(&self) -> &str {
        &self.id
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, tick_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            // hovering the chip opens the plugin's popup; leaving closes it
            // (the host doesn't auto-close — the module drives both, like calendar).
            Some(Msg::Hover) => Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::Leave) => Response::request(HostRequest::ClosePopup),
            _ => Response::none(), // Tick: just re-render; `view` reads the cache
        }
    }

    fn popup_size(&self) -> Option<(u32, u32)> {
        // Content-size the popup — a chart, a text/list, or a mix — so it isn't
        // lost in the default 480×400 surface. We have no real text metrics off
        // the GUI thread, so `measure` is a rough layout estimate; pad for the
        // surface chrome and clamp to sane bounds.
        let s = self.slot.lock().unwrap();
        let l = s.popup.as_ref()?;
        if l.nodes.is_empty() {
            return None;
        }
        let (w, h) = measure(l, l.root);
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let w = (w + 32.0).clamp(96.0, 720.0);
        let h = (h + 28.0).clamp(40.0, 560.0);
        Some((w as u32, h as u32))
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let s = self.slot.lock().unwrap();
        let has_popup = s.popup.as_ref().is_some_and(|l| !l.nodes.is_empty());
        let chip: Element<'_, ModMsg> = match &s.view {
            Some(l) if !l.nodes.is_empty() => build(l, l.root, ctx, 0),
            _ => text("\u{2026}").color(ctx.fg_dim()).into(),
        };
        drop(s);
        let area = mouse_area(chip);
        if has_popup {
            area.on_enter(ModMsg::new(Msg::Hover))
                .on_exit(ModMsg::new(Msg::Leave))
                .into()
        } else {
            area.into()
        }
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let s = self.slot.lock().unwrap();
        match &s.popup {
            Some(l) if !l.nodes.is_empty() => Some(build(l, l.root, ctx, 0)),
            _ => None,
        }
    }
}

fn tick_stream(_id: &u64) -> impl ezbar_plugin::iced::futures::Stream<Item = ModMsg> {
    use ezbar_plugin::iced::futures::SinkExt;
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                ezbar_plugin::task::sleep(Duration::from_millis(150)).await;
                if out.send(ModMsg::new(Msg::Tick)).await.is_err() {
                    break;
                }
            }
        },
    )
}

// ── render the lifted tree as real iced widgets ──────────────────────────────

fn paint_color(p: &Paint, ctx: &Ctx) -> Color {
    match p {
        Paint::Token(t) => match t {
            0 => ctx.fg(),
            1 => ctx.fg_dim(),
            2 => ctx.accent(),
            3 => ctx.ok(),
            4 => ctx.warn(),
            5 => ctx.urgent(),
            _ => ctx.bg(),
        },
        Paint::Rgba(r, g, b, a) => Color::from_rgba8(*r, *g, *b, *a as f32 / 255.0),
    }
}

/// Rough content-size of a lifted subtree, used to size a popup surface. Off the
/// GUI thread we have no real text metrics, so estimate: ~0.55em advance per
/// char, 1.4em line height. Good enough to keep a popup snug, not pixel-exact.
fn measure(l: &Lifted, idx: u32) -> (f32, f32) {
    match &l.nodes[idx as usize] {
        LNode::Text { content, size, .. } => {
            let px = size.unwrap_or(14.0);
            let cols = content.chars().count().max(1) as f32;
            (cols * px * 0.55, px * 1.4)
        }
        LNode::Row {
            children, spacing, ..
        } => children
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(w, h), (i, &c)| {
                let (cw, ch) = measure(l, c);
                (w + cw + if i > 0 { *spacing } else { 0.0 }, h.max(ch))
            }),
        LNode::Column {
            children, spacing, ..
        } => children
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(w, h), (i, &c)| {
                let (cw, ch) = measure(l, c);
                (w.max(cw), h + ch + if i > 0 { *spacing } else { 0.0 })
            }),
        LNode::Container { child, padding } => {
            let (cw, ch) = measure(l, *child);
            (cw + padding * 2.0, ch + padding * 2.0)
        }
        LNode::MouseArea { child } => measure(l, *child),
        LNode::Icon { size, .. } => (*size, *size),
        LNode::Graph { .. } => (48.0, 16.0), // matches the chip sparkline size below
        LNode::Chart { width, height, .. } => (*width, *height),
        LNode::Spacer(px) => (*px, 0.0),
    }
}

fn build<'a>(l: &Lifted, idx: u32, ctx: &Ctx, depth: usize) -> Element<'a, ModMsg> {
    if depth > MAX_DEPTH {
        return text("\u{2026}").into();
    }
    match &l.nodes[idx as usize] {
        LNode::Text {
            content,
            color,
            size,
        } => {
            let mut t = text(content.clone()).color(paint_color(color, ctx));
            if let Some(s) = size {
                t = t.size(*s);
            }
            t.into()
        }
        LNode::Row {
            children,
            spacing,
            align,
        } => {
            let kids: Vec<_> = children
                .iter()
                .map(|&c| build(l, c, ctx, depth + 1))
                .collect();
            row(kids)
                .spacing(*spacing)
                .align_y(match align {
                    0 => alignment::Vertical::Top,
                    2 => alignment::Vertical::Bottom,
                    _ => alignment::Vertical::Center,
                })
                .into()
        }
        LNode::Column {
            children,
            spacing,
            align,
        } => {
            let kids: Vec<_> = children
                .iter()
                .map(|&c| build(l, c, ctx, depth + 1))
                .collect();
            column(kids)
                .spacing(*spacing)
                .align_x(match align {
                    1 => alignment::Horizontal::Center,
                    2 => alignment::Horizontal::Right,
                    _ => alignment::Horizontal::Left,
                })
                .into()
        }
        LNode::Container { child, padding } => container(build(l, *child, ctx, depth + 1))
            .padding(*padding)
            .into(),
        // interactivity (pointer events) is phase-2b; render the child for now
        LNode::MouseArea { child } => mouse_area(build(l, *child, ctx, depth + 1)).into(),
        LNode::Icon { id, color, size } => id.view(*size, paint_color(color, ctx)),
        LNode::Graph { values, kind, line } => canvas(Graph {
            values: values.clone(),
            kind: *kind,
            line_color: Some(paint_color(line, ctx)),
        })
        .width(Length::Fixed(48.0))
        .height(Length::Fixed(16.0))
        .into(),
        // the high-fidelity stock-popup renderer (smoothed gradient area chart)
        LNode::Chart {
            values,
            width,
            height,
        } => canvas(StockChart {
            values: values.clone(),
            symbol: String::new(),
        })
        .width(Length::Fixed(*width))
        .height(Length::Fixed(*height))
        .into(),
        LNode::Spacer(px) => container(text("")).width(Length::Fixed(*px)).into(),
    }
}

// ── discovery ────────────────────────────────────────────────────────────────

/// Scan a plugins directory for `*.wasm`, returning `(id, path)` pairs. The id is
/// the file stem (a manifest is read in phase-2b).
pub fn discover(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("wasm") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push((stem.to_string(), p));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
