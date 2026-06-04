//! Host runtime for ezbar WASM plugins (RFC 0006 phase 2).
//!
//! [`WasmModule`] turns a `.wasm` component into a regular bar [`Module`]. RFC 0008:
//! one shared [`Reactor`] (a single wasmtime `Engine` + one epoch ticker, on the
//! bar's existing async runtime) drives every plugin as a green-thread task —
//! `update`/`view` async, host I/O async (a `http_get` suspends the guest's fiber,
//! not a thread). Each task lifts the returned widget tree (capped + validated) into
//! a `Send` arena and parks it in a shared slot. The bar's `view` only ever reads
//! that cached slot and renders it as **real iced widgets** — it never calls into a
//! store. A trap/OOM/timeout disables that one plugin; the reactor and bar are
//! untouched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::runtime::Handle;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use ezbar_plugin::iced::advanced::subscription::{from_recipe, EventStream, Hasher, Recipe};
use ezbar_plugin::iced::widget::{canvas, column, container, mouse_area, row, text};
use ezbar_plugin::iced::{alignment, Color, Element, Length, Subscription};
use ezbar_plugin::ui::graph::{Graph, GraphKind, StockChart};
use ezbar_plugin::{icons, Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

// RFC 0008: async world — host imports are async (http_get suspends the guest's
// fiber instead of blocking a thread), and the export calls are driven with `.await`.
wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit/since-v0.1.0",
    imports: { default: async },
    exports: { default: async },
});

// `Tree` is re-exported at the bindgen root by the world's `use`.
use ezbar::plugin::ui::Node;

// resource bounds (RFC 0006 §1a / RFC 0008: fixed constants)
const EPOCH_TICK: Duration = Duration::from_millis(10);
const DEADLINE_TICKS: u64 = 20; // ~200ms guest CPU before a cooperative epoch yield
const MEM_LIMIT: usize = 2 << 20; // 2 MiB per plugin store (RFC 0008 §3.4: 8→2)
const MAX_NODES: usize = 2_000;
const MAX_DEPTH: usize = 32;
const POLL: Duration = Duration::from_secs(2); // v1 timer cadence
                                               // Per-call wall-clock backstop — the *primary* CPU bound (epoch-yield only smooths,
                                               // a yielding guest never self-traps). Must exceed the 8s http timeout (RFC 0008 §3.4).
const WALL: Duration = Duration::from_secs(12);

// ── host store data + the gated import interface ─────────────────────────────

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    limits: StoreLimits,
    granted_network: Vec<String>,
    // One async client, shared from the reactor (Arc-cheap to clone). No more
    // epoch-pause hack: a fiber parked in `http_get.await` runs no guest code, so it
    // burns no epoch by construction (RFC 0008 §3.3).
    client: reqwest::Client,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// RFC 0008: host imports are async. INVARIANT (§3.4): every import returns `Err`,
// never panics — these run on the shared reactor worker, where a panic could wedge
// the engine. No `unwrap`/indexing/`expect` on guest-influenced data.
impl ezbar::plugin::host::Host for Host {
    async fn log(&mut self, msg: String) {
        log::info!("[wasm plugin] {msg}");
    }
    async fn text_size(&mut self) -> f32 {
        14.0
    }
    async fn fg(&mut self) -> ezbar::plugin::types::Paint {
        ezbar::plugin::types::Paint::Token(ezbar::plugin::types::ThemeToken::Fg)
    }
    async fn set_timeout(&mut self, _ms: u32) {}
    async fn subscribe(&mut self, _kinds: Vec<ezbar::plugin::types::EventKind>) {}
    async fn http_get(&mut self, url: String) -> Result<Vec<u8>, String> {
        let h = url.split("://").nth(1).unwrap_or(&url);
        let h = h.split('/').next().unwrap_or(h);
        if !self.granted_network.iter().any(|g| g == h) {
            return Err(format!("capability denied: network host '{h}' not granted"));
        }
        // Async fetch: this `await` suspends the guest's fiber, freeing the reactor
        // worker to serve other plugins. The 8s timeout bounds the wait; the guest
        // burns no epoch while parked here.
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("http {}", resp.status()));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| e.to_string())
    }
    async fn read_file(&mut self, _path: String) -> Result<Vec<u8>, String> {
        Err("capability denied: read-file not granted".into())
    }
    async fn feed_subscribe(&mut self, _feed: ezbar::plugin::types::FeedKind, _min: u32) {}
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

