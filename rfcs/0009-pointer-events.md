# RFC 0009: pointer events — interactive plugins (buttons, scroll, hover)

- **Status:** **Implemented** (v2) — design ACK'd (Torvalds + a wasm-plugin-host reviewer,
  each after a NAK), and the implementation ACK'd by both after a second NAK round (the
  `press`=`on_release` phantom-click and the click-drop-on-overflow, both fixed: `on_press`
  discrete-tap semantics + sender-side scroll coalescing). **Implementer checklist (done):**
  every guest call (timer + pointer) re-arms the epoch deadline and is wrapped in
  `timeout(WALL,…)` via `step()` — so the spinning-press-handler DoS stays closed.
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0006 (the widget DSL + `events` ABI), RFC 0008 (the async reactor)
- **No ABI change:** the WIT `since-v0.1.0` already models the v1 input set — this RFC
  only makes the host *deliver* it. (Drag is explicitly out — see §2.1.)

## Review response (what changed in v2)

- **Scope honesty.** v1 was overclaiming "sliders." The WIT `pointer-kind` is
  `{press, right-press, scroll, enter, leave}` — **no `move`, no `release`, no drag**, so
  a drag-slider is unbuildable. v1 is now scoped to **buttons + scroll-widgets + hover**;
  drag/scrubber-by-drag is deferred to an additive `since-v0.2.0` (§2.1), and the RFC no
  longer implies extending a *frozen* enum is free.
