# RFC 0012: host-computed feeds — system metrics as a safe plugin capability

- **Status:** **Implemented** (v2) — shipped in the reactor + both guest SDKs + a `sysgraph`
  dogfood plugin + the bar's injected sampler. ACK'd by a systems reviewer (Torvalds) and a
  sandbox/platform reviewer at design time *and* on the implementation: the systems reviewer
  traced the teardown race closed (single critical section, `feeds` lock never held across an
  `.await`), `try_send` Full/Closed, upsert-by-token, and net priming; the platform reviewer
  confirmed cross-SDK parity, the sandbox, and that the dogfood plugin renders. (Design review
  was two soft NAKs — "architecture sound, lifecycle under-specified" — folded in below.)
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / wasmtime reactor + the bar's metric sources)
- **Depends on:** RFC 0006 (the frozen `host.feed-subscribe` + `events.feed` + `feed-kind`
  WIT), RFC 0008 (the reactor + per-plugin drive loop), RFC 0011 (the `timer_request`
  drain-after-call pattern this mirrors). No WIT change — the surface is already frozen.

## What changed in v2 (review fold-ins)

Both reviewers ACK'd the architecture (bar-injects-sampler decoupling, one-sample-per-kind
fan-out, idle-zero, sandbox-preserving) and converged on five fixes — four lifecycle/
concurrency bugs and one blocking SDK gap. All folded in:

1. **Teardown race — fixed (was Open Q4).** "Last `Sub` drops → task exits → hub removed"
   had a window: a new subscribe could insert into a hub the dying task then deleted, silently
   stranding it. Fix (§4.2): the sampler's **emptiness test and `HashMap::remove` happen
   under one single lock acquisition**, and `subscribe_feed` inserts-or-spawns under the *same*
   lock — so either subscribe wins (hub stays, task sees non-empty, lives) or the task wins
   (hub removed atomically, subscribe spawns fresh). No generation counter needed. The slow
   *sample* runs **outside** the lock; only the (non-blocking `try_send`) fan-out + prune run
   inside.
2. **Send-error is the wrong liveness signal on a bounded channel — fixed (was Open Q3/Q5).**
   A momentarily-slow-but-alive plugin (drive loop parked in a 12 s `http_get`) backs its
   channel up; treating *any* send error as "gone" would silently, permanently drop it. Fix
   (§4.2): `try_send`, and **distinguish `Full` (drop the *sample*, keep the `Sub`) from
   `Closed/Disconnected` (drop the `Sub`)**. Depth-1 bounded channel, drop-newest — a gauge
   wants the freshest sample or nothing, never a stale queue.
