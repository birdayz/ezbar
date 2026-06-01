# RFC 0004: Config-reconciled surfaces & modules

- **Status:** Draft (v2 — implemented on branch `rfc-0004-reconcile-poc`; passed an
  adversarial wlr-layer-shell review, fixes applied. v1 was PoC-only.)
- **Created:** 2026-06-01
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (module SDK, `Factory`, phase-2 dlopen), RFC 0002 (config,
  hot-reload diff, `key` identity, `config_generation`)
- **Supersedes:** RFC 0002 §"Surface lifecycle (re-roll honesty)" — its core assumption
  is wrong (see *Surface geometry reconcile*).

## Summary

Make the bar's **surface set** and **module set** a pure function of config, **reconciled**
on reload by diffing desired-vs-live and applying the minimal delta — one rule:
`desired = f(config)`; on change, reconcile.

Today only the *render* path is reactive: `view`/`style`/`subscription` re-derive from
`self.config`/`self.theme`/`self.modules` every frame, which is why **theme and the
module list already hot-reload**. But two things are still imperative one-shots and never
re-read: **surface geometry** (baked into `NewLayerShellSettings` at `NewLayerShell` time)
and **module construction** (nuke-and-rebuilt wholesale). That's why `position = "top"` +
reload does nothing, and why changing one module entry resets *every* module's state.

A **PoC** (this branch) proves `iced_layershell` can reconfigure a *live* layer surface in
place — anchor, margin, exclusive-zone, size — so geometry reconciles **without re-rolling
the surface**, refuting RFC 0002's guess. Modules reconcile **by `key`** (RFC 0002
identity), preserving untouched instances across reload. The same reconcile point is where
**dynamically-discovered modules** (RFC 0001 phase 2) plug in — the extension chain the
request asked for.

## Motivation

- **The visible bug.** Edit `[bar] position = "top"`, reload → nothing moves. `apply_config`
  only swaps theme and (crudely) the module list; `position`/`height`/`margin`/`layer` were
  consumed *once* by `bar_settings` → `NewLayerShell` and never read again.
- **The deeper issue.** The lifecycle is half-declarative. `view()` is a pure function of
  config (theme + module list reload through it). The imperative remainder — surface
  creation, module construction — is one-shot. We want the *whole* lifecycle to follow the
  declarative rule the render path already follows.
- **Spec debt.** RFC 0002 already designed the module reconcile (`key` identity,
  generation-keyed resubscription, `reconfigure()`), but it's largely unimplemented; today's
  `rebuild_modules()` is nuke-and-rebuild. And RFC 0002's *surface* story was an untested
  guess ("can't be live-resized → re-roll"). This RFC implements the reconcile and corrects
  the surface story with proof.

## Goals / Non-goals

**Goals.** Geometry hot-reload **in place**; module reconcile **by `key`** (preserve
unchanged); a **single reconcile entry point**; per-output surface set; the foundation for
dynamic discovery; and `ezbar msg position` (falls out for free).

**Non-goals.** Rewriting the Elm loop — it is *already* the right declarative core; **free
window-dragging** — layer-shell surfaces are docked chrome, the compositor owns their
position, there is no move-grab; the **left/right vertical-bar layout** (own RFC);
implementing dlopen discovery (RFC 0001 phase 2).

## Design

### What is already reactive — leave it alone

`view()`, `style()`, `subscription()` are pure functions of `self.config` / `self.theme` /
`self.modules`, re-run every frame. Theme changes and module **list** changes already
hot-reload through this path. The state machine does **not** need re-architecting; the
"config → event → redraw" chain the request describes already exists — `ConfigReloaded` *is*
the event, iced's runtime *is* the bus.

### What is not — the gap

Two imperative one-shots:

1. **Surface geometry** — anchor/size/margin/exclusive-zone/layer are baked into
   `NewLayerShellSettings` at creation (`bar_settings`, `main.rs`) and never re-read.
2. **Module instances** — constructed once; on any set change, `rebuild_modules()` shuts
   down **all** of them and rebuilds.

Everything else hot-reloads. These two are the entire scope.

### Reconcile model

Derive a **desired state** purely from `Config`:

- **Surface set** — for each target output, one bar surface with geometry
  `G = (anchor, size, margin, exclusive_zone, layer)`.
- **Module set** — ordered instances per zone, each with `key` identity (RFC 0002) and
  resolved config.

On `ConfigReloaded(cfg)` (and on `ezbar msg`), compute desired, **diff vs live, emit the
minimal ops**. Reconcile is idempotent: same config in ⇒ no-ops out.

### Surface geometry reconcile — IN PLACE (PoC-validated)

