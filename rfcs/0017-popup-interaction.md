# RFC 0017: one popup-interaction model — a pure, tested controller

- **Status:** Draft (v2 — review folded in: Torvalds, who traced the §4 machine + the real code).
- **Created:** 2026-06-06
- **Target:** ezbar (the bar host `src/main.rs`; the `Module` trait; the self-wiring modules
  `stock`/`calendar`/`claude`/`clock`/`kubectl`/`github`/`wasm`; the `ezbar-harness` adapter).
- **Depends on / supersedes the ad-hoc bits of:** RFC 0001 (modules + popups), RFC 0009
  (pointer events), RFC 0010 (host-side popup motion). RFC 0016 (clock) is the first consumer.

## What changed in v2 (review fold-ins)

Torvalds ACK'd the diagnosis and invariants (he verified P1's events-transparency, the
`OutsidePressed` capture semantics, and the two-phase machine against the framework source), but
caught five places that claimed correctness the code/framework don't give:

1. **Multi-output anchoring was wrong (Blocking).** `operate` cannot be scoped to one window
   (`iced_layershell` runs it across all surfaces with no window id), and `PillBounds` keeps the
   *last-visited* surface's bounds; a chip's local `center_x` differs on a 1920 vs a 3840 output,
   so the popup anchored off the wrong output. **Fix:** the tag is **output-scoped** —
   `pill_id(instance, output)` — and `bar_view` tags per its surface's output; `QueryBounds`
   targets the trigger event's output (§7). v1's "capture any, x is identical" is deleted.
2. **`HostRequest::ClosePopup` cannot be removed (Blocking).** `kubectl` (`Msg::Select`) and
   `github` (`Msg::MarkAll`) already close their own popup after acting. The module→host **close**
   path stays; only `OpenPopup` (the *trigger*) moves to declared intent. §10.5 (selection-closes)
   is therefore not separable — it's the same mechanism, kept (§6).
3. **Opening has module side effects (Blocking).** `clock` resets `shown_month` on click-open;
   `kubectl` kicks a context fetch on open. A host that opens directly from `popup_trigger()`
   without telling the module would lose these. **Fix:** explicit `Module::popup_opening()` /
   `popup_closed()` lifecycle hooks the host calls when the controller opens/closes (§6).
4. **Fold the switcher in now (Blocking).** v1 left switcher/volume on `self.popup` — a *second*
   popup owner, so "at most one popup" only held inside the controller and the two owners
   cross-closed each other (exactly the multi-owner split that bred these bugs). v2 models them as
   `Click` popups with a synthetic non-module instance; `self.popup` is deleted (§4/§5).
5. **Migration was undercounted ~4× (Blocking).** Seven modules drive popups today, **plus** the
   `ezbar-harness` reimplements the request adapter. v2 enumerates all of them and moves the
   harness in lockstep (§6).

Plus: the §4 table is now **total** (explicit no-ops for `None × {BoundsResolved, SurfaceClosed,
OutsidePressed, OutputsChanged}` and `SurfaceClosed{id≠open.id}` — the design's own
dismiss-before-bounds and self-close-echo arguments depend on these); the dead `Open.placed`
field is removed (the two-phase guard is `open == None`, nothing else); P4 is corrected (the tag
and the full-height hit cell **merge** in one node — the host only reads `center_x`, invariant to
height; the top-edge bug was collapsing to `Shrink`, not co-location); and the calendar
dead-scroll consequence of P1 is named (§3/§10).

## 1. Problem

Popup interaction has been hand-wired per feature, and we've paid for it. In one session we
shipped and then fixed, in order: a brittle close-on-leave **grace timer**; a **flicker loop**;
a **stale popup anchor**; a **dead `stock` hover**; a **dead top-edge hit area**. Five "bugs",
one cause:

> Popup interaction is hand-wired per feature across **two Wayland surfaces with no shared
> model**, and the decision logic is **tangled with untestable I/O** — so every new case
> re-derives the rules and breaks an old one.

Each bug is a missing invariant, not an accident:

| bug | invariant we violated |
|---|---|
| grace timer | mixed *leave-to-close* with *interactive content* |
| flicker loop | widget-tree **shape depended on popup state** |
| stale anchor | anchored off the **event-sourced cursor**, not geometry |
| dead `stock` hover | host logic assumed **one** trigger mechanism (whole-pill) |
| dead top-edge | **conflated** the anchor-tag with the hit-target |

