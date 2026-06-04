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

use std::collections::HashMap;
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
use ezbar::plugin::events::{FeedSample, PointerEvent, PointerKind};
use ezbar::plugin::ui::Node;
use ezbar_plugin::iced::mouse::ScrollDelta;

/// The system metric a plugin can subscribe to (RFC 0012). Re-exported so the bar can write
/// the injected [`set_feed_sampler`] closure against it.
pub use ezbar::plugin::types::FeedKind;

// resource bounds (RFC 0006 §1a / RFC 0008: fixed constants)
const EPOCH_TICK: Duration = Duration::from_millis(10);
const DEADLINE_TICKS: u64 = 20; // ~200ms guest CPU before a cooperative epoch yield
const MEM_LIMIT: usize = 2 << 20; // 2 MiB per plugin store (RFC 0008 §3.4: 8→2)
const MAX_NODES: usize = 2_000;
const MAX_DEPTH: usize = 32;
// Legacy heartbeat: the wake cadence for a plugin that never calls `set-timeout` (RFC
// 0011). Plugins that do call it pick their own cadence and idle ones cost zero.
const POLL: Duration = Duration::from_secs(2);
// Floor a guest's self-requested wake cadence at 10 Hz — `set-timeout(1)` would otherwise
// busy-spin update→view→render 1000×/s. A status bar never needs faster (view is a static
// snapshot; motion is host-side, RFC 0010). `0` is exempt — it means "cancel" (RFC 0011 §3.1).
const MIN_TIMER_MS: u32 = 100;
// Per-call wall-clock backstop — the *primary* CPU bound (epoch-yield only smooths, a
// yielding guest never self-traps). Must exceed the 8s http timeout (RFC 0008 §3.4).
const WALL: Duration = Duration::from_secs(12);
// RFC 0009: minimum gap between pointer-driven guest calls — a yield point between calls
// that caps pointer cadence to ~62/s/plugin regardless of input rate or guest slowness.
const MIN_INTERVAL: Duration = Duration::from_millis(16);
// Cap on a guest-controlled `mouse-area` hit id: the lifted arena lives outside the
// store's memory limit, so bound it (same spirit as MAX_NODES).
const MAX_ID_LEN: usize = 64;
// Feed sampling cadence (RFC 0012): the host samples each subscribed metric once per `BASE`
// and fans the value out to every subscriber. A plugin's `min-period-ms` throttles delivery
// per-subscriber but can't go below this — a 48px sparkline never needs faster than 1 Hz.
const BASE: Duration = Duration::from_secs(1);

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
    // The guest's pending `set-timeout` request, written by the import during a guest
    // call and drained by the drive loop right after the call returns (RFC 0011). No
    // lock/Arc: the `Host` *is* the store data, mutated only on the owning fiber.
    timer_request: Option<TimerRequest>,
    // Feed kinds the user granted via `[modules.<id>].feeds` (RFC 0012). An ungranted
    // `feed-subscribe` is logged and dropped — the sandbox stays a sandbox.
    granted_feeds: Vec<String>,
    // Pending `feed-subscribe` requests (a guest may subscribe to several in one call),
    // drained by the drive loop after the call to register with the shared feed hub.
    feed_requests: Vec<(FeedKind, u32)>,
}