3. **Re-subscribe-each-tick leaked `Sub`s — fixed.** Unlike `timer_request` (an `Option` that
   *replaces*), `feed_requests` accumulates and `subscribe_feed` was per-entry `push`, so a
   plugin re-asserting `feed_subscribe(Cpu, …)` every `update` would get a *second* `Sub` each
   tick → N copies, unbounded fan-out. Fix (§4.3): `subscribe_feed` is an **upsert keyed by a
   per-drive-task token** (the plugin's `instance` id, plumbed into the drive loop) — re-subscribe
   updates `min_period`, never adds a duplicate.
4. **Sampler-unset + net priming (was Open Q1/§3).** If `set_feed_sampler` hasn't run at first
   subscribe, `subscribe_feed` logs once and registers nothing (never `None.unwrap()`). The net
   sampler is stateful (rate = counter delta), so on every (re)spawn it **primes `prev` with one
   throwaway read and emits nothing until the second tick** — no garbage spike from a stale `dt`.
5. **BLOCKING: neither SDK could call it — fixed.** The Rust *and* Go `Ctx` had no
   `feed_subscribe` method (the v1 examples called a doorknob that didn't exist). Fix (§4.4):
   add `feed_subscribe` to **both** SDKs with one canonical docstring, documenting the
   **fire-and-forget / no-delivery-guarantee** contract (the frozen WIT `feed-subscribe` returns
   nothing — unlike `http_get` it *cannot* signal denial synchronously, so an ungranted or
   deferred kind is silent; the host logs `module id + kind`, and authors must not busy-wait on
   a feed that may never arrive).

## 1. Problem

RFC 0007 found that the *powerful* widgets (cpu/memory/temperature/net graphs) **can't be
plugins today**: a sandboxed WASM plugin has no way to read `/proc`, and we are not going to
hand it filesystem access. So every metric widget stays a built-in — the platform can host a
weather chip but not a cpu graph, which is backwards (the cpu graph is the more obviously
"pluginnable" thing).

The fix is already designed and half-shipped, exactly like `set-timeout` was (RFC 0011): the
frozen v0.1.0 WIT **already** declares the whole surface —

```wit
feed-subscribe: func(feed: feed-kind, min-period-ms: u32);   // host import (gated)
enum feed-kind { cpu, memory, temperature, ping, battery, net }
record feed-sample { feed: feed-kind, value: f64 }
variant event { …, feed(feed-sample), … }
```

— the **guest SDKs already decode `Event::Feed`** (`from_event` maps it; `Feed` enum exists),
and the host **stubs `feed_subscribe` as a no-op** (`crates/ezbar-wasm/src/lib.rs`) and never
delivers a sample. This RFC lights it up: the host samples the metric and pushes
`Event::Feed { feed, value }` to a plugin that subscribed — **capability-gated**, so a plugin
gets exactly the numbers the user granted and nothing else. The sandbox stays a sandbox: the
plugin still can't touch `/proc`, the *host* reads it and hands over one `f64`.

## 2. Goal & non-goals

**Goal.** A plugin draws a live cpu/memory/temperature/battery/net graph or readout with no
subprocess and no filesystem access: `ctx.feed_subscribe(Feed::Cpu, 1000)` and then handle
`Event::Feed { feed: Cpu, value }` in `update`. One host timer samples each metric once and
fans the value out to every subscriber.

**Non-goals / deferred (see §6):**
- **`ping` feed** — the parameterless `feed-kind::ping` has nowhere to put a *target host*.
  Deferred: `feed-subscribe(ping)` is accepted but logs "needs a target, unsupported in v1"
  and registers nothing. (A future host-config default target can light it up additively.)
- **Read-only sway IPC** (workspaces/title to plugins) — the *other* half of the P1 "safe
  capabilities" TODO. It is **not** in the frozen WIT (no host call for it), so it needs a
  new `since-v0.2.0` WIT version and is its own RFC. Out of scope here.
- **`host.subscribe(kinds)`** (event-kind filtering) — still a no-op; `feed-subscribe` is the
  registration channel for feeds, so `subscribe` buys nothing for this feature.
- Splitting net into down/up — the frozen `feed-sample` carries a single `f64` (§6).

## 3. The architecture problem: the reactor can't read `/proc`

The metric readers live in the **`ezbar` bin crate** (`src/sources/system.rs`:
`get_cpu_usage`/`get_memory_usage`/`get_cpu_temperature` + `extract_*_value`;
`src/sources/battery.rs`; net's `/proc/net/dev` reader). The reactor lives in the
**`ezbar-wasm`** crate, which is a *dependency* of `ezbar` — the arrow points
`ezbar → ezbar-wasm`, so the reactor **cannot** call `crate::sources::*`. We are not
duplicating `/proc` parsing into the reactor, and not hoisting all the sources into a third
crate for this.

**Resolution — the bar injects a sampler.** `ezbar-wasm` exposes a one-time registration:

```rust
// in ezbar-wasm
pub type FeedSampler = dyn Fn(FeedKind) -> Option<f64> + Send + Sync + 'static;
pub fn set_feed_sampler(f: std::sync::Arc<FeedSampler>);   // bar calls once at startup
```

The bar (in `main.rs` startup) installs a closure over its own sources:

```rust
ezbar_wasm::set_feed_sampler(Arc::new(|kind| match kind {
    FeedKind::Cpu    => Some(system::extract_cpu_usage_value(&system::get_cpu_usage())),
    FeedKind::Memory => Some(system::extract_memory_usage_value(&system::get_memory_usage())),
    FeedKind::Temperature => Some(system::extract_temperature_value(&system::get_cpu_temperature())),
    FeedKind::Battery => battery_percent(),         // parse "NN%" off get_battery_status()
    FeedKind::Net     => net_total_rate(),          // FnMut-ish: owns prev counters via a Mutex
    FeedKind::Ping    => None,                       // deferred — no target in the ABI
}));
```

The reactor owns the **fan-out + cadence**; the bar owns the **metric knowledge**. Clean
layering, zero `/proc` in the reactor, and the sampler is trivially swappable for tests.
The closure is called on the blocking pool (`get_cpu_usage` does a blocking ~100 ms two-shot
`/proc/stat` read). `net_total_rate` keeps its previous `(rx,tx,instant)` in a `Mutex` it
captures, computing a bytes/s rate per call (the reactor treats every kind as an opaque
gauge — it never knows net is a counter).

**Sampler unset at first subscribe.** `main.rs` installs the sampler *before* loading any
plugin, but the reactor must not assume it: if `set_feed_sampler` hasn't run when a
`subscribe_feed` arrives, the reactor **logs once and registers nothing** (the `Sub` is
dropped) — the same silent-no-delivery outcome as an ungranted kind, never a `None.unwrap()`.

**Stateful net priming.** Because the net sampler derives a *rate* from a counter delta, its
captured `prev` is meaningless on the first read after a (re)spawn (the last subscriber may
have left long ago). On every (re)spawn it **primes `prev` with one throwaway read and emits
nothing until the second tick**, so no subscriber ever sees a garbage spike from a stale `dt`.

## 4. Design

### 4.1 Capability grant (sandbox stays a sandbox)
A new grant `[modules.<id>].feeds = ["cpu", "net"]` (string or array), parsed exactly like
`network` is today (`network_grants`). It flows into `WasmModule::new` → the drive task as
`Vec<FeedKind>`. `feed-subscribe(kind, _)` checks the kind is granted; **ungranted → logged
and ignored** (no registration, no samples). Consistent with `http_get`'s runtime allowlist
(the frozen WIT links every import; gating is a runtime check, not linker-absence).

### 4.2 The shared feed hub (one sample, N subscribers)
The reactor gains a process-wide registry:

```rust
struct FeedHub { subs: Vec<Sub>, task: JoinHandle<()> }     // one per active FeedKind
struct Sub { token: u64, tx: mpsc::Sender<FeedSample>, min_period: Duration, last_sent: Instant }
// in Reactor: feeds: Mutex<HashMap<FeedKind, FeedHub>>
```

- **First** subscriber to a kind spawns **one** sampler task. Each tick (`BASE = 1 s`):
  1. **Sample *outside* the lock** — `let v = spawn_blocking(|| sampler(kind)).await;`. Never
     hold the registry `Mutex` across the ~100 ms cpu read.
  2. **Then take the lock once** and, in a single critical section: prune dead subs, fan out,
     and decide-and-remove atomically (below). Fan-out uses `try_send` (non-blocking):
     - `Ok` → update that sub's `last_sent`.
     - `Err(Full)` → the plugin is **alive but behind** (drive loop parked in a guest call);
       **drop this *sample*, keep the `Sub`**. (Depth-1 channel, drop-newest — a gauge wants
       the freshest value or nothing, never a stale queue.)
     - `Err(Closed/Disconnected)` → the plugin is **gone** (its `feed_rx` dropped on
       `task.abort()`); **drop the `Sub`**.
     - A sub is only sent to if `now - last_sent >= min_period` (per-subscriber throttle
       honoring the WIT `min-period-ms`; the sample is computed once regardless of count).
  3. **Atomic teardown:** still holding that same lock, if `subs` is now empty,
     `feeds.remove(kind)` and `return` (the task exits). Because the emptiness test and the
     `remove` share one lock acquisition — and `subscribe_feed` inserts-or-spawns under the
     *same* lock — there is **no window** where a new subscribe lands in a hub the task then
     deletes: subscribe-wins ⇒ hub non-empty ⇒ task lives; task-wins ⇒ hub gone ⇒ subscribe
     spawns fresh. (`min-period-ms` is clamped to `>= BASE` — can't deliver faster than `BASE`.)

### 4.3 Delivery into the drive loop
Mirrors RFC 0011's `timer_request`. `Host` gains `feed_requests: Vec<(FeedKind, u32)>`
(a guest may subscribe to several in one call); `feed-subscribe` pushes onto it. The drive
loop owns a `(feed_tx, feed_rx)` mpsc pair (depth 1) and a stable `token` (the plugin's
`instance` id), and after **every** guest call drains `store.data_mut().feed_requests` — for
each **granted** kind it calls
`reactor.subscribe_feed(kind, token, min_period, feed_tx.clone())`. `subscribe_feed` is an
**upsert keyed by `token`**: if a `Sub` with this `token` already exists in the hub, it
updates `min_period` and returns; it never adds a duplicate. So a plugin that re-asserts
`feed_subscribe(Cpu, …)` on every tick (a common, reasonable habit) gets exactly one `Sub`,
not one per tick. The `select!` gains a third arm, ordered **after `input` and before the
timer** (so a feed at ≤1/s can never delay a click; `biased`):

```rust
sample = feed_rx.recv() => Some(Event::Feed(sample)),   // after input.recv(), before sleep_for(timer)
```

A feed sample becomes `Event::Feed(FeedSample { feed, value })` → `step` (same WALL/epoch
backstop as every other guest call). It is `Some(...)`, so it does **not** consume the
RFC-0011 timer one-shot (only the `None`/timer arm does). No coalescing — feeds are ≤ 1/s.
The pointer path, the timer cadence (RFC 0011), `carry`, and teardown (`task.abort()` drops
`feed_tx` → the hub sees `Closed` and drops the `Sub` on its next tick) are untouched.

### 4.4 The guest SDKs gain `feed_subscribe` (both, one contract)
The host plumbing is useless until a plugin can *call* it — and neither `Ctx` exposes it
today (the low-level binding `p::host::feed_subscribe` / `host.FeedSubscribe` exists but is
unreachable). Add it to **both** SDKs with the **same** canonical docstring (the RFC-0011
`set_timeout` treatment):

```rust
// Rust: crates/ezbar-plugin-wasm/src/lib.rs (Ctx trait) + glue.rs (HostCtx forward)
/// Subscribe to a host-sampled system [`Feed`]; the host then delivers
/// `Event::Feed { feed, value }` no faster than `min_period_ms` (clamped to ≥ 1 s).
///
/// **Capability-gated:** only feeds granted in `[modules.<id>].feeds` are delivered.
/// **Fire-and-forget:** this returns nothing and gives **no delivery guarantee** — an
/// ungranted feed, or a deferred one (`ping` has no target in v1), is silently never
/// delivered (the host logs which module+kind). Do not busy-wait on a feed that may never
/// arrive; just render whatever samples you do get. Re-subscribing is idempotent (it only
/// updates the period), so calling it once on your first `Event::Timer` is the norm.
fn feed_subscribe(&mut self, feed: Feed, min_period_ms: u32);
```
`glue.rs` forwards to `p::host::feed_subscribe(feed_to_wit(feed), min_period_ms)`; the Go
`Ctx` gains `FeedSubscribe(feed Feed, minPeriodMs uint32)` forwarding to `host.FeedSubscribe`
with the identical contract in its docstring. The asymmetry vs `http_get` (which *does*
return `Err("capability denied…")`) is called out in the docstring, since the frozen
`feed-subscribe` WIT signature has no result and can't.

### 4.5 Dogfood
A small `sysgraph` example plugin (Rust): `feed_subscribe(Feed::Cpu, 1000)` on the first
`Event::Timer`, keep a ring buffer of the last N values, draw a `graph` node. Proves a
*metric* widget can be a sandboxed plugin — the thing RFC 0007 said was impossible. (The
built-in `cpu` stays; this is the proof, not a replacement.) Ships with a one-line config
note that it needs `feeds = ["cpu"]` granted.

## 5. Perf & correctness
- **One sample per kind**, not per subscriber: ten cpu-graph plugins = one `/proc/stat` read
  per second, fanned out. The sampler is on the blocking pool, off the reactor workers.
- **Idle = zero**: no subscribers to a kind ⇒ no sampler task. Last unsubscribe (drive task
  gone) tears the sampler down.
- **Sandboxed**: the plugin never reads `/proc`; it receives one `f64` for a kind the user
  granted. Ungranted kinds deliver nothing. No new filesystem/network reach.
- **Bounded**: feed `step`s ride the existing WALL/epoch limits; `BASE` caps delivery rate;
  the channel is bounded (a slow plugin drops samples rather than growing a queue — a graph
  missing a sample is invisible, unlike a lost click).

## 6. Frozen-ABI limits (decisions to confirm in review)
- **net is total-only.** `feed-sample.value` is one `f64`, so the net feed reports **down+up
  bytes/s**, not split. A plugin that needs separate directions still can't from this feed.
  Option: ship net as total now, or omit net from v1 and revisit when a v0.2.0 WIT can carry
  a richer sample. (Leaning: ship total — a throughput sparkline reads fine as one line.)
- **ping is deferred** — no target in the parameterless enum (§2).
- **battery** is a %; charging/discharging state isn't in the feed (one `f64`). A battery
  *graph* is odd anyway; included for completeness, may be dropped if it reads as noise.

## 7. Open questions — resolved in review
1. **Injection API → global `set_feed_sampler` (OnceLock).** Both: fine, not new magic — the
   reactor is already a `static OnceLock` singleton; threading per-plugin state for a
   process-global resource is *worse*. Plus the unset-at-subscribe guard (§3).
2. **Cadence → fixed `BASE = 1 s` + per-subscriber throttle.** Both: correct — sharing one
   sample is the point; running per-min-period defeats it. 1 s is the right floor for a 48 px
   sparkline (the `view` is a static snapshot; same reasoning as RFC 0011's `MIN_TIMER_MS`).
3. **net in v1 → ship total.** Both: the frozen single-`f64` `feed-sample` makes net
   total-only, and that does **not** corner ABI evolution — a v0.2.0 WIT (copy+edit, never an
   edit to the frozen dir, RFC 0006 §4) can add `net-down`/`net-up` variants additively while
   the total feed keeps working in-window. A throughput sparkline reads fine as one line.
4. **Teardown race → atomic single-critical-section remove.** Folded into §4.2 (no generation
   counter; emptiness-test + `remove` + subscribe-insert all under one `feeds` lock; sample
   outside it).
5. **Channel depth → depth-1, drop-newest via `try_send`.** Folded into §4.2 — a gauge wants
   the freshest sample or nothing, and `Full` (alive-but-behind) must keep the `Sub`, only
   `Closed` drops it.