// ── the cached render slot: writer = the reactor task, readers = the bar ─────

#[derive(Default)]
struct Slots {
    view: Option<Lifted>,
    popup: Option<Lifted>,
}

/// The plugin's cached render, shared between the off-GUI actor (the writer) and
/// the bar's `view`/subscription (the readers). `version` bumps on every new frame
/// so the chip's render tick can fire *only when content actually changed* instead
/// of on a blind 150ms timer — an idle plugin then costs zero per-frame renders
/// (and zero allocation churn), while a fresh frame still shows within one poll.
struct Shared {
    slots: Mutex<Slots>,
    version: AtomicU64,
}
type Slot = Arc<Shared>;

// ── the reactor: ONE shared engine, ONE epoch ticker, N plugin tasks ─────────
// RFC 0008. The whole-process reactor is built once (lazily) on the first plugin
// load, from the bar's existing tokio runtime `Handle` — no per-plugin engine, no
// per-plugin thread.

struct Reactor {
    engine: Engine,
    linker: Linker<Host>,
    client: reqwest::Client,
    rt: Handle,
}

static REACTOR: OnceLock<Reactor> = OnceLock::new();

/// The process reactor, initialised on first use with the bar's runtime `Handle`
/// (threaded in explicitly per RFC 0008 §3.1 — never `Handle::current()`).
fn reactor(rt: &Handle) -> &'static Reactor {
    REACTOR.get_or_init(|| Reactor::new(rt.clone()))
}