**Finding.** `iced_layershell` 0.18 exposes live-surface mutators, generated onto our own
`Message` enum by `#[to_layer_message(multi)]`, each keyed by `window::Id`:

```
AnchorChange { id, anchor }            MarginChange       { id, margin }
SizeChange   { id, size }              ExclusiveZoneChange{ id, zone_size }
LayerChange  { id, layer }             AnchorSizeChange   { id, anchor, size }
```

Emitting these as `Task::done(...)` reconfigures the existing `zwlr_layer_surface_v1` and
re-commits — **no destroy/recreate**. They are consumed by the layershell runtime, not our
`update`.

**PoC.** A `reconcile_bar_geometry(pos)` that emits `AnchorChange` + `MarginChange` +
`ExclusiveZoneChange` + `SizeChange` for `bar_id`, driven two ways:

- `ezbar msg position <top|bottom|toggle>` → live re-anchor.
- file-watch reload of `position = "top"` → `apply_config` diffs geometry, reconciles.

Both moved the bar between edges **in place** — no flicker, and the exclusive zone reflowed
(tiled windows shifted). The same surface; only its anchor/margin/zone changed.

Default (`position = "bottom"`):

![PoC — bar anchored bottom](assets/0004-poc-bottom.png)

After `position = "top"` applied by **file-watch hot-reload** (no restart, no re-roll):

![PoC — same surface re-anchored top by reload](assets/0004-poc-top.png)

**This supersedes RFC 0002 §"Surface lifecycle (re-roll honesty)".** That section assumed
geometry "can't be live-resized" and specced a double-buffered re-roll (open new surface,
await first commit, close old, *tag it intentional* so it doesn't trip the exit-on-bar-close
path). The PoC shows that machinery is unnecessary: in-place reconfigure is simpler,
flicker-free, and sidesteps the exit-on-close coupling entirely. **Re-roll survives only for
what the protocol genuinely can't mutate in place:** moving a surface to a different
`output`, and adding/removing surfaces (multi-output). `anchor`/`size`/`margin`/
`exclusive_zone`/`layer` all mutate live.

*Honesty:* validated on **sway** (wlroots). These are standard `zwlr_layer_surface_v1`
requests, so other wlroots compositors should match; Hyprland to confirm.

### Module reconcile — by `key` (designed, NOT yet prototyped)

RFC 0002 already designed this; here it is implemented. Today `rebuild_modules()` shuts down
**all** modules on **any** set change — dropping every instance's state and resubscribing
even untouched ones (a real latent bug: add one entry and your CPU graph history resets
across the bar).

Reconcile instead — index live instances by `key`, diff desired vs live:

- **kept** (same `key`, same resolved config) → untouched: instance, state, subscription all
  retained.
- **added** → construct via RFC 0001 `Factory(instance_id, &cfg)`.
- **removed** → `shutdown()`.
- **reconfigured** (same `key`, changed config) → RFC 0002 `reconfigure(&cfg) -> Reconfigure`
  + host `config_generation` bump to re-key the subscription when needed.
- **reordered** → view order follows config; no churn.

**Prerequisite / crux decision:** instance identity must become `instance_id = hash(key)`
(stable across zone and order), not today's positional `u64`. This is the one genuine
refactor, and the place to be careful — subscription re-keying interacts with iced's recipe
identity `(TypeId, hashed-data, fn-ptr)`. RFC 0002 §Hot-reload worked this out on paper; it
deserves **its own PoC** before landing. Marked not-yet-validated, deliberately.

### Multi-output / per-output surfaces

Today: a single surface (`OutputOption::None`); the launcher respawns the whole process on
monitor change. Desired: `[bar].outputs = "all" | ["DP-1"]` ⇒ **one surface per matching
output**, keyed by output name. Reconcile on output hotplug by the same diff — add a surface
for a new output, drop it for a removed one. This is the one place the surface *set* (not
just geometry) changes, so `NewLayerShell`/close are used here; it is the **only** remaining
create/destroy path. Medium effort (needs output enumeration); lands after geometry
reconcile.

### Where dynamic discovery plugs in (RFC 0001 phase 2)

dlopen-discovered modules are just **more entries in the desired module set**. Discovery
(scan the plugin dir, validate the ABI/toolchain stamp) yields additional `Factory`s; the
**same** module reconcile adds/removes their instances. No separate code path, no second
chain — `config(+discovery) → reconcile → modules` is the single extension point.

### Single reconcile entry point

Three call sites already funnel into `apply_config` (file-watch reload, `ezbar msg reload`,
preset switch). Keep the funnel; make `apply_config(cfg) -> Task<Message>` the reconcile
root:

- **theme/preset** → live re-render (no churn) — the cheap path; why a preset swap feels
  instant.
- **modules** → diff by `key`.
- **surfaces** → geometry **in place**; output/set changes via create/destroy.

The PoC already shipped step 1 of this (geometry).

## Alternatives considered

1. **Re-roll the surface (RFC 0002's plan).** Rejected by PoC: in-place is simpler and
   flicker-free; re-roll needs the open-new/await-commit/close-old + exit-guard dance. Re-roll
   kept only for `output` moves and surface add/remove.
2. **Full rewrite to a retained scene driven by a new event bus.** Rejected: the render path
   is *already* declarative and `ConfigReloaded` is already the event. A rewrite is risk for
   no gain. Reconcile the two imperative one-shots; leave the working core.
3. **Restart the bar on geometry change (status quo: launcher respawn).** Rejected: it
   flashes, drops popups, and resets all module state — the very thing this RFC removes.

## Migration

1. ✅ `apply_config -> Task`; `reconcile_bar_geometry` (in place, all of
   position/height/margin/layer). Geometry hot-reload + `ezbar msg position`.
2. ✅ Stable `key`-based identity (`instance_id = stable_id(key)`) → module reconcile by
   diff; unchanged instances keep state + streams; `reconfigure()`/generation for changed.
3. ✅ Per-output surface set + reconcile, driven by `[bar].outputs`, config reload, a
   surface closing, and a sway output-event subscription. A surface closing no longer
   exits the bar. Popups follow the bar edge and the cursor's output.
4. ☐ Dynamic discovery feeds the module reconcile. **← RFC 0001 phase 2 (not built).**

Steps 1–3 are implemented on `rfc-0004-reconcile-poc` and validated live on sway
(single 5120×1440 output): top↔bottom flip in place, floating margin, the cpu graph's
state surviving an unrelated reload, the `outputs` filter add/dropping the surface, and
the app surviving zero surfaces.

### Implementation notes & residual risks (from the layer-shell review)

- **Geometry is committed as few times as possible.** Each iced_layershell mutator
  commits the surface itself, so `reconcile_bar_geometry` applies layer/margin/exclusive-
  zone first, then anchor+size *together and last* via `AnchorSizeChange` — one reflow per
  logical change, not up to four.
- **`OutputName` bind race (the one real multi-output caveat).** iced_layershell resolves
  `OutputOption::OutputName` against its own `wl_output` cache; if a name isn't cached yet
  (sway's Output IPC event can precede the `wl_output` global reaching the client), the
  surface binds to the compositor-default output instead of failing. We *mitigate* with a
  250 ms settle delay before reconciling on an output event, which lets the global land —
  but the **true fix needs an upstream API**: either `OutputOption::Output(wl_output)`
  resolved from the same enumeration, or a bind-result the host can verify and retry on.
  Tracked as the top multi-output follow-up. Single-output and steady-state multi-output
  are correct; the residual is a narrow hotplug-instant race.
- **Subscription generation is monotonic per id** (never reset on removal; bumped on
  re-add) so a removed-then-readded key re-keys past any still-draining recipe.
- **Zero surfaces is a valid state** (iced `StartMode::Background` does not exit at zero
  windows); the output subscription re-adds surfaces as outputs appear, and emits one
  synthetic reconcile per (re)connect so a cold start / sway restart converges.

## Open questions

1. **`OutputName` bind race** — the residual from the review (see *Implementation notes*).
   The proper fix is upstream: expose `OutputOption::Output(wl_output)` or a bind-result.
   Until then we ship the 250 ms settle-delay mitigation. **Top multi-output follow-up.**
2. **Identity** — adopted RFC 0002 `key` semantics (`instance_id = stable_id(key)`). Still
   open: an explicit `id` escape hatch and collision handling (validation error with the
   offending line, as RFC 0002) — today duplicate keys just warn-and-skip the second.
3. ~~**Output hotplug source**~~ — **resolved: sway IPC** (`swayipc` output events). ezbar is
   a sway bar and already depends on swayipc; the host-side reconcile is compositor-agnostic,
   so a `wl_output` source could be swapped in later without touching the reconcile.
4. **`ezbar msg position` persistence** — write to the state file (like presets) or stay
   ephemeral? Today it is ephemeral, so an IPC re-anchor diverges from `config` until the
   next reload re-asserts it. Persisting to state (never `config.toml`) mirrors the preset
   model.
5. **Multi-output stress test** — exclusive-zone / mixed-DPI behaviour was validated on a
   single 5120×1440 output; the per-output path needs a real two-monitor + hotplug pass.
