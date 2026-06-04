# RFC 0011: event-driven cadence — honor `set-timeout`, kill the 2 s blind poll

- **Status:** **Implemented** (v2) — shipped in the reactor + both guest SDKs + the three
  Rust example plugins. ACK'd by a systems reviewer (Torvalds) and a plugin-platform/API
  reviewer at design time *and* on the implementation (both traced the state machine, fold
  ordering, and the three plugin migrations — no defects).
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / wasmtime reactor + guest SDKs)
- **Depends on:** RFC 0006 (WASM plugins, the frozen `host.set-timeout` WIT), RFC 0008
  (the reactor + its per-plugin drive loop). No WIT change — the function is already
  shipped and frozen.

## What changed in v2 (review fold-ins)

Both reviewers ACK'd the core mechanism (state machine, absolute-deadline `sleep_until`,
single-fiber fold) and converged on four required fixes, all folded in:

1. **The weather migration as drafted dropped the backoff — fixed.** Weather has *two*
   cadences (15 min on good data, 2 min retry on error/429). A single `set_timeout(15min)`
   both loses the retry *and* (Torvalds) would strand weather on `Idle` after one fire,
   because weather only re-armed on the rare fetch path. The fix re-arms **unconditionally
   on every `Event::Timer`** with the branch-appropriate delay (`REFRESH_MS` / `RETRY_MS`)
   and deletes the `cooldown` tick-counter entirely (§4.4).
2. **btc/quakes are the real offenders — migrate them too.** They have *no* throttle and
   `http_get` Coinbase/USGS on **every** 2 s tick. Left on `Heartbeat` that's unchanged
   (no regression) but irresponsible; both are migrated to a sane cadence + error backoff
   (§4.4).
3. **Immediate post-init `Event::Timer` (resolves Open Q2 = yes).** A plugin can only arm
   from inside `update`, and the first `update` was ≤2 s away via `Heartbeat` → a 2 s blank
   chip on every load. The host now delivers **one bootstrap `Event::Timer` right after
   `init`** (SDK-agnostic, rides the existing variant). It is an *explicit* tick, not a
   one-shot fire, so a legacy plugin stays on `Heartbeat` (§4.2).
