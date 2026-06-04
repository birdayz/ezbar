# RFC 0013: read-only sway state — the capability that needs the first WIT version bump

- **Status:** **Implemented** (v2) — both phases shipped + verified via `preview --check`.
  Phase 1 (the version-window): v0.1.0 weather (78 popup nodes, unchanged) and a v0.2.0 plugin
  co-load on one binary. Phase 2 (sway-read, **pull**): the `wintitle` dogfood (a v0.2.0 plugin)
  reads `ctx.sway_snapshot()` and renders. ACK'd by a systems (Torvalds) and a platform/security
  reviewer (v1 was a soft-NAK from both on **push**; folded to pull below). **Follow-up:** Go-SDK
  `SwaySnapshot` parity needs the v0.2.0 Go bindings regenerated (blocked by the tinygo/Go
  version skew); the Rust SDK + dogfood are the proven path.
- **Created:** 2026-06-04
- **Target:** ezbar (Rust / wasmtime reactor + guest SDKs + the frozen WIT)
- **Depends on:** RFC 0006 (WASM plugins + the **frozen-version-window** §4 this finally
  exercises, incl. the `ezbar:api-version` custom section §4.4), RFC 0008 (reactor), RFC 0012
  (host feeds — the bar-injects-source + capability pattern this reuses).

## What changed in v2 (review fold-ins)

Both reviewers ACK'd the version-window machinery and the read-only stance, and both
independently said **the guest surface must be PULL, not push.** Folded:

1. **PULL, not push.** v1 delivered sway state as a new `Event::Sway` variant (mirroring
   feeds). Wrong shape: a feed is a *time series* (a sparkline needs the sequence), but sway
   state is a *snapshot* — a workspaces widget wants "the current list to render," and the
   host source (`sources::sway`) is **already a `watch::Receiver<Arc<Snapshot>>`** (a
   get-latest primitive, not a delta stream). So the guest gets a host import
   `sway-snapshot() -> result<sway-state, string>` it calls in `update()`, NOT an event. This
   (a) deletes the mirror-into-`self` boilerplate push forces; (b) gives **synchronous denial**
   (`result`, like `http_get`) instead of feeds' silent fire-and-forget footgun; (c) keeps the
   v0.2.0 `event` enum **byte-identical to v1's** (no 5th variant) so the reactor's
   event-construction path never forks and the whole fan-out/subscribe/dedup machinery
   evaporates; (d) is trivially symmetric across Rust + Go (a `Ctx` method, sidestepping the
   existing Rust-vs-Go asymmetry where Go intercepts `Config` into `Load`).
2. **Split into two phases** (§8). The infra is the bulk; the sway surface is small. Phase 1
   stands up the version-window with a **copy-only** v0.2.0 (zero new surface) and proves a
   v0.1.0 plugin (weather) and a v0.2.0 plugin both load against one binary. Phase 2 adds
   sway-read as a thin consumer. A reviewer can't tell a window bug from a sway bug if combined.
3. **Version detection decided: introspect, anchored on the existing `ezbar:api-version`
   custom section** (RFC 0006 §4.4) read *before* compiling the Component (cheap reject of
   out-of-window plugins), cross-checked against the component's imported
   `ezbar:plugin/host@x.y` interface id. The "try-newest-then-fallback" alternative is a
   footgun (masks real link errors as version mismatches, doubles instantiate cost) — dropped.
4. **The "v2 linker wires one extra import" claim was false — corrected.** The v2 `bindgen!`
   generates a *new* `host::Host` trait (new module path + the new fn), so the host re-impls
   the **whole** import block. The fix: factor each import body into a shared free fn that both
   `impl host::Host for Host` blocks call. This (not the drive loop) is the real weight.
5. **Drive loop does NOT fork.** Only 4 call sites (`instantiate`/`init`/`update`/`view+popup`)
   are version-specific; `lift`/`build`/`measure`/feeds/epoch/timer/the `Wake` enum are shared
   (the `ui`/`Tree` types are identical across versions). Use `enum DrivenPlugin { V1, V2 }`,
   not a trait over two foreign bindgen types.
6. **Dogfood retargeted for honesty.** The bounded DSL has **no fill/border/background** node
   (`Paint` only colors text/icon/graph *lines*), so a plugin workspaces pill can't reproduce
   the built-in's chips/underbars/urgent-fill/cross-fade and — being read-only (no
   `run_command`) — is non-interactive. The primary dogfood is now a **window-title widget**
   (`text(title)`, which the DSL renders faithfully); `wsplugin` is demoted to an explicitly
   minimal, read-only text indicator with that caveat stated. A DSL fill node is a separate
   future RFC, not snuck in here.