The kicker (RFC 0009-style): **a human is the only integration test.** Synthetic pointer events
don't reach layer-shell surfaces, so none of these were caught by code — each shipped, then a
person reported it. Any model that leaves the decision logic entangled with Wayland will keep
failing that way.

## 2. Goal & non-goals

**Goal.** One model for *every* bar popup, with the decision logic as a **pure state machine**
that is unit-tested without a compositor; Wayland/iced confined to thin adapters at the edges.
Modules declare *intent*; the host owns wrapping, hit-area, anchoring, and lifecycle, in one
place.

**Non-goals.** Popup *content* (modules still own their `popup()` element). Motion (RFC 0010,
host-side, unchanged). The hardcoded switcher/volume popups keep their own path for now (they're
not module popups), though they should fold in later (§9).

## 3. Principles (the invariants)

**P1 — Two modes, never mixed.**
- **Hover** popup = display-only, **events-transparent**, closes on **pill-leave**.
- **Click** popup = interactive menu, closes on **dismiss** (click-outside / re-click / another
  opens). Does **not** track hover.

This is the load-bearing invariant. The grace timer existed *only* because we made an
interactive popup (the kube picker) close on leave — the forbidden mix. The two halves are
mutually exclusive and each dissolves the gap problem:
- A hover popup is **events-transparent** (it already is — `popup_settings(.., events_transparent
  = matches!(mode, Hover))`), so the pointer is *never* "over" it; the surface gap between pill
  and popup is irrelevant because you never cross into it. Close = pill-leave. **No region
  tracking, no timer.**
- An interactive popup is **click-mode**, which doesn't watch hover at all, so leave/gap simply
  don't enter the logic.

We do not *manage* the gap; we make it *irrelevant*. (Decided: enforce, no hover+interactive
escape hatch.) **Consequence, named honestly:** `calendar` today wraps its agenda in a
`scrollable` yet requests `PopupMode::Hover` → events-transparent → the scrollbar already gets
no input (dead scroll, *today*). P1 makes that correct-by-construction: either the calendar
accepts a size-to-fit, non-scrolling preview (it's a hover *glance*), or it becomes a `Click`
popup (a UX change from preview to sticky). §10 carries the choice; the invariant does not bend.

**P2 — Tree shape is invariant to popup state.** Never add/remove a wrapper based on whether a
popup is open (that reset hover → flicker). Host wrappers are unconditional and constant.

**P3 — Anchor to geometry, never the cursor; one path for every module.** Popup x comes from the
triggering **chip's laid-out bounds** (ground truth, deterministic, RFC 0016-confirmed `operate`
mechanism) — for *all* modules. No cursor anchoring anywhere → no staleness, consistent
placement, no `stock` special-case.