/// A guest's `set-timeout` request (RFC 0011). `After(d)` = one `Event::Timer` in `d`;
/// `Cancel` (`ms == 0`) = no timer until re-armed.
enum TimerRequest {
    After(Duration),
    Cancel,
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
    async fn set_timeout(&mut self, ms: u32) {
        // Record the request; the drive loop folds it into the wake cadence after this
        // call returns (RFC 0011). One-shot: the guest re-arms each tick. `0` cancels.
        self.timer_request = Some(match ms {
            0 => TimerRequest::Cancel,
            n => TimerRequest::After(Duration::from_millis(n.max(MIN_TIMER_MS) as u64)),
        });
    }
    async fn subscribe(&mut self, _kinds: Vec<ezbar::plugin::types::EventKind>) {}
    async fn feed_subscribe(&mut self, feed: FeedKind, min: u32) {
        // Capability check (RFC 0012): only deliver feeds the user granted. Fire-and-forget
        // — the frozen WIT has no result, so an ungranted feed is logged and silently never
        // delivered (the plugin can't tell; documented in the SDK contract).
        if self.granted_feeds.iter().any(|g| g == feed_kind_name(feed)) {
            self.feed_requests.push((feed, min));
        } else {
            log::info!(
                "ezbar-wasm: feed '{}' not granted ([modules.<id>].feeds) — ignored",
                feed_kind_name(feed)
            );
        }
    }
    async fn http_get(&mut self, url: String) -> Result<Vec<u8>, String> {
        // host[:port] authority, after the scheme and before the path/query.
        let h = url.split("://").nth(1).unwrap_or(&url);
        let h = h.split(['/', '?', '#']).next().unwrap_or(h);
        if !self.granted_network.iter().any(|g| host_matches(g, h)) {
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
        id: String,
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
            // keep the guest's hit id (RFC 0009 — it routes pointer events back), capped.
            id: m.id.chars().take(MAX_ID_LEN).collect(),
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
    // Shared feed hubs, keyed by metric (RFC 0012). One sampler task per active kind fans a
    // single sample out to every subscriber; the entry is removed when its last subscriber
    // leaves. Keyed by a small index so we don't depend on `FeedKind: Hash`.
    feeds: Mutex<HashMap<u8, FeedHub>>,
}

/// One active metric's fan-out: its subscribers. The sampler task isn't held — it
/// self-terminates (removes its own hub) within one `BASE` of the last unsubscribe, so a
/// detached handle is all we need.
struct FeedHub {
    subs: Vec<Sub>,
}

/// One plugin's subscription to a feed. `token` (the plugin's instance id) keys the upsert
/// so re-subscribing each tick doesn't pile up duplicate `Sub`s. `last_sent` throttles
/// delivery to the guest's requested `min_period` (`None` = never sent → deliver next tick).
struct Sub {
    token: u64,
    tx: tokio::sync::mpsc::Sender<FeedSample>,
    min_period: Duration,
    last_sent: Option<tokio::time::Instant>,
}

/// The bar-injected metric sampler (RFC 0012 §3): the reactor lives *below* the bar in the
/// dependency graph and can't read `/proc`, so the bar hands down a closure that maps a
/// `FeedKind` to its current value (a rate for `net`, a % for `cpu`, …). `None` = unavailable
/// (and `Ping` is deferred → always `None`). Called on the blocking pool.
pub type FeedSampler = dyn Fn(FeedKind) -> Option<f64> + Send + Sync + 'static;

static FEED_SAMPLER: OnceLock<Arc<FeedSampler>> = OnceLock::new();

/// Install the metric sampler. The bar calls this once at startup, before loading plugins,
/// with a closure over its own `/proc`/sysfs readers (RFC 0012 §3). First write wins.
pub fn set_feed_sampler(f: Arc<FeedSampler>) {
    let _ = FEED_SAMPLER.set(f);
}

/// Stable index for a feed kind — the `feeds` map key (avoids needing `FeedKind: Hash`).
fn feed_index(k: FeedKind) -> u8 {
    match k {
        FeedKind::Cpu => 0,
        FeedKind::Memory => 1,
        FeedKind::Temperature => 2,
        FeedKind::Ping => 3,
        FeedKind::Battery => 4,
        FeedKind::Net => 5,
    }
}

/// The grant/config name for a feed kind (matches `[modules.<id>].feeds = [...]`).
fn feed_kind_name(k: FeedKind) -> &'static str {
    match k {
        FeedKind::Cpu => "cpu",
        FeedKind::Memory => "memory",
        FeedKind::Temperature => "temperature",
        FeedKind::Ping => "ping",
        FeedKind::Battery => "battery",
        FeedKind::Net => "net",
    }
}

/// Per-plugin wake cadence (RFC 0011). `Heartbeat` is the legacy auto-renewing 2 s poll for
/// a plugin that never calls `set-timeout`; the first request latches it to explicit
/// one-shot control (`At`) or off (`Idle`), and it never returns to `Heartbeat`.
#[derive(Clone, Copy)]
enum Timer {
    Heartbeat,
    At(tokio::time::Instant),
    Idle,
}

/// Park until this plugin's next timer should fire. `Idle` never fires — a purely reactive
/// plugin then waits only on its input channel and costs zero.
async fn sleep_for(t: Timer) {
    match t {
        Timer::Idle => std::future::pending::<()>().await,
        Timer::Heartbeat => tokio::time::sleep(POLL).await,
        Timer::At(at) => tokio::time::sleep_until(at).await, // absolute → no drift on cancel
    }
}

/// Fold the guest's `set-timeout` request (issued during the call just finished) into the
/// cadence. `None` (no call) leaves the state unchanged — which keeps a `Heartbeat` alive
/// and preserves a pending `At` across a pointer-driven `update` that didn't re-arm. The
/// deadline is `now()` *after* the call, so a slow `update` doesn't compound into the next
/// interval's start (one-shot, not fixed-rate — no catch-up storms). (RFC 0011 §3.)
fn fold_timer(state: &mut Timer, req: Option<TimerRequest>) {
    match req {
        Some(TimerRequest::After(d)) => *state = Timer::At(tokio::time::Instant::now() + d),
        Some(TimerRequest::Cancel) => *state = Timer::Idle,
        None => {}
    }
}

/// Drain the `feed-subscribe`s the guest issued during the call just finished and register
/// each with the shared hub (RFC 0012 §4.3). Requests are already grant-filtered by the
/// `feed_subscribe` import; `min-period-ms` is clamped to `>= BASE` here. Called after every
/// guest call, exactly like `fold_timer`.
fn register_feeds(
    reactor: &'static Reactor,
    token: u64,
    feed_tx: &tokio::sync::mpsc::Sender<FeedSample>,
    store: &mut Store<Host>,
) {
    let reqs: Vec<(FeedKind, u32)> = store.data_mut().feed_requests.drain(..).collect();
    for (kind, min) in reqs {
        let min_period = Duration::from_millis((min as u64).max(BASE.as_millis() as u64));
        reactor.subscribe_feed(kind, token, min_period, feed_tx.clone());
    }
}

/// What woke a plugin's drive loop this iteration. The feed arm (RFC 0012) sits between the
/// pointer input and the timer in the `select!` so a feed (≤1/s) never delays a click.
enum Wake {
    Pointer(PointerEvent),
    Timer,
    Feed(FeedSample),
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
            feeds: Mutex::new(HashMap::new()),
        }
    }

    /// Register `tx` as a subscriber to `kind` (RFC 0012 §4.2). Upsert by `token`: a plugin
    /// that re-subscribes each tick updates its period, never adds a duplicate `Sub`. Spawns
    /// the kind's sampler task on the first subscriber. Runs entirely under the `feeds` lock,
    /// so it can't race the sampler's atomic self-removal.
    fn subscribe_feed(
        &'static self,
        kind: FeedKind,
        token: u64,
        min_period: Duration,
        tx: tokio::sync::mpsc::Sender<FeedSample>,
    ) {
        let key = feed_index(kind);
        let mut feeds = self.feeds.lock().unwrap_or_else(|e| e.into_inner());
        match feeds.get_mut(&key) {
            Some(hub) => {
                if let Some(sub) = hub.subs.iter_mut().find(|s| s.token == token) {
                    sub.min_period = min_period; // re-subscribe → just update the cadence
                } else {
                    hub.subs.push(Sub { token, tx, min_period, last_sent: None });
                }
            }
            None => {
                self.spawn_sampler(kind);
                let subs = vec![Sub { token, tx, min_period, last_sent: None }];
                feeds.insert(key, FeedHub { subs });
            }
        }
    }

    /// The per-kind sampler: every `BASE`, sample once *outside* the lock (the cpu read
    /// blocks ~100ms), then under one lock acquisition fan the value out and atomically
    /// self-remove if no subscribers remain (RFC 0012 §4.2 — closes the teardown race).
    /// Detached — the task ends itself when its hub empties.
    fn spawn_sampler(&'static self, kind: FeedKind) {
        let key = feed_index(kind);
        self.rt.spawn(async move {
            loop {
                tokio::time::sleep(BASE).await;
                // Sample on the blocking pool, holding no lock. If the bar never installed a
                // sampler, disable this feed (remove + exit) rather than spin.
                let Some(sampler) = FEED_SAMPLER.get().cloned() else {
                    log::warn!("ezbar-wasm: no feed sampler installed — disabling feed");
                    self.feeds
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                    return;
                };
                let value = tokio::task::spawn_blocking(move || sampler(kind))
                    .await
                    .ok()
                    .flatten();

                // One critical section: fan-out (non-blocking `try_send`) + prune + the
                // emptiness-test-and-remove. Sharing the lock with `subscribe_feed` is what
                // makes teardown race-free.
                let mut feeds = self.feeds.lock().unwrap_or_else(|e| e.into_inner());
                let Some(hub) = feeds.get_mut(&key) else {
                    return; // already removed elsewhere
                };
                if let Some(v) = value {
                    let now = tokio::time::Instant::now();
                    hub.subs.retain_mut(|sub| {
                        let due = sub
                            .last_sent
                            .is_none_or(|t| now.duration_since(t) >= sub.min_period);
                        if !due {
                            return true; // throttled — keep, deliver a later tick
                        }
                        match sub.tx.try_send(FeedSample { feed: kind, value: v }) {
                            Ok(()) => {
                                sub.last_sent = Some(now);
                                true
                            }
                            // Alive but behind (drive loop parked in a guest call): drop the
                            // *sample*, keep the sub — a gauge wants freshest-or-nothing.
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                            // Receiver gone (plugin torn down): drop the sub.
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        }
                    });
                }
                if hub.subs.is_empty() {
                    feeds.remove(&key); // atomic with the emptiness test — no respawn window
                    return;
                }
            }
        }); // detached — self-terminates when the hub empties
    }

    /// Spawn a green-thread driver for one plugin on the shared runtime. `token` (the
    /// plugin's instance id) keys its feed subscriptions; `grants_feeds` are the granted
    /// metric kinds (RFC 0012).
    #[allow(clippy::too_many_arguments)]
    fn add_plugin(
        &'static self,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
        grants_feeds: Vec<String>,
        token: u64,
        slot: Slot,
        input: tokio::sync::mpsc::Receiver<PointerEvent>,
    ) -> tokio::task::JoinHandle<()> {
        self.rt.spawn(async move {
            if let Err(e) = self
                .drive(path.clone(), config, grants, grants_feeds, token, slot, input)
                .await
            {
                log::warn!("ezbar-wasm: plugin {path:?} stopped: {e:#}");
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &'static self,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
        grants_feeds: Vec<String>,
        token: u64,
        slot: Slot,
        mut input: tokio::sync::mpsc::Receiver<PointerEvent>,
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
                timer_request: None,
                granted_feeds: grants_feeds,
                feed_requests: Vec::new(),
            },
        );
        store.limiter(|h| &mut h.limits);
        // Feed delivery channel (RFC 0012): depth 1 so a slow plugin drops stale samples
        // (drop-newest) rather than queueing them; the drive loop owns `feed_tx` and clones
        // it into each subscription, so `feed_rx` never closes while the loop runs.
        let (feed_tx, mut feed_rx) = tokio::sync::mpsc::channel::<FeedSample>(1);
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

        // Wake cadence (RFC 0011): `Heartbeat` until the guest takes control via `set-timeout`.
        // `init` could already have armed one (a future SDK with a ctx in `load`) — fold it.
        let mut timer = Timer::Heartbeat;
        fold_timer(&mut timer, store.data_mut().timer_request.take());
        register_feeds(self, token, &feed_tx, &mut store);
        // Bootstrap: one immediate `Event::Timer` so the chip paints at t≈0 instead of after a
        // full heartbeat, and a poller arms its real cadence (and feed subscriptions) from the
        // first tick. Explicit — it does NOT consume a one-shot, so a legacy plugin stays on
        // `Heartbeat`.
        if !step(&mut store, &plugin, &slot, &Event::Timer).await {
            return Ok(()); // trapped on the first tick — disabled
        }
        fold_timer(&mut timer, store.data_mut().timer_request.take());
        register_feeds(self, token, &feed_tx, &mut store);

        // Drive loop: a (coalesced) pointer event when one arrives, else a timer tick when the
        // plugin's cadence elapses (`set-timeout`, or the legacy heartbeat). `carry` holds a
        // non-scroll event pulled while coalescing a scroll run, so it's processed next — a
        // click is never reordered across a scroll batch.
        let mut carry: Option<PointerEvent> = None;
        loop {
            let wake = match carry.take() {
                Some(c) => Wake::Pointer(c),
                None => tokio::select! {
                    biased;
                    ev = input.recv() => match ev {
                        Some(e) => Wake::Pointer(e),
                        None => return Ok(()), // all senders gone (module dropped) → stop
                    },
                    // Feed sample (RFC 0012) — after input, before the timer, so a ≤1/s feed
                    // never delays a click. `None` is unreachable (the loop holds `feed_tx`).
                    sample = feed_rx.recv() => match sample {
                        Some(s) => Wake::Feed(s),
                        None => return Ok(()),
                    },
                    _ = sleep_for(timer) => Wake::Timer, // cadence elapsed → a timer tick
                },
            };
            let event = match wake {
                Wake::Timer => {
                    // One-shot consumed on fire: an explicit `At` drops to `Idle` until the
                    // guest re-arms in `update`; the legacy `Heartbeat` auto-renews.
                    if let Timer::At(_) = timer {
                        timer = Timer::Idle;
                    }
                    Event::Timer
                }
                Wake::Feed(sample) => Event::Feed(sample),
                Wake::Pointer(first) if matches!(first.kind, PointerKind::Scroll) => {
                    // Coalesce the leading run of consecutive scrolls (lossless sum); a
                    // non-scroll flushes the run and is carried to the next iteration.
                    let mut delta = first.delta;
                    loop {
                        match input.try_recv() {
                            Ok(e) if matches!(e.kind, PointerKind::Scroll) => delta += e.delta,
                            Ok(e) => {
                                carry = Some(e);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    Event::Pointer(PointerEvent {
                        id: first.id,
                        kind: PointerKind::Scroll,
                        delta,
                    })
                }
                Wake::Pointer(first) => Event::Pointer(first),
            };
            let is_pointer = matches!(event, Event::Pointer(_));
            if !step(&mut store, &plugin, &slot, &event).await {
                return Ok(()); // trap/timeout — store dropped here, on this worker
            }
            // Fold any `set-timeout` the guest issued during this step into the cadence
            // (re-arms a one-shot; `None` leaves a heartbeat or a pending `At` untouched),
            // and register any `feed-subscribe`s it issued.
            fold_timer(&mut timer, store.data_mut().timer_request.take());
            register_feeds(self, token, &feed_tx, &mut store);
            if is_pointer {
                // Cadence gate: a yield + min gap between pointer-driven calls so input
                // can't pin a worker (the real fairness bound — RFC 0009 §3.4).
                tokio::time::sleep(MIN_INTERVAL).await;
            }
        }
    }
}

/// One guest `update(event)` followed by a re-render into the slot if it went dirty.
/// Returns `false` to disable the plugin (trap/timeout) — the caller then drops the
/// `Store` on this worker, never re-entered. Every guest call re-arms the epoch deadline
/// and is wrapped in the WALL backstop (RFC 0008 §3.4 / RFC 0009 implementer checklist).
async fn step(store: &mut Store<Host>, plugin: &Plugin, slot: &Slot, event: &Event) -> bool {
    store.set_epoch_deadline(DEADLINE_TICKS);
    let dirty = match tokio::time::timeout(WALL, plugin.call_update(&mut *store, event)).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            log::warn!("ezbar-wasm: update trapped — disabling plugin: {e}");
            return false;
        }
        Err(_) => {
            log::warn!("ezbar-wasm: update exceeded {WALL:?} — disabling plugin");
            return false;
        }
    };
    if !dirty {
        return true;
    }
    store.set_epoch_deadline(DEADLINE_TICKS);
    let view = match tokio::time::timeout(WALL, plugin.call_view(&mut *store)).await {
        Ok(Ok(tree)) => match lift(&tree) {
            Ok(l) => Some(l),
            Err(e) => {
                log::warn!("ezbar-wasm: view rejected: {e}");
                None
            }
        },
        Ok(Err(e)) => {
            log::warn!("ezbar-wasm: view trapped — disabling plugin: {e}");
            return false;
        }
        Err(_) => {
            log::warn!("ezbar-wasm: view exceeded {WALL:?} — disabling plugin");
            return false;
        }
    };
    store.set_epoch_deadline(DEADLINE_TICKS);
    let popup = match tokio::time::timeout(WALL, plugin.call_popup(&mut *store)).await {
        Ok(Ok(Some(tree))) => lift(&tree).ok(),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            log::warn!("ezbar-wasm: popup trapped — disabling plugin: {e}");
            return false;
        }
        Err(_) => {
            log::warn!("ezbar-wasm: popup exceeded {WALL:?} — disabling plugin");
            return false;
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
    true
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
            sweep_cache(&dir); // a fresh compile = a plugin rebuilt; bound the accumulated artifacts
        }
    }
    Ok(component)
}

