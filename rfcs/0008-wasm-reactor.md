# RFC 0008: the WASM reactor — one shared runtime, async I/O, N cheap plugins

- **Status:** **Accepted** (v3) — ACK'd by a systems review (Torvalds) after two NAK
  rounds; all six blocking items (four first-pass, two second-pass) folded (see *Review
  response*). Cleared for implementation.
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Supersedes:** the "off-GUI **actor thread**" execution model of RFC 0006 §1a (the
  WIT ABI, the bounded UI DSL, the capability model, and the lift/cap pipeline are all
  **unchanged** — this RFC only changes *how the host drives the guests*).
- **Prior art studied:** Zellij (MIT — one shared wasmtime runtime hosting many plugin
  instances, driven asynchronously).

## Review response (what changed in v2)

NAK'd on the memory premise and two design holes. v2 resolves all four:

- **The savings were asserted, not substantiated — and I measured under the wrong
  premise.** The +27 MB/plugin was measured *on the jemalloc build*, so it is **not**
  glibc per-thread arena retention (jemalloc doesn't do 64 MB arenas). The only real
  lever is therefore the **shared `Engine`** (its compiled WASI/component support,
  duplicated per engine today) vs the **per-`Store`** residue (guest code + heap) that
  the reactor does *not* save. §2 now commits to a **target floor + a stop condition**,
  and §4 to an **attribution method** that isolates the two. (Torvalds #1)
- **Runtime ownership.** Plugins drive on the bar's **existing** tokio runtime (iced's
  `BoundedExecutor`, `main.rs`), not a new one — a second runtime adds threads for no
  reason. §3.1. (Torvalds #2)
- **Epoch story corrected.** The "long I/O on A no longer freezes B" claim was false
  (the old pause was per-plugin). The real bound is the **wall-clock `timeout` (primary)
  + epoch-yield (cooperative smoothing)**; a yielding guest never self-traps, so **≥2
  workers, not 1.** §3.4. (Torvalds #3)
- **Shared-fate isolation downgrade is now explicit**, with the binding invariant *host
  imports must return `Err`, never panic.* §3.4. (Torvalds #4)

**Second pass** added two more, both folded: the per-`Store` floor is measured as a
*slope* on a warm engine (not instance #1, which folds in the one-time Engine cost), §4;
and the runtime `Handle` is threaded **explicitly**, with the preview harness — including
headless `--check` — owning its own runtime, §3.1.

## 1. Problem — the actor model is O(N) in the expensive things

RFC 0006 phase-2 (`crates/ezbar-wasm`) gives **every plugin its own OS thread and its
own wasmtime `Engine`** (`WasmModule::new → spawn_actor → run_actor`): each actor builds
an `Engine` + `Component` + `Store`, spawns a **second** thread (an epoch ticker), and
runs a blocking `update→view→sleep(2s)` loop. Host calls are **synchronous** — `http_get`
blocks the actor's thread for up to 8 s, which is *the whole reason* each plugin needs
its own thread.

Fine for 1–3 plugins; it does not scale. **Measured on a real multi-monitor box, on the
jemalloc build** (10 copies of `weather.wasm`, settled):

| | 1 plugin | 10 plugins | per-plugin |
|---|---|---|---|
| RSS | 254 MB | 502 MB | **+27 MB** |
| Private-dirty | 126 MB | 362 MB | **+26 MB** |
| OS threads | 57 | 75 | **+2** |

≈ **27 MB + 2 OS threads per plugin** → 30 plugins ≈ **+800 MB, ~60 threads**. The 148 KB
of `weather.wasm` is irrelevant; the cost is the per-plugin wasmtime instance. Three
things scale with N that shouldn't: **(1)** Engines (one per plugin, each re-holding
wasmtime's compiled WASI/component glue), **(2)** OS threads (two per plugin), **(3)** the
8 MiB linear-memory cap reserved per instance.

## 2. Goal — and the falsifiable acceptance bar

The Zellij shape: **one shared runtime, plugins are cheap instances, an async reactor
drives them all.** One `Engine`, one epoch ticker, the bar's existing worker pool — for
*any* N. Thread count **O(1)**, not O(N).

**Acceptance is a measured number, not a story** (I guessed "5 MB" once and was 5× off;
I will not assert a floor I haven't measured):

- **Target:** per-plugin marginal RSS on **10 distinct plugins** ≤ **10 MB** (from ~27),
  i.e. a **≥2.5× reduction**, with thread count flat in N.
- **Attribution required (§4):** the win must be shown to come from collapsing the
  per-Engine residue; the per-`Store` floor (guest code + committed heap) must be
  measured *in isolation* and named.
- **Stop condition:** if the per-`Store` floor alone exceeds ~10 MB, the premise is
  wrong — the bulk was never the Engine — and we **do not ship** the reactor on a
  memory argument (it may still be worth it on thread count; that's a separate call).

## 3. Design

### 3.1 One shared runtime, on the bar's existing executor
A process-global `Reactor` (lazy `OnceLock`), built once on first plugin load:
- one `Engine` with `Config { async_support: true, epoch_interruption: true }`;
- one async `Linker<Host>` (`wasmtime_wasi::p2::add_to_linker_async` + the bindgen async
  host imports), reused for every instantiation;
- one **epoch ticker** thread: `loop { sleep(10ms); engine.increment_epoch() }` (a plain
  sleep loop, *not* a runtime — the single extra thread the host owns);
- **no new tokio runtime.** Plugin tasks spawn onto the bar's **existing**
  `BoundedExecutor` (the 2-worker runtime iced already runs, `main.rs`). The `Handle` is
  threaded **explicitly** — `WasmModule::new(handle, …)`, passed down through
  `modules::build` — *not* taken from an ambient `Handle::current()`. The bar captures the
  handle once in `Bar::new` (which iced runs inside the executor context, so
  `Handle::current()` is valid *there*) and threads it to every plugin it builds, on the
  initial build and on hot-reload alike. The **preview harness owns its own small runtime
  and passes that handle** — including **headless `--check`**, which has no iced loop at
  all: it builds a 1-worker tokio runtime, hands the reactor that handle, and polls
  `debug_snapshot()` as today. A second *reactor* runtime is rejected: it adds worker
  threads (and, pre-jemalloc, would have re-created the arena bloat the bar fights) for
  zero benefit.

### 3.2 Plugins are async tasks, not threads
`WasmModule::new` registers the plugin with the reactor, which spawns a **green-thread
task** on the shared runtime:

```text
drive(engine, linker, client, path, cfg, grants, slot):
  component = spawn_blocking(|| compile_or_load_cache(path)).await   // §6 Q3
  store = Store::new(engine, Host{ grants, client, wasi, limits(2 MiB) })
  store.set_epoch_deadline(D); store.epoch_deadline_async_yield_and_update(D)
  plugin = instantiate_async(store, component, linker).await
  plugin.call_init(store, cfg).await
  loop:
    match timeout(WALL, plugin.call_update(store, Timer)).await:   // WALL > 8s http
        Ok(Ok(dirty)) if dirty: view/popup → lift+cap+validate → publish slot, bump ver
        _ => disable(this plugin); drop(store); return            // trap / timeout / err
    sleep(POLL)
```

N tasks, O(1) OS threads. The `Module`/slot interface to the bar is **untouched** —
`WasmModule::view` reads the cached lifted tree; the bar's `view` never calls a store.

### 3.3 Async host I/O is the unlock
With `bindgen!({ async: true })` the host imports are `async fn`, so `http_get` does an
**async** fetch (async `reqwest::Client`, rustls, 8 s timeout). The guest still calls it
*synchronously* — wasmtime suspends that guest's fiber, the reactor runs the other 29
plugins, and resumes it when the bytes arrive. One loop, many plugins, no blocking. This
also **deletes the `epoch_paused` hack**: a fiber parked in a host `await` runs no guest
code, so it burns no epoch — naturally.

(The guest still has no *guest-side* concurrency: it blocks its own fiber on one host
call at a time. That's correct for a status chip; async-host ≠ async-guest.)

### 3.4 Bounds, isolation, and the shared-fate downgrade
- **Per-store memory limit lowered 8 MiB → 2 MiB** (`StoreLimits`); a chip's guest heap
  is tiny. Flat, no per-plugin knob (RFC 0006 v2.1: no tunables for zero users); revisit
  on evidence of a real plugin needing more.
- **Node-count + depth cap on lift:** unchanged (RFC 0006 §1a/v2.1).
- **CPU bound — the `timeout` is primary, epoch-yield is smoothing.**
  `epoch_deadline_async_yield_and_update(D)` makes a CPU-bound guest *yield* every ~200 ms
  instead of trapping, so it can't starve the shared workers — but a yielding guest
  **never self-traps**, so the load-bearing kill is the per-call wall-clock
  `tokio::time::timeout(WALL, …)` (WALL > the 8 s http timeout). On timeout the in-flight
  call future is dropped — which unwinds the fiber — and the plugin is disabled. **The
  reactor needs ≥2 workers** — not because a guest pins a worker (it doesn't: it yields
  every ~200 ms on CPU and frees the worker on every I/O await), but so the timeout timers
  and other plugins' tasks stay live through a busy plugin's yield cycles, with headroom
  for concurrent guest compute.
- **Trap / OOM / timeout are per-`Store`** → terminal for exactly one plugin (frozen slot
  / error chip), never the reactor, never the bar. Invariant (code comment): a poisoned
  store is dropped **on the worker that was driving it** (automatic with `timeout`, which
  owns the future) and **never re-entered** — no `save_state`, no further call.
- **Shared-fate downgrade (the one isolation property weaker than N engines).** All
  plugins now share one runtime and one engine. *Safety* (traps/OOM/limits) stays
  per-store. *Scheduling* and *host-side panics* are now shared: a `panic!` inside a host
  import executes on a shared worker and could wedge the engine. **Binding invariant:
  every host import — and the lift/validate pass, and anything else running on a shared
  worker — returns `Err`, never panics** — no `unwrap`, no indexing, no `expect`;
  `reqwest`/parse errors map to `Err`. (The current `http_get` is already panic-free; keep
  it so under async.) This is an accepted, explicit trade for O(1)
  threads; the alternative is the N-runtime model we're removing.

### 3.5 What does NOT change
- The WIT `since-v0.1.0` — byte-identical. **Guest `.wasm` need no rebuild**: the imports
  are synchronous `func`s in the WIT; going async is purely host-side and the canonical
  ABI lowering the guest sees is identical (verified against `world.wit` + the wasmtime
  async lowering). The `since-v0.1.0` freeze holds.
- The bounded widget DSL, the `lift`/`paint`/`icon`/`build` pipeline, the node/depth caps,
  the capability model (grants enforced by linker absence), `WasmModule`, the slot, the
  change-gated `TickRecipe`.

## 4. Memory model — and how we prove it (not assert it)

Measured **under jemalloc**, so the 26 MB private-dirty/plugin is **not** glibc thread
arenas. It is the per-plugin wasmtime instance, which splits into:

| source | actor (today) | reactor | recoverable? |
|---|---|---|---|
| `Engine` + its compiled WASI/component support | ×N | **×1 shared** | **yes — the whole lever** |
| OS threads (+ tcache/stack) | 2×N | ~2 total | yes (small under jemalloc) |
| compiled guest `Component` code | ×N | ×N | no (per-.wasm) |
| per-`Store` committed linear memory + tables | ×N | ×N (cap 8→2 MiB) | partly (cap) |

**The entire memory case rests on the first row being the bulk — which is unproven.**
Attribution method, run before declaring success:

1. **Isolate the per-`Store` floor — as a *slope*, not instance #1.** The engine is built
   lazily on first load (§3.1), so instance #1's marginal RSS folds in the one-time Engine
   cost — and the §2 stop condition hinges on the clean floor. So measure the floor as the
   **per-additional-instance delta on a warm engine** (RSS of instance N+1 minus N — the
   slope across 1→2→3 of the same component). *That* delta is the irreducible per-instance
   cost (code + heap). Name it.
2. **Isolate the per-`Engine` residue.** Compare 10 instances on **one** engine (reactor)
   vs 10 on **ten** engines (today). The delta per plugin *is* the Engine residue — the
   thing we're collapsing. This is the number that decides the RFC.
3. **Test 10 *distinct* plugins, not just 10 copies** (weather, btc, quakes, clock,
   loadgauge, …). Identical copies are *not* flattered here — the host compiles each
   `.wasm` separately (no Component dedup) — but distinct plugins are the real desktop, so
   measure them too.
4. Report all three with `smaps_rollup` private-dirty. Pass = per-plugin ≤ 10 MB and the
   delta in (2) accounts for most of the 27→floor drop.

## 5. Alternatives considered

- **Keep thread-per-plugin (status quo).** O(N) threads *and* O(N) engines; measured ~27
  MB + 2 threads/plugin. Rejected.
- **Sync shared engine + a blocking thread-pool for host calls.** Suspending a guest
  across a blocking http call needs async wasmtime anyway; the pool variant is more moving
  parts for the same result. Rejected.
- **wasmtime pooling allocator** (`PoolingAllocationConfig`) — pre-reserves a fixed pool
  of instance slots; bounds/speeds instantiation but reserves upfront. Orthogonal,
  composable; **deferred** (revisit if instantiation cost shows up).

## 6. Open questions — resolved

1. **Workers / runtime: reuse the bar's existing `BoundedExecutor` (2 workers).** Not a
   new runtime; not 1 worker. The pool is now shared three ways — bar I/O, ~18 built-in
   modules' async polls, and all plugin fibers — so **acceptance also measures the tail
   latency of a built-in HTTP poll with 30 plugins loaded**; if it regresses, the pool
   sizing (not the design) is revisited.
2. **Mem cap: flat 2 MiB**, no knob — but **measure committed heap** (§4); if a real
   plugin needs more we'll learn fast and revisit on evidence.
3. **Startup compile: `spawn_blocking` + the module cache.** Inline serializes 30 startup
   compiles across 2 workers (stalls running plugins); bare `spawn_blocking` re-pays on
   every hot-reload — so cache (RFC 0006's `deserialize` path) makes steady-state free.
4. **`set_timeout`: still a no-op (fixed 2 s POLL) — out of scope**, but the immediate
   follow-up. Event-driven cadence is the one Zellij-defining trait still stubbed; the 2 s
   blind poll per task is what makes 30 idle plugins each wake 0.5×/s for nothing.

## 7. Risk register

- **Async wasmtime complexity** (fibers, `Send` bounds, async-fn-in-trait). Contained to
  `crates/ezbar-wasm`. The precise bound: `func_wrap_async` requires the **returned future
  be `Send`**, and `call_async` requires `Store::Data: Send` — so `Host` must be `Send`
  **and nothing `!Send` may be held across an `await`** in any host import (rustls
  `reqwest` future is `Send`; keep it that way). `Store::Data: Send` alone is necessary,
  not sufficient.
- **Fiber cancellation on timeout** unwinds the fiber (wasmtime supports drop-to-cancel);
  the future must be dropped on its driving worker — automatic under `timeout`. Never
  cancel via an out-of-band handle (that would be a use-after-fiber).
- **Shared scheduling fate + host-side panics** — see §3.4; accepted trade, guarded by the
  no-panic-in-imports invariant.
- **Startup thundering herd** (30 compiles) — `spawn_blocking` + cache (§6 Q3).