- **Scroll unit (the #1 fix, both reviewers).** `pointer-event.delta` is a single frozen
  `f32`, but iced delivers scroll as a `ScrollDelta::{Lines, Pixels}` enum (~50× apart on
  wheel vs touchpad). The host now **normalizes to a line-equivalent host-side** before it
  crosses the ABI (§3.2); the guest always sees lines.
- **DoS / fairness.** A bounded queue isn't a rate bound. v2 adds a **per-plugin
  call-cadence gate** that paces *all* pointer-driven guest calls (press included), with a
  yield between them, so a sub-epoch-threshold spinner can't pin a reactor worker (§3.4).
- **Ordering.** Coalesce only the **leading run of consecutive scrolls** up to the next
  non-scroll event — never reorder a click across a scroll batch (§3.4).
- **Click semantics + folds.** `press` fires on **button-down over the widget** (a discrete
  tap — iced's `on_release` can't give a true completed-click, see §3.2); the hit-id is
  length-capped on lift; clicks are protected
  from scroll-flood drop; the existing 150 ms render debounce is credited (§3.3).

## 1. Problem — the ABI models interactivity; the host throws it away

The plugin system *describes* pointer input end to end and uses none of it:

- `wit/since-v0.1.0/world.wit` — `events` has `pointer-event { id, kind, delta }`,
  `pointer-kind { press, right-press, scroll, enter, leave }`, `event` includes
  `pointer(pointer-event)`; the `ui` `mouse-area` node carries a hit `id: string`.
- The host **drops the hit id on lift** (`crates/ezbar-wasm/src/lib.rs:243`), **renders
  `mouse_area` with no handlers** (`lib.rs:924`), and the reactor only ever calls
  `update(Event::Timer)`. So a `mouse-area` + its `update` arm **never fire**.

Plugins are therefore **render-only** — fine for a clock or a graph, impossible for a
button, a scroll-to-adjust control, or a clickable list. That is the ceiling on the
"GPU widget platform, not a status bar" thesis.

## 2. Goal

Deliver `press` / `right-press` / `scroll` / `enter` / `leave` from the bar's real iced
render to the guest's `update(Event::Pointer { id, kind, delta })`, through the reactor,
under the **same** timeout / epoch / trap bounds as the timer tick — so plugins gain
**clickable controls, scroll-to-adjust widgets, and in-widget hover state** without
weakening the sandbox and without touching the frozen WIT.

### 2.1 Explicitly out of scope (v1)
**Drag / drag-sliders / press-and-hold timing / drag-to-reorder.** These need cursor
*motion while held* and a *release* edge, which `pointer-kind` does not have. Adding them
is a real, **non-free** change: a new `since-v0.2.0` WIT that *additively* extends
`pointer-kind` with `move`/`release` and extends `pointer-event` with position, with the
host's forward-adapt window for old guests (RFC 0006 §4). The `since-v*` dirs are frozen
("never edited; a new version is a copy+edit", `world.wit:5`); v1 does **not** edit
`pointer-kind`. Scroll-to-adjust covers the common "scrubber" need without drag.

## 3. Design

### 3.1 Keep (and cap) the hit id — lift
`LNode::MouseArea { child }` → `{ child, id: String }`; `lift_node` keeps `m.id`, **capped
to 64 bytes** on lift (same spirit as `MAX_NODES`/`MAX_DEPTH` — the id is guest-controlled
and the lifted arena lives outside the 2 MiB store cap, so cap it to deny a host-memory
amplification). The id is the plugin's own opaque label: stored and echoed, never parsed
or trusted.

### 3.2 Render with handlers — the bar's `build`
`LNode::MouseArea { child, id }` wraps the child in a real iced `mouse_area` with:
- **`press`** ← iced **`on_press`** (button-**down** over the widget — a discrete tap),
  **`right-press`** ← `on_right_press`. (iced's `on_release` is *not* a completed-click
  primitive — it fires on any release-while-hovering regardless of where the press began,
  inventing phantom clicks on drag-onto — so v1 uses the unambiguous down edge; true
  completed-click/drag is deferred with `move`/`release` to v0.2.0, §2.1.)
- **`scroll`** ← `on_scroll`, with the host **normalizing `ScrollDelta` to a line-equivalent
  `f32`** before it crosses the ABI: `Lines{y}` passes through; `Pixels{y}` is divided by a
  line-height constant. The guest always receives **lines** (documented unit), so a wheel
  and a touchpad drive a scrubber identically;
- **`enter` / `leave`** ← `on_enter` / `on_exit` (edge-triggered, human-rate).

Each emits `ModMsg::new(Msg::Pointer { id, kind, delta })` (`delta` = normalized scroll
lines; `0.0` otherwise) — the same `ModMsg` → `Message::ModuleMsg` round-trip the bar
already runs (`src/main.rs`), and the same `mouse_area` event API the dev harness proves
out (built-in *bar* modules use a different host-hover path; this is new wiring for the
plugin tree).

### 3.3 Route into the reactor (the new path)
A guest call may run **only** on the drive task that owns the `Store` (RFC 0008 §3.4), so
pointer events cross from the GUI thread to that task over a **bounded channel**:
- `WasmModule` holds a `tokio::sync::mpsc::Sender<PointerEvent>`.
- `WasmModule::update(Msg::Pointer{..})` → `try_send` (non-blocking; never stalls the GUI).
- `drive()`'s loop becomes a `select!` over **{ input channel, poll timer, shutdown }**.
  On a pointer batch it **re-arms the epoch deadline and wraps the call in
  `timeout(WALL, …)` — the same two lines as the timer path** (no shared deadline across
  branches), calls `update(Event::Pointer{ id, kind, delta })`, then publishes a frame
  (view/popup → lift → slot → `version` bump). `Event::Pointer` reuses the existing
  per-call bounds verbatim; nothing new is unbounded.

**Render is decoupled and debounced — there is no per-event GPU storm.** A pointer event
bumps `slot.version`; the GUI's `TickRecipe` polls that version every ~150 ms and renders
*only on change* (RFC 0008), so N input events coalesce to ≤ ~6.6 renders/s at the GPU
regardless. "One frame published per processed batch" is a slot write + version bump, not
a synchronous GUI repaint. Worst-case click-to-pixel is ~150 ms — fine for a bar.

### 3.4 Backpressure — the real bound is a cadence gate, not the queue
`press`/`right-press`/`enter`/`leave` are human-rate; **`scroll` floods**. A bounded queue
alone does **not** bound guest-CPU: a guest whose `update` spins just under the 200 ms
epoch-yield never self-traps, and back-to-back pointer calls would pin one of only two
reactor workers. So:
- **Per-plugin call-cadence gate.** At most one pointer-driven `call_update` per
  `MIN_INTERVAL` (~16 ms) per plugin, with a `sleep`/`yield` between batches so other tasks
  always interleave. This bounds pointer-driven guest cadence to a fixed rate (~62/s)
  *independent of input rate and of how slow the guest is*, and guarantees a yield point
  between pointer calls.
- **Coalesce the leading run of consecutive scrolls.** Sum the deltas of the scrolls at the
  head of the queue **up to the next non-scroll event**, deliver as one `scroll`; a press
  flushes the accumulator first. Never reorder a click across a scroll batch. (A discrete
  switcher thresholds the summed delta guest-side; a scrubber wants the sum — summing is
  lossless, so it's the default.)
- **Protect clicks on overflow.** When the channel is full a scroll merges **sender-side**
  into a single pending accumulator instead of being dropped (no scroll lost), so under a
  flood the scroll backlog collapses to ≤1 pending message and can never fill the channel
  out of a queued `press`. A `press`/`enter`/`leave` flushes the pending scroll first,
  preserving order. Channel sized ~32. (A pure *press* flood is human-rate and can't
  realistically fill it; the threat is the machine-rate scroll flood, which is defended.)

### 3.5 Popups come free; chip-hover stays host-owned
- The **popup tree is lifted+rendered by the same `build`**, so a `mouse-area` inside a
  popup routes identically — **interactive popups** (a list, a refresh button) fall out.
- **Chip-level hover stays host-driven** (`WasmModule::hover_messages` + the whole-pill
  `mouse_area` open/close the popup, RFC 0008). That is the *chip's* hover, a different
  layer from in-tree `mouse-area` hover state; they don't conflict.
- **Limitation, stated:** a guest cannot open/close its *own* popup from an in-tree click in
  v1 (no guest→`HostRequest` path; only host-hover drives the popup). A `Response`-from-guest
  capability is a separate, later surface.

### 3.6 Lifecycle semantics (define once, so authors don't each get it wrong)
`press` = a discrete tap, fired on **button-down over the widget** (not a completed click —
v1 has no `release`/motion). `enter`/`leave` are hover edges and **do not** imply or clear a
pressed state. There is **no `release` edge at all** in v1, so there is nothing to build a
press→release state machine against: model clicks as discrete `press` events and hover state
from `enter`/`leave`. The SDK doc says this in one paragraph.

### 3.7 Isolation
A pointer event tells a plugin only "your region `<id>` was clicked/scrolled," with a
host-normalized `delta`. No absolute cursor position, no cross-plugin handle, no path out
of the sandbox. A pointer-triggered `update` that traps or runs away is disabled per-`Store`
exactly like a timer-triggered one.

## 4. What does NOT change
- The WIT `since-v0.1.0` (already models the v1 set). Guest `.wasm` need no rebuild — one
  that handles `Event::Pointer` starts receiving them; one that ignores it is unaffected.
- The bounded DSL, the lift/cap pipeline, the slot, the teardown/abort lifecycle.

## 5. Open questions — resolved
1. **enter/leave routing:** route all five — edge-triggered, human-rate, paced by the gate;
   in-widget hover-highlight is a real want and doesn't collide with chip-level hover.
2. **Scroll coalescing:** sum the leading consecutive run (§3.4) — lossless; switchers
   threshold guest-side.
3. **Channel capacity / drop:** ~32 buffer; the cadence gate is the real control; protect
   clicks (coalesce scrolls to make room, never drop a queued press).
4. **press → host popup:** keep orthogonal — popup open/close stays with `hover_messages`;
   no guest-inferred popup intent in v1.

## 6. Risks
- **Scroll-flood / spinner DoS of a reactor worker** — mitigated by the per-plugin cadence
  gate (§3.4): bounded call rate + a yield between calls, independent of input/guest speed.
- **Slow click handler** — the synchronous-guest model means a `press` handler that does
  `http_get` parks the fiber for up to 8 s, during which that plugin's further pointer
  events queue then drop. **Authors must keep pointer handlers cheap and kick I/O to the
  poll path** — documented loudly in the SDK.
- **GUI-thread blocking** — `try_send` is non-blocking; a full channel coalesces/drops,
  never stalls the render.
- **Cross-thread store access** — none: the GUI only *sends*; every `call_update` stays on
  the owning drive task (RFC 0008 §3.4 invariant preserved).