/// Cap on cached `.cwasm` artifacts. Each plugin *rebuild* changes the wasm's content hash,
/// so it publishes a NEW artifact and orphans the old — left unbounded the cache grows one
/// (~MB) file per rebuild forever. After publishing, evict the oldest beyond this cap. If an
/// artifact still in use is ever evicted (only if `MAX` newer ones exist), it just recompiles
/// once on the next load and re-publishes — self-healing.
const MAX_CACHED_ARTIFACTS: usize = 24;

/// Delete the oldest `.cwasm` files (by mtime) so the cache holds at most
/// [`MAX_CACHED_ARTIFACTS`]. Best-effort — any I/O error is ignored.
fn sweep_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut arts: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("cwasm"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if arts.len() <= MAX_CACHED_ARTIFACTS {
        return;
    }
    arts.sort_by_key(|(mtime, _)| *mtime); // oldest first
    let evict = arts.len() - MAX_CACHED_ARTIFACTS;
    for (_, p) in arts.into_iter().take(evict) {
        let _ = std::fs::remove_file(&p);
    }
}

/// `$XDG_CACHE_HOME/ezbar/wasm` (or `~/.cache/...`). `None` if neither is set — we then
/// compile without caching rather than fall back to a world-writable `/tmp`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("ezbar").join("wasm"))
}