**P4 — Three concerns, defined once each, never *collapsed*:** **trigger** (hover vs click — who
opens), **hit-target** (full-height cell, Fitts's law — where the pointer activates), **anchor-
tag** (the chip's bounds — where the popup points). They may share one widget node — the host
reads only `center_x`, which is invariant to the node's height — so the tag and the full-height
hit cell **merge cleanly**: `container(widgets).id(pill_id(inst,out)).height(Fill)` (width
`Shrink`). The top-edge bug was not co-location; it was letting that container default to
`height(Shrink)` and collapse the hit area. The rule is "never collapse the hit height," not
"separate the nodes."

**P5 — One pure controller; Wayland/iced only at the edges.** Because a human is the only
integration test, the decision logic must be unit-testable without one.

## 4. The controller (pure state machine)

A single type, **no iced/Wayland types inside it** (positions are plain `f32`, ids are opaque
newtypes the host maps to `window::Id`/`widget::Id`). The host translates raw iced events →
`In`, and `Out` → iced tasks. It is the **only** popup-state owner — the switcher/volume popups
are modelled here too, as `Click` popups under a synthetic `InstanceId` (so `self.popup` is
deleted; one owner, one proof).

```rust
enum Mode { Hover, Click }

struct Open {
    id: PopupId,            // allocated monotonically at open; NEVER reused (stale-guards depend on it)
    instance: InstanceId,   // a real module, or a synthetic id for switcher/volume
    mode: Mode,
    output: OutputId,
}                           // NB: no `placed` flag — the two-phase guard is purely `open == None`

struct PopupController {
    open: Option<Open>,
    next_id: u64,           // monotonic PopupId source
}

enum In {
    HoverEnter { instance, output },   // pointer entered a hover-intent chip
    HoverLeave { instance },           // pointer left a hover-intent chip
    Click      { instance, output },   // a click-intent chip (or switcher) was pressed
    OutsidePressed,                    // a press that did NOT land on any chip (bar bg / scrim)
    BoundsResolved { id: PopupId, anchor_x: Option<f32> },
    SurfaceClosed { id: PopupId },     // compositor/host closed it out from under us
    OutputsChanged,
}

enum Out {
    QueryBounds { id: PopupId, instance: InstanceId, output: OutputId }, // host runs `operate`
    CreateSurface { id: PopupId, mode: Mode, anchor_x: Option<f32> },
    CloseSurface { id: PopupId },
}
```

`update(&mut self, In) -> Vec<Out>`. The transition table is **total** — every (state, input)
has a row, because the correctness arguments depend on the no-ops as much as the actions:

| state.open | input | action |
|---|---|---|
| `None` | `HoverEnter{i,o}` | `open = {fresh id, i, Hover, o}`; → `QueryBounds` |
| `Some(p)`, `p.instance==i` | `HoverEnter{i,_}` | nothing *(already this popup)* |
| `Some(p)` (other) | `HoverEnter{i,o}` | `CloseSurface(p.id)`; `open={fresh id,i,Hover,o}`; → `QueryBounds` |
| `Some(p)`, `p.instance==i`, `p.mode==Hover` | `HoverLeave{i}` | `CloseSurface(p.id)`; `open=None` |
| `None` **or** `Some(p)` where `p.instance≠i` or `p.mode≠Hover` | `HoverLeave{i}` | **nothing** *(stale-leave guard)* |
| `Some(p)`, `p.instance==i`, `p.mode==Click` | `Click{i,_}` | toggle off: `CloseSurface(p.id)`; `open=None` |
| `Some(p)` (other) | `Click{i,o}` | `CloseSurface(p.id)`; `open={fresh id,i,Click,o}`; → `QueryBounds` |
| `None` | `Click{i,o}` | `open={fresh id,i,Click,o}`; → `QueryBounds` |
| `Some(p)` | `OutsidePressed` | `CloseSurface(p.id)`; `open=None` *(both modes; for Hover, pill-leave usually beat it)* |
| `None` | `OutsidePressed` | **nothing** |
| `Some(p)`, `p.id==id` | `BoundsResolved{id,a}` | → `CreateSurface{id,p.mode,a}` |
| `Some(p)`, `p.id≠id` **or** `None` | `BoundsResolved{id,_}` | **nothing** *(stale — suppresses orphan surface after dismiss-before-bounds)* |
| `Some(p)`, `p.id==id` | `SurfaceClosed{id}` | `open=None` |
| `Some(p)`, `p.id≠id` **or** `None` | `SurfaceClosed{id}` | **nothing** *(self-close echo of an already-replaced surface)* |
| `Some(p)` | `OutputsChanged` | `CloseSurface(p.id)`; `open=None` |
| `None` | `OutputsChanged` | **nothing** |

Why the no-op rows are load-bearing:
- **dismiss-before-bounds:** `HoverEnter` → (dismiss) → `open=None`, then the late
  `BoundsResolved{id}` hits the `None`/`p.id≠id` row → **nothing** → no orphan `CreateSurface`.
- **self-close echo:** opening B closes A's surface; A's `SurfaceClosed{idA}` then arrives while
  `open==B` → `p.id≠id` row → **nothing** → B survives. (Traced clean by review.)
- **stale-leave:** a `HoverLeave` from a module that isn't the open one can't close the current
  popup (adjacent-pill `exit(A)/enter(B)` converges to `open=B` in either arrival order).

**PopupId is monotonic and never reused** — every stale-id guard above relies on it.
`CloseSurface(id)` on an `AwaitingBounds` (not-yet-created) surface is harmless in layer-shell.

That's the whole brain: one `Option<Open>`, ~30 lines, **zero** I/O — every row is a unit test.

## 5. The host adapter (the only place Wayland lives)

- **One chip chokepoint.** Each module's chip is wrapped *once*, per-module (not per-group), in a
  single node that is **both** the full-height hit cell **and** the anchor-tag (P4 — they merge):
  `container(chip).id(pill_id(instance, output)).height(Fill)` (width `Shrink`), inside a
  `mouse_area` emitting `HoverEnter/Leave` or `Click` for that instance+output. The `id` is
  **output-scoped** (Blocking 1). Grouping, islands, and separators (RFC 0005) layer on top and
  never touch interaction. This replaces the group-level, single-module-only `with_pill_hover`.
  *Nested-`mouse_area` caveat:* a WASM chip with an inner clickable `mouse_area` (its own
  `LNode::MouseArea`) under a whole-pill **Click** trigger — the inner `on_press` captures the
  event, so the host's click-to-open won't fire on the inner region (use a Hover trigger, or the
  inner control *is* the popup interaction). `on_enter/on_exit` don't capture, so Hover triggers
  compose fine.
- **`OutsidePressed`:** the unconditional bar-background `mouse_area.on_press` (already present as
  `arm_dismiss`, kept constant per P2).
- **`QueryBounds{id,instance,output}` → `BoundsResolved`:** the `PillBounds` `operate` task
  (RFC 0016) keyed by the **output-scoped** `pill_id(instance, output)`, returning
  `Option<Rectangle>`; `anchor_x = bounds.map(center_x)`. `None` only if the chip vanished mid-
  flight → controller drops the open (no fallback-to-cursor; P3 forbids the cursor entirely).
- **`CreateSurface` → `NewLayerShell`** with `events_transparent = matches!(mode, Hover)` (P1),
  output = `open.output`.
- **`CloseSurface` → `window::close`; `SurfaceClosed`** from the window-closed event;
  **`OutputsChanged`** from the existing outputs subscription.

The host holds the `PopupController` plus the `PopupId → window::Id` / `InstanceId` maps. `view`
renders `open`'s module `popup()` (or the switcher/volume content for a synthetic instance) for
the matching surface, as today. **`self.popup` and `self.module_popup` collapse into the
controller's single `open`** — the two-owner split is gone.

## 6. Module intent + lifecycle (kill the second *trigger* mechanism; keep *close*)

Modules stop hand-rolling popup **triggers**, but the **open/close lifecycle and the
programmatic close stay** — Torvalds caught that both are load-bearing today.

- **Intent, not wiring:** `popup_trigger(&self) -> Option<Trigger>` where `Trigger ∈ {Hover,
  Click}` (folds the existing `hover_messages`/`click_message` into one intent enum; the host
  supplies the events). A module's own `mouse_area` is henceforth for **non-popup** input only
  (scroll, an inline button). It MUST NOT *trigger* a popup.
- **Lifecycle hooks (Blocking 3):** opening is not side-effect-free — `clock` resets `shown_month`
  on click-open (`clock.rs`), `kubectl` kicks a `get_all_contexts` fetch on open (`kubectl.rs`).
  The host calls `Module::popup_opening(&mut self) -> Response` and `Module::popup_closed(&mut
  self)` when the controller opens/closes, so those side effects (and any async kick) still run.
  These replace the `update → HostRequest::OpenPopup` round-trip the modules used to (ab)use to
  hang on-open work.
- **Programmatic close stays (Blocking 2):** `kubectl` (`Msg::Select`) and `github`
  (`Msg::MarkAll`) close their own popup after acting. `HostRequest::ClosePopup` is **retained**;
  the host turns it into an `In::OutsidePressed`-equivalent close for that instance. (This *is*
  §10.5 "selection-closes-popup" — same mechanism, not a separate feature.) Only
  `HostRequest::OpenPopup` is removed (the trigger moved to intent).

**Full migration set (Blocking 5)** — every popup-driving site moves in one change:
`stock`, `calendar`, `claude`, `clock`, `kubectl`, `github`, and the `wasm` reactor
(`crates/ezbar-wasm`, which maps its guests' hover/click to `OpenPopup` today) → declared
`popup_trigger` + `popup_opening`/`popup_closed`. **The `ezbar-harness` adapter
(`crates/ezbar-harness/src/lib.rs`, `apply_request`) and `examples/counter.rs` move in lockstep**
— the harness is where modules get tested, so it must speak the new contract or the dev surface
diverges from the bar. Because §5 wraps per-module (not per single-module group), `stock` works
*inside* its `["stock","volume","battery"]` group with no regrouping — exactly the case that was
dead.

## 7. Multi-output (corrected — Blocking 1)

A chip is laid out on **every** bar surface, and `iced_layershell` runs a widget operation across
**all** surfaces with no window-id scoping — `PillBounds` would keep whichever surface is visited
last. On heterogeneous outputs (1920 vs 3840 wide, or fractional scale) a right-cluster chip's
local `center_x` differs per output, so anchoring off "any" surface lands on the wrong one. v1's
"x is identical" was wrong.

**Fix:** the tag is **output-scoped** — `pill_id(instance, output)` — so each surface tags its
chip with a distinct id; `QueryBounds` carries the **trigger event's** `output` and matches only
that surface's chip. The **output** itself comes from the trigger event (`HoverEnter/Click` carry
the surface the pointer is on), not a cursor cache — so output selection is event-correct too,
closing the residual `cursor_output` staleness from the prior anchor fix. `bar_view` is
parameterized by its surface's output (it already knows it — `view(id)` resolves the bar
surface) and tags accordingly.

## 8. Testing

`PopupController::update` is pure → the entire §4 table is unit tests: open-on-enter,
close-on-leave, stale-leave-noop, click-toggle, outside-dismiss, two-phase open + dismiss-before-
bounds (assert no orphan `CreateSurface`), bounds-for-stale-id ignored, surface-closed,
outputs-changed-closes. Plus property checks: *at most one `open` ever*; *every `CreateSurface(id)`
is preceded by an `open` with that id and eventually followed by exactly one `CloseSurface(id)`*
(no leaks). None of this needs a compositor — the part a human can't re-verify each time is now
the part the machine does.

## 9. What this fixes, by construction

- grace timer → **gone** (P1: interactive ⇒ click ⇒ no leave-close; hover ⇒ transparent ⇒ no gap).
- flicker → **gone** (P2: constant tree).
- stale anchor → **gone** (P3: geometry, uniform).
- dead `stock` → **gone** (§5/§6: per-module wrap + intent, works in any group).
- dead top-edge → **gone** (P4: never collapse the hit-cell height — merged tag keeps `Fill`).
- multi-output mis-anchor → **gone** (§7: output-scoped tag).
- two popup owners → **gone** (§4/§5: switcher folded in; one `open`).
- the bug class (a human finds it) → **shrunk**: the decision logic is now machine-verified.

## 10. Open questions (for review)

1. **`calendar` under P1** (forced by the invariant — its `scrollable` popup is already dead under
   events-transparency): keep it a **Hover** glance with a size-to-fit, non-scrolling `popup()`,
   or promote to a **Click** sticky so the agenda scrolls? *Rec:* Hover glance — it's a preview.
2. **Islands Fitts, admitted (Torvalds nit):** inside a shared island the screen-edge target comes
   from the *group* float cell, not per-module, so an inner module gets a full-height *trigger*
   (fixes dead-stock) but not a screen-edge hit area (it never had one). Accept, or split shared
   islands into per-module floating pills (breaks the island look)? *Rec:* accept.
3. **Scrim for true click-outside-anywhere.** `OutsidePressed` fires only on the *bar* background,
   not other apps/desktop. A full-screen transparent scrim behind a `Click` popup gives real
   click-anywhere dismiss. In or out of v1? *Rec:* out of v1 (bar-bg + re-click + another-open
   cover the common path).
4. **`HostRequest::OpenPopup` removal timing** — rip out now (trigger fully replaced by
   `popup_trigger`) or keep one release as a deprecated no-op for out-of-tree plugins?
   (`ClosePopup` stays regardless — §6, Blocking 2.)
5. **Lifecycle hook shape (§6)** — explicit `popup_opening() -> Response` + `popup_closed()`, vs.
   reusing the trigger `ModMsg` through `update` with the host ignoring any popup `HostRequest` in
   the response (less new API, keeps the round-trip we're killing). *Rec:* explicit hooks.
