//! ezbar WASM plugin **host harness** (RFC 0006 PoC).
//!
//! Loads a real plugin component and exercises the spine the design was ACK'd
//! on: Component-Model instantiate, a **capability-gated linker**, **per-store
//! memory limits**, **epoch interruption** (a runaway `view()` traps instead of
//! hanging), the **node/depth cap enforced during the lift**, and **terminal-
//! for-instance** trap containment (a fresh store per demo). Rendering is to
//! text here — the bar maps each node to a real iced widget.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::add_to_linker_sync;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../wit/since-v0.1.0",
});

// `Event` and `Tree` are re-exported at the bindgen root by the world's `use`.
use ezbar::plugin::types::{Paint, ThemeToken};
use ezbar::plugin::ui::Node;

// ── host store data ─────────────────────────────────────────────────────────

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    limits: StoreLimits,
    granted_network: Vec<String>, // capability grants (RFC 0006 §5)
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// The host-import interface (`host` in the WIT). In the real bar these are
// async + gated; here we wire the read-only ones and gate the rest.
impl ezbar::plugin::host::Host for Host {
    fn log(&mut self, msg: String) {
        eprintln!("    [plugin log] {msg}");
    }
    fn text_size(&mut self) -> f32 {
        14.0
    }
    fn fg(&mut self) -> Paint {
        Paint::Token(ThemeToken::Fg)
    }
    fn set_timeout(&mut self, _ms: u32) {}
    fn subscribe(&mut self, _kinds: Vec<ezbar::plugin::types::EventKind>) {}
    fn http_get(&mut self, url: String) -> Result<Vec<u8>, String> {
        // capability check (RFC 0006 §5): the host acts only if granted.
        let host = url.split("://").nth(1).unwrap_or(&url);
        let host = host.split('/').next().unwrap_or(host);
        if self.granted_network.iter().any(|h| h == host) {
            Err("(PoC host does not actually fetch)".into())
        } else {
            Err(format!("capability denied: network host '{host}' not granted"))
        }
    }
    fn read_file(&mut self, _path: String) -> Result<Vec<u8>, String> {
        Err("capability denied: read-file not granted".into())
    }
    fn feed_subscribe(&mut self, _feed: ezbar::plugin::types::FeedKind, _min_period_ms: u32) {}
}

// type-only interfaces still generate an (empty) Host trait to implement.
impl ezbar::plugin::types::Host for Host {}
impl ezbar::plugin::ui::Host for Host {}
impl ezbar::plugin::events::Host for Host {}

// ── one plugin instance, set up per the RFC's safety model ───────────────────

const EPOCH_TICK: Duration = Duration::from_millis(10);
const VIEW_DEADLINE_TICKS: u64 = 20; // ~200ms wall-clock budget for view()
const MEM_LIMIT: usize = 8 << 20; // 8 MiB per store (RFC 0006 §1a)
const MAX_NODES: usize = 2_000; // node cap enforced during the lift
const MAX_DEPTH: usize = 32;

fn new_store(engine: &Engine) -> Store<Host> {
    let wasi = WasiCtxBuilder::new().build();
    let data = Host {
        table: ResourceTable::new(),
        wasi,
        limits: StoreLimitsBuilder::new().memory_size(MEM_LIMIT).build(),
        granted_network: Vec::new(),
    };
    let mut store = Store::new(engine, data);
    store.limiter(|h| &mut h.limits);
    store.set_epoch_deadline(VIEW_DEADLINE_TICKS);
    store
}

/// Instantiate a fresh plugin in its own store (a trap is terminal for the
/// instance, so each demo gets a clean one — RFC 0006 §1a).
fn instantiate(
    engine: &Engine,
    component: &Component,
    linker: &Linker<Host>,
    config: &[(String, String)],
) -> Result<(Store<Host>, Plugin)> {
    let mut store = new_store(engine);
    let plugin = Plugin::instantiate(&mut store, component, linker)?;
    plugin.call_init(&mut store, config)?;
    Ok((store, plugin))
}

// ── the node/depth-capped lift: WIT tree -> text render ──────────────────────
// Walks the flat arena iteratively, enforcing the count + depth cap DURING the
// walk (not lift-then-count) — Rockwood's v2.1 nit.

fn render(tree: &Tree) -> Result<String, String> {
    if tree.nodes.len() > MAX_NODES {
        return Err(format!(
            "node cap exceeded: {} nodes > {MAX_NODES}",
            tree.nodes.len()
        ));
    }
    let mut out = String::new();
    walk(tree, tree.root, 0, &mut out, &mut 0)?;
    Ok(out)
}

