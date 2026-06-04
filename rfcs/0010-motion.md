# RFC 0010: motion — the GPU's unused payoff (workspace transition, v1)

- **Status:** **Implemented but DISABLED at runtime** (v2) — the cross-fade landed and was
  ACK'd, but live on the bar it regressed **hover**: the redraw driver was `window::frames()`,
  and in **iced_layershell** the frame-callback path corrupts `layershellev`'s pointer-seat
  tracking (`mouse hasn't entered`), so whole-pill hover (the weather forecast popup) died
  after the first workspace switch. The animation is disabled (the highlight renders discrete,
  `t ∈ {0,1}`) to keep hover correct — found only by deploying to the real layershell surface,
  which the design/impl reviews (mainline-iced reasoning) couldn't surface. **Re-enable plan:**
  drive the fade with a plain `iced::time::every(16ms)` timer (gated on `is_animating`) instead
  of `window::frames()` — a timer-driven redraw doesn't touch the frame-callback/seat path. The
  `anim` state machinery is left in place for that. (Original design notes below, unchanged.)
- **Was: Implemented** (v2) — shipped in `src/modules/workspaces.rs`. ACK'd by both
  a systems reviewer (Torvalds) and a UI/iced reviewer at design time *and* on the impl.
  Two folded cleanups landed: `.duration(Duration::from_millis(180))` set explicitly (the
  constructor otherwise ships lilt's 100 ms default), and **each pill's `Animation` stays
  independent** (no shared `t`) so the cross-fade reads as a *moving highlight*, not a
  synchronized blink — the incoming pill (fast `EaseOutCubic`) leads the outgoing one. One
  impl-review fold-in: the resting "transparent" fill is `accent @ alpha 0`, not
  `Color::TRANSPARENT` ({0,0,0,0}), so the `filled`/`underbar` fade-in stays on-hue instead
  of darkening toward black mid-fade (alpha-0 still renders nothing → resting look unchanged).
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** the `workspaces` module (`src/modules/workspaces.rs`), iced 0.14's
  `iced::animation::Animation<T>` + `iced::window::frames()`.

## Review response (what changed in v2)

- **Headline re-aimed: hover-lift → workspace active-indicator transition.** v1 proposed
  animating pill *hover*. The reviewer's product call (correct): **nobody hovers a status
  bar** — it's a glance surface, judged in screenshots/GIFs, and a hover nobody performs is
  invisible in that medium. The on-thesis effect ("eased *value* transitions") is a
  **user-triggered, screenshot-visible** one: when you switch workspaces, the active
  highlight **cross-fades** to the new workspace. Same `Animation` plumbing, but it lives
  **inside the `workspaces` module** (no Bar-level per-pill `HashMap`, so the lifecycle
  leak Torvalds flagged is gone) and it's the effect that earns the GPU tax. Hover-lift is
  demoted to a fast-follow (§6).
- **The `Interpolable` correctness bug (both reviewers).** `Animation<bool>` does **not**
  interpolate `Color`/`Shadow`/`Border` — `Interpolable` is impl'd only for `f32` (and
  `Option<f32>`). So `Animation<bool>::interpolate(0.0, 1.0, now)` yields a **scalar
  `t ∈ [0,1]`**, and the view **hand-lerps** each style field from `t`. v2 says this plainly.
- **`now` read once.** No "standard pattern" of `Instant::now()` in `view` — `Animation` is
  fully caller-driven. Read `now` **once** at the top of the module's `view` and thread that
  single value into every pill (avoids intra-frame skew).
- **Idle cost + the trailing frame.** Frames are gated on `is_animating`; an idle bar gets
  zero extra redraws. Settling costs exactly **one** trailing frame (the runtime requests
  the next frame before the subscription is dropped) — bounded, noted so the acceptance
  frame-counter expects +1, not 0.

## 1. Problem

ezbar pays ~100 MB of Mesa/LLVM to render on the GPU, then draws **static** chips. The
payoff for GPU rendering is **motion**, and there is none — a thing waybar/polybar
(CPU/cairo, damage-repaint) physically cannot do. The first motion should be the one that
shows up where the bar is judged: a **GIF of switching workspaces** where the active
highlight glides instead of snapping.

## 2. Goal & constraints

When the focused workspace changes, the active highlight **cross-fades** from the old pill
to the new (~150 ms, ease-out) instead of hard-cutting. Self-contained in the `workspaces`
module; no Bar/DSL/WIT change.

**iced 0.14 constraint:** no general `opacity` widget, no container transforms
(translate/scale). So a *positional* slide (a highlight bar physically moving) is out — v1
is a **color cross-fade** (the new pill's accent background/text/border ease in while the
old eases out), which reads as a smooth transition and needs only color interpolation. A
true positional slide is a follow-up (§6). And `Color` itself isn't `Interpolable` — we
drive a scalar `t` and lerp the rgba by hand.

## 3. Design (all inside `src/modules/workspaces.rs`)