7. **Security sentence added** (§7): the window title is **attacker-controlled text** (any app
   sets its own title); composed with a `network` grant on the same module it's an exfil
   value. Standard "two capabilities compose" caveat — keep read-only hard out of scope.

## 1. Problem

RFC 0007 found the workspace/title widgets can't be plugins; RFC 0012 (feeds) fixed *half* of
"safe capabilities" — system metrics. The other half is **sway state**: a plugin can't see
the workspace list or the focused window title, so those widgets must stay built-in. The data
exists host-side (`src/sources/sway.rs`: `workspaces()`/`title()`/a `watch`-backed
`Snapshot`); the gap is purely the **ABI** — and a structured snapshot (a *list* of
workspaces, a title *string*) doesn't fit feeds' single `f64`. **So this is the feature that
forces the first WIT version bump (`since-v0.2.0`)** and the frozen-version-window machinery
RFC 0006 §4 designed but nothing has needed yet. That machinery is the bulk of this RFC.

## 2. Goal & non-goals

**Goal.** A sandboxed plugin **reads** sway workspace state + the focused window title (a
get-current call, capability-gated), enough for a title widget or a (minimal, read-only)
workspaces indicator. Existing v0.1.0 plugins (weather) keep working **unchanged** on the same
host binary.

**Non-goals:** **writing** to sway (`run_command`, switching workspaces) — read-only, hard out
(a plugin that drives the compositor is input-injection, a different and far more dangerous
capability). Arbitrary sway IPC (tree/marks). `layout`/output name (deferred to v0.3.0 — it's
in the same `Snapshot`, a one-field additive WIT bump later). Re-freezing v0.1.0 (it stays
exactly as shipped; v0.2.0 is copy+add).

## 3. The crux: the frozen-version-window (RFC 0006 §4, finally built)

Sway state is added in a **new** `wit/since-v0.2.0/` — a *copy* of v0.1.0 plus the sway import
(never an edit to the frozen dir). The host compiles **both** versions and drives each plugin
with the bindings matching the version it was built against.

### 3.1 Dual bindgen, dual linker — and the real cost
A second `bindgen!` over `wit/since-v0.2.0` (e.g. `mod v2`) alongside the v0.1.0 one; two
`Linker<Host>` on the shared engine. **The honest weight:** the v2 bindgen emits a *new*
`v2::…::host::Host` trait, so the host needs a **second `impl host::Host for Host`** with every
method — not "one extra import." Factor each current import body
(`log`/`text_size`/`fg`/`set_timeout`/`subscribe`/`http_get`/`read_file`/`feed_subscribe`,
lib.rs:119–179) into a shared free fn that both version impls delegate to; the v2 impl adds
`sway_snapshot`. `add_to_linker` is likewise per-version.

### 3.2 Version detection — introspect, anchored on `ezbar:api-version`
On load, read the **`ezbar:api-version` custom section** (RFC 0006 §4.4) from the wasm *before*
compiling the Component — it's the authoritative selector and lets an out-of-window version be
rejected cheaply (refuse-and-explain, §4.5). Cross-check against the compiled component's
imported interface id (`wasmtime`'s `Component::component_type().imports(&engine)` yields
`(name, ComponentItem)` with names like `ezbar:plugin/host@0.1.0`) to catch a mislabeled
plugin. The detected version is stored on the `WasmModule` and logged. (No
try-newest-fallback — it masks real link errors as version mismatches.)

### 3.3 The drive loop stays single
`drive()`/`step()` are ~95% version-agnostic — epoch, WALL, `fold_timer`, `register_feeds`,
the feed hub, scroll coalescing, the `select!`, the `Slot`, and crucially `lift()`/`build()`
(the `ui`/`Tree` types are **identical** across versions). The version-specific surface is
just the 4 call sites + nothing else (pull adds no `Event` variant, so event construction is
shared). Model it as `enum DrivenPlugin { V1(v1::Plugin), V2(v2::Plugin) }`; `step()` matches
the arm to pick `call_update`/`call_view`/`call_popup`. The loop is **not** forked — if it is,
it's wrong.

## 4. The sway surface (the small part, PULL) — `wit/since-v0.2.0`

Copy v0.1.0, then add to `types` + `host`:

```wit
// types: add
record sway-workspace { name: string, focused: bool, visible: bool, urgent: bool }
record sway-state { workspaces: list<sway-workspace>, title: string }
// host: add (gated by `bar-state { sway }`) — a PULL get-current, with synchronous denial
sway-snapshot: func() -> result<sway-state, string>;
```