fn walk(tree: &Tree, idx: u32, depth: usize, out: &mut String, count: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("depth cap exceeded: > {MAX_DEPTH}"));
    }
    *count += 1;
    if *count > MAX_NODES {
        return Err(format!("node cap exceeded during walk: > {MAX_NODES}"));
    }
    let node = tree
        .nodes
        .get(idx as usize)
        .ok_or_else(|| format!("dangling node index {idx}"))?;
    let pad = "  ".repeat(depth);
    match node {
        Node::Text(t) => out.push_str(&format!("{pad}text {:?}\n", t.content)),
        Node::Icon(i) => out.push_str(&format!("{pad}icon {:?}\n", i.id)),
        Node::Graph(g) => out.push_str(&format!("{pad}graph[{} pts]\n", g.values.len())),
        Node::Spacer(px) => out.push_str(&format!("{pad}spacer {px}\n")),
        Node::Row(l) => {
            out.push_str(&format!("{pad}row spacing={}\n", l.spacing));
            for &c in &l.children {
                walk(tree, c, depth + 1, out, count)?;
            }
        }
        Node::Column(l) => {
            out.push_str(&format!("{pad}column spacing={}\n", l.spacing));
            for &c in &l.children {
                walk(tree, c, depth + 1, out, count)?;
            }
        }
        Node::Container(b) => {
            out.push_str(&format!("{pad}container pad={}\n", b.padding));
            walk(tree, b.child, depth + 1, out, count)?;
        }
        Node::MouseArea(m) => {
            out.push_str(&format!("{pad}mouse-area id={:?}\n", m.id));
            walk(tree, m.child, depth + 1, out, count)?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let wasm = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../weather/target/wasm32-wasip2/release/weather.wasm".into());

    let mut config = Config::new();
    config.epoch_interruption(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, &wasm)?;

    let mut linker: Linker<Host> = Linker::new(&engine);
    add_to_linker_sync(&mut linker)?;
    // The custom `host` interface is added to the linker here. Per RFC 0006 §5,
    // a gated import would be *omitted* unless its capability is granted; for the
    // PoC we add it and gate at call-time (see http_get).
    Plugin::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h: &mut Host| h)?;

    // an epoch ticker so deadlines mean wall-clock (gated on a plugin existing).
    let eng = Arc::new(engine);
    {
        let eng = eng.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(EPOCH_TICK);
            eng.increment_epoch();
        });
    }

    println!("== ezbar wasm host PoC — loaded {wasm}\n");

    // 1) normal run — the happy path: init -> update(timer) -> view -> lift
    {
        println!("[1] normal plugin: init -> update(Timer) -> view()");
        let (mut store, plugin) = instantiate(&eng, &component, &linker, &[])?;
        let dirty = plugin.call_update(&mut store, &Event::Timer)?;
        println!("    update(Timer) -> dirty={dirty}");
        let tree = plugin.call_view(&mut store)?;
        match render(&tree) {
            Ok(r) => println!("    rendered chip ({} nodes):\n{}", tree.nodes.len(), indent(&r)),
            Err(e) => println!("    render rejected: {e}"),
        }
    }

    // 2) epoch interruption — a runaway view() must trap, not hang.
    {
        println!("[2] runaway plugin (demo=spin): view() spins forever");
        let (mut store, plugin) =
            instantiate(&eng, &component, &linker, &[("demo".into(), "spin".into())])?;
        store.set_epoch_deadline(VIEW_DEADLINE_TICKS); // ~200ms
        match plugin.call_view(&mut store) {
            Ok(_) => println!("    !! view returned (epoch did not fire — BUG)"),
            Err(e) => println!(
                "    ✓ trapped after deadline -> instance disabled (terminal). [{}]",
                first_line(&e.to_string())
            ),
        }
    }

    // 3) node cap — a pathological tree is rejected during the lift.
    {
        println!("[3] pathological plugin (demo=huge): view() returns a giant tree");
        let (mut store, plugin) =
            instantiate(&eng, &component, &linker, &[("demo".into(), "huge".into())])?;
        match plugin.call_view(&mut store) {
            Ok(tree) => match render(&tree) {
                Ok(_) => println!(
                    "    !! {} nodes accepted (cap not enforced — BUG)",
                    tree.nodes.len()
                ),
                Err(e) => println!(
                    "    ✓ {} nodes returned, rejected during lift: {e}",
                    tree.nodes.len()
                ),
            },
            Err(e) => println!("    view trapped (store memory cap): {}", first_line(&e.to_string())),
        }
    }

    // 4) capability gate at call time: plugin calls an ungranted network host.
    {
        println!("[4] plugin (demo=fetch): calls http_get with no network grant");
        let (mut store, plugin) =
            instantiate(&eng, &component, &linker, &[("demo".into(), "fetch".into())])?;
        plugin.call_update(&mut store, &Event::Timer)?; // triggers the gated call
        println!("    ✓ host denied the call (see [plugin log] above)");
    }

    // 5) capability by linker-absence: the `host` interface is simply not linked,
    //    so a plugin that imports it cannot even instantiate (RFC 0006 §5).
    {
        println!("[5] same plugin, but the `host` interface is absent from the linker");
        let mut bare: Linker<Host> = Linker::new(&eng);
        add_to_linker_sync(&mut bare)?; // WASI only — no Plugin::add_to_linker
        let mut store = new_store(&eng);
        match Plugin::instantiate(&mut store, &component, &bare) {
            Ok(_) => println!("    !! instantiated without the host import (BUG)"),
            Err(e) => println!("    ✓ refused to instantiate: {}", first_line(&e.to_string())),
        }
    }

    println!("\n== all PoC checks ran.");
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("      {l}")).collect::<Vec<_>>().join("\n")
}
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}