### 3.1 State: per-workspace focus animation
The module gains `anim: HashMap<String, Animation<bool>>` keyed by workspace name —
each tracks "is this the focused workspace," bounded by the (small, ~10) workspace count.

`update` (`Msg::Update(list)`), when the focused set changes:
```text
for w in &list:
    anim.entry(w.name).or_insert_with(|| Animation::new(w.focused)).go_mut(w.focused, now)
anim.retain(|name, _| list.iter().any(|w| w.name == name))   // evict gone workspaces — no leak
```
(`Animation` has no `Default`; use `Animation::new(state).easing(EaseOutCubic)`.)

### 3.2 Subscription: frames only while animating
```text
subscription():
    batch[
        ws_sub,                                   // the existing sway event stream
        if anim.values().any(|a| a.is_animating(now)):
            window::frames().map(|_| Msg::Tick)   // drive redraws ONLY while a fade runs
    ]
```
`Msg::Tick` is a state no-op — it just forces `view` to re-interpolate. When the last fade
settles, `is_animating` is false, the frames recipe is dropped, redraws stop (+1 trailing
frame, §Review-response). Idle workspaces = no frames.

### 3.3 Render: lerp the highlight by `t`
At the **top of `view`** read `now = Instant::now()` **once**. For each pill,
`t = anim[name].interpolate(0.0, 1.0, now)` (eased focus-ness ∈ [0,1]); hand-lerp:
- **text color**: `lerp(dim_or_fg, accent, t)`
- **background**: `lerp(transparent/base, accent @ ~0.18 alpha, t)`
- **border**: `lerp(none, accent @ ~0.35 alpha, t)` — **width fixed** (animating width
  reflows/jitters every frame).

So switching workspaces fades the old pill's accent out and the new pill's in over ~150 ms.
`urgent`/`visible` states keep their current discrete styling (out of scope to animate).

### 3.4 Timing
~150 ms, `Easing::EaseOutCubic` (snappy settle; **not** lilt's `EaseInOut` default, which
has a slow start that reads as lag). A cross-fade is symmetric enough that one duration is
fine (unlike hover, which wants asymmetric in/out).

## 4. What does NOT change
- No Bar-level state, no per-pill `HashMap` in `Bar` (so no lifecycle leak), no plugin/DSL/
  WIT change. The animation is owned by the module that owns the data, exactly like
  `calendar` owns its blink timer.
- The sway event stream, the click-to-switch, the urgent/visible styling.

## 5. Perf & correctness
- **Idle = zero extra redraws.** The frames sub is in the module's `subscription`, gated on
  `is_animating`; iced drops the recipe when it returns false. The bar has no free-running
  redraw today (verified: `RedrawRequest::Wait` is a no-op, unconditional-rendering off), so
  this introduces redraws **only** during the ~150 ms fade after a workspace switch.
- **Animating cost** is ~9 whole-module re-`view`s (150 ms × 60 Hz; more at 144 Hz) per
  switch — negligible, and only on an actual switch. `go_mut` re-bases from the live value,
  so a flurry of switches eases continuously, no pile-up. The map is bounded by workspace
  count and evicted on every update (§3.1).
- **One `now` per frame**, read at the top of `view` — no intra-frame skew. The
  `subscription`'s own `is_animating(now)` read uses a `now ≥` the frame instant, so it can
  only drop the sub *after* the visual finishes, never mid-fade. Safe direction.

## 6. Explicitly out of scope (v1) → follow-ups
- **Hover-lift on the islands pills** (shadow-grow + accent-brighten on hover) — the same
  `Animation` plumbing, but Bar-level (needs a `Bar` per-pill map with a `retain` evictor +
  a `PillKey` enum so chrome can't collide with `stable_id`). A good fast-follow; demoted
  here because hover is invisible in the medium that sells the bar.
- **Positional slide** (a highlight bar physically translating between pills) — needs laid-
  out pill geometry + animated `Length`; richer than the cross-fade. Later.
- **Opacity fades / scale** (popups materializing) — iced 0.14 has neither; custom widget or
  a future iced. Separate RFC.
- **Eased graph/value transitions** (a metric number rolling, graph bars sliding) — per-
  module, best in the `canvas` (full GPU control).
- **Plugin-authored animation** — the DSL is a static snapshot; needs an `animated` node or
  host-side tween between trees. A real ABI design, deferred.

## 7. Open questions (resolve in review)
1. **Cross-fade vs richer.** Is the accent cross-fade enough to read as "premium motion" in
   a GIF, or does v1 need the positional slide to land the "what bar is *that*?" — accepting
   the extra geometry work?
2. **Exact values/curve.** accent alphas (~0.18 bg / ~0.35 border), 150 ms, `EaseOutCubic` —
   tasteful and glanceable (not distracting on a status bar)?
3. **`now` source.** `Instant::now()` at the top of `view` is the pragmatic read (no per-pill
   skew). Confirm there's no cleaner 0.14 channel for the frame instant into a module `view`.