impl Reactor {
    fn new(rt: Handle) -> Self {
        let mut cfg = Config::new();
        cfg.epoch_interruption(true); // async support is always on in wasmtime 45
        let engine = Engine::new(&cfg).expect("ezbar-wasm: build wasmtime engine");
        // ONE epoch ticker for the shared engine (a plain sleep loop, not a runtime).
        {
            let eng = engine.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(EPOCH_TICK);
                eng.increment_epoch();
            });
        }
        let mut linker: Linker<Host> = Linker::new(&engine);
        add_to_linker_async(&mut linker).expect("ezbar-wasm: wasi async linker");
        Plugin::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h: &mut Host| h)
            .expect("ezbar-wasm: plugin linker");
        // ONE async client shared by every plugin (Arc-cheap clone into each Host).
        let client = reqwest::Client::builder()
            .user_agent("ezbar-wasm")
            .build()
            .expect("ezbar-wasm: reqwest client");
        Reactor {
            engine,
            linker,
            client,
            rt,
        }
    }

    /// Spawn a green-thread driver for one plugin on the shared runtime.
    fn add_plugin(
        &'static self,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
        slot: Slot,
    ) -> tokio::task::JoinHandle<()> {
        self.rt.spawn(async move {
            if let Err(e) = self.drive(path.clone(), config, grants, slot).await {
                log::warn!("ezbar-wasm: plugin {path:?} stopped: {e:#}");
            }
        })
    }

    async fn drive(
        &'static self,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
        slot: Slot,
    ) -> Result<()> {
        // Load the component via the on-disk artifact cache (mmap'd, Shared_Clean —
        // RFC 0008 §6 Q3), all on the blocking pool so the heavy CPU compile + I/O
        // never stalls the reactor worker.
        let component = {
            let engine = self.engine.clone();
            tokio::task::spawn_blocking(move || load_component(&engine, &path))
                .await
                .context("compile task join")??
        };
        let mut store = Store::new(
            &self.engine,
            Host {
                table: ResourceTable::new(),
                wasi: WasiCtxBuilder::new().build(),
                limits: StoreLimitsBuilder::new().memory_size(MEM_LIMIT).build(),
                granted_network: grants,
                client: self.client.clone(),
            },
        );
        store.limiter(|h| &mut h.limits);
        // CPU bound: cooperative epoch-yield (smoothing) + the wall-clock `timeout`
        // below (the real backstop). A yielding guest never self-traps.
        store.epoch_deadline_async_yield_and_update(DEADLINE_TICKS);
        // instantiate + init get the same wall-clock backstop as the loop calls — a
        // guest that spins in `init` yields on epoch but never self-traps, so without
        // this the drive loop would never start and nothing could disable it.
        store.set_epoch_deadline(DEADLINE_TICKS);
        let plugin = match tokio::time::timeout(
            WALL,
            Plugin::instantiate_async(&mut store, &component, &self.linker),
        )
        .await
        {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                log::warn!("ezbar-wasm: instantiate failed — disabling plugin: {e}");
                return Ok(());
            }
            Err(_) => {
                log::warn!("ezbar-wasm: instantiate exceeded {WALL:?} — disabling plugin");
                return Ok(());
            }
        };
        match tokio::time::timeout(WALL, plugin.call_init(&mut store, &config)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                log::warn!("ezbar-wasm: init failed — disabling plugin: {e}");
                return Ok(());
            }
            Err(_) => {
                log::warn!("ezbar-wasm: init exceeded {WALL:?} — disabling plugin");
                return Ok(());
            }
        }

        let timer = ezbar::plugin::events::Event::Timer;
        loop {
            store.set_epoch_deadline(DEADLINE_TICKS); // re-arm the yield window
            let dirty =
                match tokio::time::timeout(WALL, plugin.call_update(&mut store, &timer)).await {
                    Ok(Ok(d)) => d,
                    Ok(Err(e)) => {
                        log::warn!("ezbar-wasm: update trapped — disabling plugin: {e}");
                        return Ok(()); // store dropped here, on this worker, never re-entered
                    }
                    Err(_) => {
                        log::warn!("ezbar-wasm: update exceeded {WALL:?} — disabling plugin");
                        return Ok(());
                    }
                };
            if dirty {
                store.set_epoch_deadline(DEADLINE_TICKS);
                let view = match tokio::time::timeout(WALL, plugin.call_view(&mut store)).await {
                    Ok(Ok(tree)) => match lift(&tree) {
                        Ok(l) => Some(l),
                        Err(e) => {
                            log::warn!("ezbar-wasm: view rejected: {e}");
                            None
                        }
                    },
                    Ok(Err(e)) => {
                        log::warn!("ezbar-wasm: view trapped — disabling plugin: {e}");
                        return Ok(());
                    }
                    Err(_) => {
                        log::warn!("ezbar-wasm: view exceeded {WALL:?} — disabling plugin");
                        return Ok(());
                    }
                };
                store.set_epoch_deadline(DEADLINE_TICKS);
                let popup = match tokio::time::timeout(WALL, plugin.call_popup(&mut store)).await {
                    Ok(Ok(Some(tree))) => lift(&tree).ok(),
                    Ok(Ok(None)) => None,
                    Ok(Err(e)) => {
                        log::warn!("ezbar-wasm: popup trapped — disabling plugin: {e}");
                        return Ok(());
                    }
                    Err(_) => {
                        log::warn!("ezbar-wasm: popup exceeded {WALL:?} — disabling plugin");
                        return Ok(());
                    }
                };
                {
                    let mut s = slot.slots.lock().unwrap_or_else(|e| e.into_inner());
                    if view.is_some() {
                        s.view = view;
                    }
                    s.popup = popup;
                }
                // A new frame landed — bump the version so the chip re-renders once.
                slot.version.fetch_add(1, Ordering::Release);
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

// ── on-disk compiled-artifact cache (RFC 0008 §6 Q3) ─────────────────────────
// Compiling a wasm component JITs ~MBs of native code into *private* (dirty) memory
// on every load. Instead we compile once, serialize the artifact to disk keyed by the
// wasm's content hash, and `deserialize_file` (mmap) it thereafter — so the code is
// file-backed Shared_Clean (reclaimable, shared across instances of the same plugin),
// not per-instance private-dirty. This is the lever that takes the per-plugin floor
// from the JIT-code-dominated ~17 MB down toward the linear-memory floor.

/// Load `path` as a component, preferring a cached compiled artifact (mmap'd) and
/// falling back to a fresh compile that is then cached. Blocking — run on the pool.
fn load_component(engine: &Engine, path: &Path) -> Result<Component> {
    let Some(dir) = cache_dir() else {
        return Ok(Component::from_file(engine, path)?); // no cache location → just compile
    };
    let bytes = std::fs::read(path).with_context(|| format!("read {path:?}"))?;
    let key = hash64(&bytes);
    let cached = dir.join(format!("{key:016x}.cwasm"));

    // Fast path: a prior run compiled this exact wasm — mmap the artifact.
    if cached.is_file() {
        // SAFETY: this artifact was produced by our own `serialize`; `deserialize_file`
        // validates it against this engine + wasmtime version and errors on a mismatch
        // (a stale cache then just falls through to the recompile below).
        match unsafe { Component::deserialize_file(engine, &cached) } {
            Ok(c) => return Ok(c),
            Err(e) => {
                log::debug!("ezbar-wasm: stale cache {cached:?} ({e}); recompiling");
                let _ = std::fs::remove_file(&cached);
            }
        }
    }

    // Compile fresh, then publish the artifact for next time (best-effort, atomic).
    let component = Component::from_file(engine, path)?;
    if let Ok(serialized) = component.serialize() {
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(format!(
            "{key:016x}.{}.{:x}.tmp",
            std::process::id(),
            hash64(path.to_string_lossy().as_bytes())
        ));
        if std::fs::write(&tmp, &serialized).is_ok() {
            let _ = std::fs::rename(&tmp, &cached); // atomic publish — readers see whole files
        }
    }
    Ok(component)
}

/// `$XDG_CACHE_HOME/ezbar/wasm` (or `~/.cache/...`). `None` if neither is set — we then
/// compile without caching rather than fall back to a world-writable `/tmp`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("ezbar").join("wasm"))
}

fn hash64(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
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
    /// The reactor task driving this plugin, aborted on `Drop` (RFC 0008 lifecycle).
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WasmModule {
    fn drop(&mut self) {
        // Tear the plugin down when the bar drops it (removed from config, or rebuilt
        // on a `Reconstruct` reconcile). `abort()` cancels any in-flight guest call
        // (wasmtime drop-to-cancel) and drops the `Store` on its driving worker — so
        // the plugin stops running, frees its linear memory, and **ceases to exercise
        // its granted capabilities**. Without this, every config edit would leak a
        // live `Store` and a revoked network grant would keep firing.
        self.task.abort();
    }
}

impl WasmModule {
    /// Load `path` as a plugin with the placement `id`, driven on the reactor that
    /// runs on `rt` (the bar's existing runtime `Handle`, threaded in explicitly —
    /// RFC 0008 §3.1). `config` is the `[modules.<id>]` table flattened to string
    /// pairs; `grants` are the granted network hosts (capabilities).
    pub fn new(
        rt: Handle,
        instance: u64,
        id: impl Into<String>,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
    ) -> Self {
        let slot: Slot = Arc::new(Shared {
            slots: Mutex::new(Slots::default()),
            version: AtomicU64::new(0),
        });
        let task = reactor(&rt).add_plugin(path, config, grants, slot.clone());
        WasmModule {
            id: id.into(),
            instance,
            slot,
            task,
        }
    }
}

impl WasmModule {
    /// Headless snapshot of the latest lifted view/popup node counts. Returns
    /// `(view_nodes, popup_nodes)` — both 0 until the actor has produced a frame
    /// (or if the plugin trapped). Used by the `preview --check` smoke test.
    pub fn debug_snapshot(&self) -> (usize, usize) {
        let s = self.slot.slots.lock().unwrap_or_else(|e| e.into_inner());
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

    fn shutdown(&mut self) {
        // The bar's reconcile calls this explicitly before dropping us; `Drop` is the
        // safety net for every other path. Idempotent — a second `abort` is a no-op.
        self.task.abort();
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        from_recipe(TickRecipe {
            instance: self.instance,
            slot: self.slot.clone(),
        })
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
        let s = self.slot.slots.lock().unwrap_or_else(|e| e.into_inner());
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
        // No content-sized hover mouse_area here: hover is whole-pill, driven by the
        // host from `hover_messages` so the pill's padding ring is hoverable too.
        let s = self.slot.slots.lock().unwrap_or_else(|e| e.into_inner());
        match &s.view {
            Some(l) if !l.nodes.is_empty() => build(l, l.root, ctx, 0),
            _ => text("\u{2026}").color(ctx.fg_dim()).into(),
        }
    }

    fn hover_messages(&self) -> Option<(ModMsg, ModMsg)> {
        // Claim the whole pill as the hover surface — but only when there's a popup
        // to open (no point hovering a chip with nothing behind it).
        let s = self.slot.slots.lock().unwrap_or_else(|e| e.into_inner());
        let has_popup = s.popup.as_ref().is_some_and(|l| !l.nodes.is_empty());
        has_popup.then(|| (ModMsg::new(Msg::Hover), ModMsg::new(Msg::Leave)))
    }

    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let s = self.slot.slots.lock().unwrap_or_else(|e| e.into_inner());
        match &s.popup {
            Some(l) if !l.nodes.is_empty() => Some(build(l, l.root, ctx, 0)),
            _ => None,
        }
    }
}

/// A change-gated render tick. The old version woke the GUI on a blind 150ms timer
/// — re-running `view` and reallocating the whole chip tree ~7×/s per plugin even
/// when the actor produced nothing new (the per-frame allocation "fuel" that, fanned
/// across tokio workers, bloats glibc arenas). This polls the slot's `version` and
/// emits a tick ONLY when a new frame landed: an idle plugin costs zero renders, and
/// a fresh frame still appears within one poll (≤150ms — no latency regression).
struct TickRecipe {
    instance: u64,
    slot: Slot,
}

impl Recipe for TickRecipe {
    type Output = ModMsg;

    fn hash(&self, state: &mut Hasher) {
        use std::hash::Hash;
        std::any::TypeId::of::<Self>().hash(state);
        self.instance.hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: EventStream,
    ) -> ezbar_plugin::iced::futures::stream::BoxStream<'static, ModMsg> {
        use ezbar_plugin::iced::futures::SinkExt;
        let slot = self.slot;
        Box::pin(ezbar_plugin::iced::stream::channel(
            1,
            move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
                // One tick up front so the chip adopts whatever is already cached,
                // then tick only when the version advances (a new frame).
                let mut seen = slot.version.load(Ordering::Acquire);
                let _ = out.send(ModMsg::new(Msg::Tick)).await;
                loop {
                    ezbar_plugin::task::sleep(Duration::from_millis(150)).await;
                    let v = slot.version.load(Ordering::Acquire);
                    if v != seen {
                        seen = v;
                        if out.send(ModMsg::new(Msg::Tick)).await.is_err() {
                            break;
                        }
                    }
                }
            },
        ))
    }
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