4. **Pin the one-shot / `0`=cancel / floor contract — in the RFC + both SDK docstrings,
   not the WIT.** The WIT dir is hard-frozen ("never edit a shipped `since-vX`"), and a doc
   comment, while ABI-safe, would still touch it; so this RFC is the canonical contract and
   the Rust + Go `Ctx::set_timeout` docstrings are made identical and loud about one-shot,
   `0`=cancel, and the 100 ms floor (§4.3). Considered-and-rejected: a host `log::debug` on
   the `At→Idle` (fired-not-rearmed) transition — it would misfire on *intentionally*
   reactive plugins (that's a valid state, not an error), so the loud docstring carries it
   instead.

## 1. Problem

The reactor drives **every** plugin off a hardcoded 2 s timer
(`POLL = Duration::from_secs(2)`, `crates/ezbar-wasm/src/lib.rs`): the drive loop wakes
each plugin's fiber every 2 s, calls `update(Event::Timer)`, and re-renders if it went
dirty. This is the **one un-Zellij thing left** in the reactor — a blind, uniform poll
where Zellij is fully event-driven.

It is wrong in both directions:

- **Too fast for slow data.** The `weather` plugin wants a ~15-min cadence; it gets a 2 s
  tick and *counts ticks to throw most of them away* (`// the host ticks us ~every 2s …
  cooldown counts ticks left before the next fetch`). 450 wakes to do 1 fetch.
- **Too slow / wrong for everything else, and impossible to opt out.** A plugin that is
  purely reactive (only redraws on a click or a feed sample) still eats a wake every 2 s
  forever. There is no way to say "don't wake me."

The kicker: **the fix is already designed and half-shipped.** `host.set-timeout(ms)` is in
the frozen v0.1.0 WIT (RFC 0006). The **Go SDK already exposes it** —
`Ctx.SetTimeout(ms)`, documented as *"asks the host to deliver the next EvTimer after ms
milliseconds"* — and the Go `clock` (`ctx.SetTimeout(10_000)`) and `loadgauge`
(`ctx.SetTimeout(tickMS)`) examples **already call it**. The host just **ignores it**:

```rust
async fn set_timeout(&mut self, _ms: u32) {}   // ← no-op
```

So today those Go plugins are silently downgraded to the 2 s poll. This RFC makes the host
honor the call. No new ABI — we light up a frozen one.

## 2. Goal & non-goals

**Goal.** A plugin controls its own wake cadence. `set-timeout(ms)` delivers exactly one
`Event::Timer` after `ms`; the plugin re-arms each tick to keep polling. A reactive plugin
that never arms (or cancels) costs **zero** timer wakes — only the events it actually wants.

**Non-goals / explicitly deferred:**
- `host.subscribe(kinds)` (event-kind filtering) — also frozen-but-no-op. Orthogonal to
  cadence (pointer events flow regardless; `set-timeout` arms the timer directly). It pairs
  with the **feeds capability** work (the next P1 TODO), not this one. Stays a no-op.
- The `feed-subscribe` host timer fan-out — same: feeds capability RFC.
- Any WIT change. The function is frozen; we only change host behavior + add the missing
  Rust-SDK `Ctx` method (the Go SDK already has it).

## 3. Semantics — one-shot timer with a legacy heartbeat fallback

`set-timeout(ms)`:
- `ms > 0` → deliver **one** `Event::Timer` after `ms` ms, then stop (one-shot). Re-arm in
  `update` to keep a cadence. This is the Zellij / Go-SDK contract verbatim.
- `ms == 0` → **cancel**: no timer until the plugin arms one again. The opt-out for a
  purely reactive plugin (cost zero).

**The compat problem and its resolution.** Every *existing* plugin (weather/btc/quakes, and
any Rust plugin — the Rust `Ctx` can't even call `set-timeout` yet) depends on the 2 s
heartbeat to bootstrap and to poll. If the host went purely event-driven, they'd all freeze
after `init`. So the drive loop keeps a small per-plugin state machine:

```
enum Timer { Heartbeat, At(Instant), Idle }
```

- **`Heartbeat`** (initial): the legacy 2 s auto-renewing poll. A plugin that *never* calls
  `set-timeout` stays here forever — byte-for-byte today's behavior. Zero regressions.
- The **first** `set-timeout(ms>0)` moves it to **`At(now+ms)`** — the plugin has taken
  control. The timer is **one-shot**: when it fires, the state drops to `Idle`; the plugin
  re-arms in `update` for the next tick (back to `At`).
- `set-timeout(0)` moves it to **`Idle`** — no timer; the loop parks on input only.

So the latch is implicit and needs no flag: once a plugin issues *any* `set-timeout`, the
state leaves `Heartbeat` and never returns to it — from then on the host arms exactly what
the plugin asks for and nothing more. A plugin that wants a steady 10 s poll re-arms every
tick (`At` → fires → `Idle` → re-arm → `At`); the moment it stops re-arming, it goes quiet.

**Folding rule (after every guest call — `init` and each `update`):** read the request the
guest issued during that call and fold it into the state:
- `After(d)` → `At(now + d)`   (now read *after* the call, so the delay starts when asked)
- `Cancel`  → `Idle`
- *(no call)* → **leave the state unchanged.** Critical for two cases: (a) a `Heartbeat`
  plugin keeps its heartbeat; (b) a pointer-driven `update` that doesn't re-arm must **not**
  drop an already-pending `At` deadline.

**One-shot consumption** happens when the timer *fires* (the loop takes the timer branch),
not in the fold: on fire, `At(_) → Idle` before running `update`; the plugin's `update` then
typically re-arms back to `At`. `Heartbeat` stays `Heartbeat` on fire (auto-renew).

### 3.1 Floor (footgun guard)
`set-timeout(1)` would wake the guest 1000×/s — a self-inflicted busy loop of
update→view→lift→render. Clamp nonzero requests to a floor:
`MIN_TIMER_MS = 100` (≤10 Hz self-wake). A status bar never needs faster (the clock wants
1000 ms; motion is host-side per RFC 0010, not plugin-driven). `0` is exempt (it's cancel,
not a fast timer). No upper clamp — `set-timeout(u32::MAX)` ≈ 49 days ≈ "never," which is
just `Idle` by another name and `tokio::time::sleep` handles it.

## 4. Implementation

All in `crates/ezbar-wasm/src/lib.rs` (host) + two small SDK files. ~50 lines net.

### 4.1 Host store data carries the pending request
`Host` gains `timer_request: Option<TimerRequest>` where
`enum TimerRequest { After(Duration), Cancel }`. The `set-timeout` import writes it
(replacing the no-op); the drive loop drains it with `store.data_mut().timer_request.take()`
after each guest call — no `Arc`, no lock; the `Host` *is* the store data:

```rust
async fn set_timeout(&mut self, ms: u32) {
    self.timer_request = Some(match ms {
        0 => TimerRequest::Cancel,
        n => TimerRequest::After(Duration::from_millis(n.max(MIN_TIMER_MS) as u64)),
    });
}
```
(Last-write-wins within a single `update` if a guest calls it twice — fine, that's the
plugin's own choice.)

### 4.2 Drive loop arms from the state, after a bootstrap tick
Replace the hardcoded `_ = sleep(POLL) => None` with a state-driven timer future, and
deliver one immediate `Event::Timer` after `init`. The exact ordering (pin it — getting
consume-vs-fold wrong double-arms or strands the timer):

```rust
async fn sleep_for(t: Timer) {                       // Timer: Copy
    match t {
        Timer::Idle      => std::future::pending::<()>().await,   // never fires
        Timer::Heartbeat => tokio::time::sleep(POLL).await,
        Timer::At(at)    => tokio::time::sleep_until(at).await,   // tokio::time::Instant (absolute!)
    }
}
fn fold_timer(state: &mut Timer, req: Option<TimerRequest>) {
    match req {
        Some(TimerRequest::After(d)) => *state = Timer::At(Instant::now() + d), // now() AFTER the call
        Some(TimerRequest::Cancel)   => *state = Timer::Idle,
        None => {}                                                              // leave unchanged
    }
}

// after init succeeds:
let mut timer = Timer::Heartbeat;
fold_timer(&mut timer, store.data_mut().timer_request.take());   // a future load-with-ctx could arm
if !step(&mut store, &plugin, &slot, &Event::Timer).await { return Ok(()); }  // bootstrap tick
fold_timer(&mut timer, store.data_mut().timer_request.take());

loop {
    let next = /* carry, else select! { input.recv(), _ = sleep_for(timer) => None } */;
    let event = match next {
        None => { if let Timer::At(_) = timer { timer = Timer::Idle; } Event::Timer } // consume on FIRE
        Some(p) => /* …pointer / scroll-coalesce, unchanged… */,
    };
    if !step(&mut store, &plugin, &slot, &event).await { return Ok(()); }
    fold_timer(&mut timer, store.data_mut().timer_request.take());                    // fold AFTER step
    if is_pointer { sleep(MIN_INTERVAL).await; }
}
```

The one-shot is consumed **when the timer fires** (`At → Idle`, before `step`), and the
guest re-arms inside `update` → folded back to `At` *after* `step`. `Heartbeat` is **not**
consumed on fire (auto-renews). The pointer path, coalescing, `carry`, the `MIN_INTERVAL`
cadence gate, and the WALL/epoch backstops are **untouched** — `set-timeout` only governs
the Timer branch's delay. `At(Instant)` is **absolute** (`tokio::time::Instant`), so the
`select!` dropping the un-taken `sleep_for` future on every pointer event (or the `carry`
path skipping the select) loses **zero** time — the next `sleep_until` targets the same
wall instant. A relative `sleep(Duration)` would drift on every pointer event; we don't use
one.

### 4.3 Rust SDK gains the method (parity with Go) + the loud contract
`crates/ezbar-plugin-wasm/src/lib.rs` — add to the `Ctx` trait, with the **canonical**
docstring (the Go `Ctx.SetTimeout` doc is updated to match word-for-word):
```rust
/// Ask the host to deliver the next `Event::Timer` after `ms` milliseconds.
///
/// **One-shot:** this schedules exactly ONE timer. To keep a cadence, call it again
/// from each `Event::Timer` (e.g. `ctx.set_timeout(1000)` every tick for a 1 Hz clock)
/// — if you don't re-arm, the timer goes silent after firing once.
/// `set_timeout(0)` **cancels**: no timer until you arm one again (a purely reactive
/// plugin that only redraws on pointer/feed events should call this once to cost zero).
/// Values below 100 ms are floored to 100 ms.
///
/// A plugin that *never* calls this keeps a legacy ~2 s heartbeat (zero-config default).
fn set_timeout(&mut self, ms: u32);
```
`crates/ezbar-plugin-wasm/src/glue.rs` — `HostCtx` forwards to the binding:
```rust
fn set_timeout(&mut self, ms: u32) { p::host::set_timeout(ms); }
```

### 4.4 Dogfood: migrate all three Rust example plugins off the 2 s poll
- **weather** — delete `cooldown`/`REFRESH_TICKS`/`RETRY_TICKS` and the tick-counting
  guard; re-arm **on every `Event::Timer`** (so it can never strand itself on `Idle`):
  `ctx.set_timeout(if fetched { REFRESH_MS } else { RETRY_MS })` with `REFRESH_MS = 900_000`
  (15 min good-data) and `RETRY_MS = 120_000` (2 min error/429 backoff). Same two-tier
  cadence as today, expressed in real time instead of ticks — no longer coupled to `POLL`.
- **btc** — was hammering Coinbase every 2 s. Re-arm `30_000` on a good fetch, `60_000` on
  error. Pointer-driven fetches (scroll/click) stay immediate; the periodic `At` deadline
  persists across them (the fold leaves it unchanged), so they don't disturb the cadence.
- **quakes** — was hammering USGS every 2 s. Re-arm `120_000` good / `60_000` on error
  (it re-renders a `!` on error, so it re-arms on both arms of the match).

This proves the Rust path end-to-end and stops three plugins from polling public APIs at
0.5 Hz. Behavior at the endpoints is preserved (same data, same backoff intent); only the
wasted wakes go away.

## 5. Perf & correctness
- **Idle = zero wakes**, finally true: a `set-timeout(0)` plugin parks on `input.recv()`
  (no timer future armed), costing nothing until a pointer/feed event or teardown.
- **No regression for legacy plugins**: never calling `set-timeout` ⇒ `Heartbeat` ⇒ the
  exact 2 s poll of today. The change is *opt-in by calling a function that did nothing
  before*, so no shipped plugin's behavior changes unless it already calls `set-timeout`
  (today those are silently mis-served — this fixes them).
- **Lost-timer safety**: the "no call → leave unchanged" fold means a pointer event in the
  middle of a 15-min wait can't reset or drop the pending `At` deadline.
- **Deadline drift**: deadline is `now() + d` read *after* the guest call returns, so slow
  `update`s don't compound into the next interval's start. One-shot, not fixed-rate — a
  plugin that takes 50 ms to fetch still waits its full `d` afterward (no catch-up storms).
- **Footgun bounded** by the 100 ms floor + the existing per-call WALL/epoch limits.

## 6. Open questions — resolved in review
1. **Floor value → 100 ms.** Both reviewers: correct. `view` is a static snapshot and
   motion is host-side (RFC 0010), so there is no real sub-100 ms plugin-timer use case; the
   WALL/epoch limits bound per-call CPU regardless. Keep 100 ms.
2. **Bootstrap latency → yes, deliver an immediate post-init `Event::Timer`.** Both leaned
   yes; it's SDK-agnostic and subsumes cold-start for every plugin (clock paints at t≈0).
   Folded into §4.2 — explicit tick, doesn't consume a one-shot.
3. **`Heartbeat` default forever → keep it.** Both: it's the zero-config "trivial plugin
   Just Works" default and is byte-for-byte today's behavior (zero regression). The
   "calling a frozen no-op is the only opt-in" property is worth keeping; no deprecation
   path. A plugin that wants true zero-cost idle opts out explicitly with `set_timeout(0)`.