/// Does a `[modules.<id>].network` grant authorize requests to `url_host` (the
/// `host[:port]` lifted from the URL)? Case-insensitive (DNS is); a port-less grant
/// authorizes the host on any port, while a grant that pins a `:port` must match exactly.
/// Replaces the old naive `grant == url_host`, which rejected `API.Example.com` or an
/// explicit `:443` against an `example.com` grant.
fn host_matches(grant: &str, url_host: &str) -> bool {
    let g = grant.trim().to_ascii_lowercase();
    let h = url_host.trim().to_ascii_lowercase();
    if g.is_empty() {
        return false;
    }
    if g.contains(':') {
        g == h // grant pins a port → exact host:port
    } else {
        h.split(':').next() == Some(g.as_str()) // host-only grant → any port
    }
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
    /// A pointer event on one of the plugin's `mouse-area`s (RFC 0009), forwarded to the
    /// drive task and delivered to the guest as `Event::Pointer`.
    Pointer(PointerEvent),
}

/// A loaded WASM plugin, presented to the bar as a [`Module`].
pub struct WasmModule {
    id: String,
    instance: u64,
    slot: Slot,
    /// GUI → drive-task pointer events (RFC 0009). Bounded; `try_send` never blocks the
    /// GUI thread.
    input: tokio::sync::mpsc::Sender<PointerEvent>,
    /// Coalesced scroll not yet on the channel: `(id, summed delta)`. When the channel is
    /// full a scroll merges in here instead of being dropped (no scroll lost), so under a
    /// flood the backlog collapses to ≤1 pending message and can never fill the channel out
    /// of a queued `press` (RFC 0009 §3.4 — protect clicks).
    pending_scroll: Option<(String, f32)>,
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
    /// pairs; `grants` are the granted network hosts, `grants_feeds` the granted system
    /// metric feeds (RFC 0012). `instance` doubles as the feed-subscription token.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rt: Handle,
        instance: u64,
        id: impl Into<String>,
        path: PathBuf,
        config: Vec<(String, String)>,
        grants: Vec<String>,
        grants_feeds: Vec<String>,
    ) -> Self {
        let slot: Slot = Arc::new(Shared {
            slots: Mutex::new(Slots::default()),
            version: AtomicU64::new(0),
        });
        let (input, rx) = tokio::sync::mpsc::channel(32);
        let task = reactor(&rt).add_plugin(
            path,
            config,
            grants,
            grants_feeds,
            instance, // feed-subscription token
            slot.clone(),
            rx,
        );
        WasmModule {
            id: id.into(),
            instance,
            slot,
            input,
            pending_scroll: None,
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
            // Forward a pointer event to the drive task (non-blocking). Scrolls are
            // coalesced into `pending_scroll` and flushed as one message, so a scroll
            // flood never fills the channel and evicts a queued `press` (RFC 0009 §3.4).
            Some(Msg::Pointer(pe)) => {
                if matches!(pe.kind, PointerKind::Scroll) {
                    let acc = self.pending_scroll.take().map_or(0.0, |(_, d)| d) + pe.delta;
                    let merged = PointerEvent {
                        id: pe.id.clone(),
                        kind: PointerKind::Scroll,
                        delta: acc,
                    };
                    // On a full channel, retain the accumulator (no scroll is ever lost).
                    if self.input.try_send(merged).is_err() {
                        self.pending_scroll = Some((pe.id.clone(), acc));
                    }
                } else {
                    // Flush any pending scroll first (preserve order), then the discrete tap.
                    if let Some((id, delta)) = self.pending_scroll.take() {
                        let _ = self.input.try_send(PointerEvent {
                            id,
                            kind: PointerKind::Scroll,
                            delta,
                        });
                    }
                    let _ = self.input.try_send(pe.clone());
                }
                Response::none()
            }
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

/// Normalize a wheel/touchpad scroll to **line-equivalents** before it crosses the frozen
/// `f32` ABI (RFC 0009): a notched wheel gives `Lines` (±1/notch), a touchpad gives
/// `Pixels` (tens per tick); without this the same gesture would differ ~50× and the guest
/// couldn't tell. `16` ≈ a line height in px.
fn scroll_lines(d: ScrollDelta) -> f32 {
    match d {
        ScrollDelta::Lines { y, .. } => y,
        ScrollDelta::Pixels { y, .. } => y / 16.0,
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
        LNode::MouseArea { child, .. } => measure(l, *child),
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
        // RFC 0009: route pointer events back to the guest. `press`/`right-press` fire on
        // button-**down** over the widget — a discrete tap. (iced's `on_release` is *not* a
        // completed-click primitive: it fires on any release-while-hovering regardless of
        // where the press began, so it would invent phantom clicks on drag-onto. v1 has no
        // release/motion, so no drag/cancel semantics.) Scroll is normalized to
        // line-equivalents host-side before the f32 ABI.
        LNode::MouseArea { child, id } => {
            let inner = build(l, *child, ctx, depth + 1);
            let ptr = |kind, delta| {
                ModMsg::new(Msg::Pointer(PointerEvent {
                    id: id.clone(),
                    kind,
                    delta,
                }))
            };
            let scroll_id = id.clone();
            mouse_area(inner)
                .on_press(ptr(PointerKind::Press, 0.0))
                .on_right_press(ptr(PointerKind::RightPress, 0.0))
                .on_enter(ptr(PointerKind::Enter, 0.0))
                .on_exit(ptr(PointerKind::Leave, 0.0))
                .on_scroll(move |d| {
                    ModMsg::new(Msg::Pointer(PointerEvent {
                        id: scroll_id.clone(),
                        kind: PointerKind::Scroll,
                        delta: scroll_lines(d),
                    }))
                })
                .into()
        }
        LNode::Icon { id, color, size } => id.view(*size, paint_color(color, ctx)),
        LNode::Graph { values, kind, line } => canvas(Graph::new(
            values.clone(),
            *kind,
            Some(paint_color(line, ctx)),
        ))
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

#[cfg(test)]
mod tests {
    use super::host_matches;

    #[test]
    fn host_grant_is_case_insensitive_and_port_agnostic() {
        assert!(host_matches("api.open-meteo.com", "api.open-meteo.com"));
        assert!(host_matches("API.Open-Meteo.com", "api.open-meteo.com")); // case
        assert!(host_matches("api.open-meteo.com", "api.open-meteo.com:443")); // any port
        assert!(host_matches(" api.open-meteo.com ", "api.open-meteo.com")); // trimmed
    }

    #[test]
    fn port_pinned_grant_matches_exactly() {
        assert!(host_matches("example.com:8080", "example.com:8080"));
        assert!(!host_matches("example.com:8080", "example.com:9090"));
        assert!(!host_matches("example.com:8080", "example.com")); // pinned port required
    }

    #[test]
    fn unrelated_host_or_empty_grant_is_denied() {
        assert!(!host_matches("example.com", "evil.com"));
        assert!(!host_matches("example.com", "sub.example.com")); // no implicit subdomain
        assert!(!host_matches("", "example.com"));
    }
}