No new `event` variant. The bar injects the source (`set_sway_source`, fed by
`sources::sway`'s existing `watch::Receiver<Arc<Snapshot>>` on the bar's runtime); the host
keeps that one shared receiver and `sway_snapshot` returns the **latest** snapshot on demand
(`Err("capability denied: sway not granted")` if `[modules.<id>].sway` is unset — synchronous,
like `http_get`). The guest calls it in `update()` (e.g. on its `Event::Timer` cadence) and
renders from the result. **No subscribe, no fan-out, no dedup, no teardown race** — the
snapshot is read, not pushed.

**Capability:** `[modules.<id>].sway = true` (a bool, not a list). Read-only, already-on-screen
data from one cohesive source — splitting `["workspaces","title"]` buys nothing (unlike feeds,
whose list gates genuinely different sources cpu/battery/net). Ungranted → the call returns
`Err` (and is logged), the plugin can degrade gracefully.

## 5. Guest SDKs + dogfood
- Rust + Go SDKs gain `Ctx::sway_snapshot() -> Result<SwayState, String>` (Go:
  `SwaySnapshot() (SwayState, error)`) — a `Ctx` method exactly like `http_get`, symmetric
  across both SDKs (pull sidesteps the Rust-surfaces-`Config` / Go-intercepts-it asymmetry that
  a new event arm would have hit). `SwayState { workspaces: Vec<SwayWorkspace>, title: String }`.
- **Dogfood (primary): a `wintitle` plugin** — `text(snapshot.title)`, which the DSL renders
  faithfully. The honest proof that a sway-reading widget can be a sandboxed plugin.
- **Dogfood (secondary): `wsplugin`**, an explicitly-minimal, **read-only, non-interactive**
  workspaces indicator — a `row` of `text` pills (focused in `Token::Accent`, others
  `Token::FgDim`). Shipped with a plain caveat: the DSL has no fill/border node and no
  `run_command`, so this is a glance indicator, **not** a replacement for the built-in
  `workspaces` (which keeps its chips/underbars/cross-fade/click-to-switch). A DSL fill node to
  close that gap is a separate RFC.

## 6. Perf, safety, compat
- **v0.1.0 plugins untouched:** weather loads against `linker_v1`, same 4-variant event,
  byte-for-byte. The version window is the whole point — Phase 1 proves it in isolation.
- **Read-only:** no `run_command` export; the plugin cannot move the compositor. **Most
  important security call here — keep it hard out of scope.**
- **One watcher, read on demand:** the host holds one `watch` receiver; `sway_snapshot` is a
  cheap clone of the latest `Arc<Snapshot>`. Idle when no plugin calls it.

## 7. Security note (the one sentence v1 missed)
The focused-window **title is attacker-controlled text** — any running app sets its own title,
and workspace names are user/app-set strings. Composed with a `network` grant **on the same
module**, a plugin could exfiltrate another app's title/workspace names (`http_get` to a
granted host). This is the standard "two capabilities compose" property (a `network`-granted
plugin is already a trusted exfil surface); sway-read merely **widens what flows in**. Mitigation
is the grant model itself: grant `sway` + `network` to the same module only if you trust it.
Read-only (no write to sway) keeps the *authority* a plugin gains bounded to "see what's on
screen," which is the right line.

## 8. Phasing (the split both reviewers asked for)
- **Phase 1 — the version-window (load-bearing, risky, review in isolation).** `since-v0.2.0`
  as a **pure copy** of v0.1.0 (zero new surface); dual bindgen; the shared-free-fn refactor of
  the `Host` import bodies + a second version impl; `enum DrivenPlugin`; `ezbar:api-version`
  detection + import cross-check. **Acceptance:** weather (v0.1.0) and a trivially-rebuilt
  v0.2.0 plugin both load and run against the same host binary; out-of-window version is
  refused-and-explained.
- **Phase 2 — sway-read (thin consumer).** Add the `sway-snapshot` import to the v0.2.0 WIT +
  the shared free fn (reads the injected `watch`); `set_sway_source` wired in `main.rs`;
  `Ctx::sway_snapshot()` in both SDKs; the `wintitle` + `wsplugin` dogfoods.

## 9. Open questions — resolved
1. **Detection** → introspect, anchored on `ezbar:api-version` (§3.2). Resolved.
2. **Drive-loop genericity** → `enum DrivenPlugin`, loop not forked (§3.3). Resolved.
3. **Push vs pull** → **pull** (§4) — friendlier *and* cheaper. Resolved (the headline change).
4. **State scope** → `workspaces` + `title` for v0.2.0; `layout`/output additively in v0.3.0
   (same `Snapshot`, cheap). Resolved.
5. **Worth it now?** → yes: feeds-wants-richer-than-`f64` is a known second consumer, so the
   window carries its own weight; pull + the Phase-1 split make the first consumer cheap.
