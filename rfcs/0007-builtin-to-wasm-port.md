# RFC 0007: Porting built-in widgets to WASM plugins — which, and why mostly not

- **Status:** **Draft** — pending decision on the final port list
- **Created:** 2026-06-03
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0006 (WASM plugins — the sandbox + bounded UI vocabulary)

## TL;DR

We have 21 built-in widgets and 3 WASM plugins (`weather`, `btc`, `quakes`).
The question: port the remaining built-ins to plugins, *unless there's a genuine
reason a given one is worse as a plugin*. The hard gate: **no visible diff, or we
don't do it.**

**Answer, blunt version:** with the sandbox as it actually stands today, **zero of
the 21 port without a visible diff.** Exactly **one** — `stock` — is a real
"network widget mis-filed as a built-in" and becomes pixel-identical after *one
additive UI primitive* (a sparkline node). Everything else has a genuine reason to
stay built-in: it needs a privilege the sandbox exists to deny (compositor IPC,
subprocess, `/proc`, `/sys`, pointer-driven mutation), or porting it produces a
strictly more fragile copy of an identical widget for zero user benefit.

The honest recommendation is therefore: **port `stock`, keep the other 20
built-in.** Detail and the full table below.

## 1. The binding constraint: what the sandbox can do *today*

RFC 0006 specifies a generous host ABI. The **implemented** host
(`crates/ezbar-wasm/src/lib.rs`) is far narrower. This gap is the whole story.

| Host import (WIT) | Spec intent | Implemented? | Reality |
|---|---|---|---|
| `http-get(url)` | gated network fetch | **yes** | works; gated by granted hosts |
| `read-file(path)` | gated file read | **no** | always returns `Err` (`lib.rs:109`) |
| `feed-subscribe(feed)` | cpu/mem/temp/ping/battery/net feeds | **no** | no-op stub (`lib.rs:112`) — never delivers a sample |
| `set-timeout(ms)` | pick your own cadence | **no** | no-op stub (`lib.rs:84`) — cadence is fixed |
| `subscribe(kinds)` | opt into pointer/feed/config events | **no** | no-op stub (`lib.rs:85`) |
| pointer events | click / scroll / press in chip & popup | **no** | actor delivers only `Event::Timer`; `mouse-area` renders but is inert ("interactivity is phase-2b", `lib.rs:688`) |
| timer cadence | — | fixed | hard-coded `POLL = 2s` (`lib.rs:41`) |
| config | init + live reload | init only | `init` gets string pairs; no live `config` event |
| hover→popup | open a detail surface on hover | **yes** | host-driven via `hover_messages` + whole-pill `mouse_area` — *not* a guest pointer event, so it works |

So the **sandbox envelope** a plugin can fill diff-free is exactly:

> pure-HTTP data · read-only · hover-to-open popup · ≥2s cadence · no local files · config at init only

That envelope is *precisely* `weather` / `btc` / `quakes`. Nothing about that is an
accident — it's the contract those three already prove out.

Anything outside the envelope means either (a) a visible diff, which the gate
forbids, or (b) growing the ABI to re-expose subprocess / sway-IPC / pointer
mutation / arbitrary file reads — i.e. dismantling the sandbox the RFC 0006 review
fought to keep (`exec` was explicitly cut; Torvalds #1/#4).

## 2. The full table (all 21 built-ins)

Verdict legend: **PORT** (diff-free achievable) · **KEEP** (genuine reason to stay
built-in) · **(feed)** technically feasible only after re-privileging the sandbox,
not recommended.

| Widget | Data source | Interactivity | Outside-envelope blocker(s) | Diff if ported now | Verdict |
|---|---|---|---|---|---|
| **stock** | HTTP quote API | hover popup | chip sparkline (`MiniTrend` 28×18) has no node | chip sparkline only | **PORT** ¹ |
| weather | HTTP | hover popup | — | — | already a plugin |
| btc | HTTP | hover popup | — | — | already a plugin |
| quakes | HTTP | hover popup | — | — | already a plugin |
| cpu | `/proc/stat` (feed) | **click toggles graph** | feeds stubbed + no pointer events | no data; toggle dead | KEEP (feed) ² |
| memory | `/proc/meminfo` (feed) | **click toggles graph** | feeds stubbed + no pointer events | no data; toggle dead | KEEP (feed) ² |
| temperature | `/sys` (feed) | **click toggles graph** | feeds stubbed + no pointer events | no data; toggle dead | KEEP (feed) ² |
| ping | host runs `ping` (feed) | **click toggles graph** | feeds stubbed + no pointer events | no data; toggle dead | KEEP (feed) ² |
| net | `/proc/net/dev` (feed) | — | feed carries **1** f64; net shows **↓ and ↑** | down/up collapse to one number | KEEP (feed) ² |
| battery | `/sys/class/power_supply` (feed) | — | feed = 1 f64 (no charging state); `visible()` self-hide has no plugin equivalent | charging state lost; can't hide on desktops | KEEP (feed) ² |
| clock | `chrono::Local` (tz) | — | needs local tz **in** sandbox; **1s** tick, default format has seconds | wrong tz risk; seconds lag at 2s poll | KEEP ³ |
| calendar | HTTP iCal | hover popup | reads `~/.config/ezbar/calendar_url` (file); local tz; **500ms blink** | can't read URL; tz; blink can't run at 2s | KEEP ³ |
| github | HTTP REST | **click / right-click**, interactive popup | token via `gh auth token` / env + `github_config.json` file; popup opens URLs & marks-read (mutations) | no token; dead popup actions | KEEP ³ |
| ip | `ip route get 8.8.8.8` | — | local kernel routing query; HTTP gives **public** IP (different value) | shows a different IP | KEEP |
| disk | `df` subprocess | — | no subprocess, no `statvfs`, no feed | no data | KEEP |
| claude | `/proc` walk + `bunx ccusage` | hover popup | process enumeration + subprocess | no data | KEEP |
| volume | `pactl`/`amixer` | **click mute, scroll ±** | subprocess; click/scroll **mutate** audio | no data; controls dead | KEEP |
| spotify | MPRIS/HTTP + OAuth | **click play/pause, scroll skip** | TCP listener + browser spawn for OAuth; controls mutate | auth impossible; controls dead | KEEP |
| kubectl | `kubectl` subprocess | **click / right-click** switches context | subprocess; context switch mutates | no data; switch dead | KEEP |
| updates | `checkupdates` + spawn updater | **click runs updater** | subprocess (read **and** the click action) | no data; click dead | KEEP |
| custom | arbitrary shell command | **click runs command** | it *is* a subprocess executor | the entire widget | KEEP (by definition) |
| workspaces | sway IPC | **click / scroll** switch workspace | sway IPC read + mutate | no data; switching dead | KEEP |
| window_title | sway IPC (`get_tree`) | — | sway IPC; no feed for it | no data | KEEP |
| keyboard | sway IPC (`get_inputs`) | **click** switches layout | sway IPC read + mutate | no data; switch dead | KEEP |

¹ `stock` — the one genuine port. Data is pure HTTP; the hover popup chart maps
exactly onto the existing `chart-node` (`StockChart`). The *only* gap is the chip's
mini gradient sparkline (`MiniTrend`, 28×18): `graph-node` renders the cpu-style
line at a fixed 48×16, and `chart-node` is the big popup renderer — neither
reproduces it. Add a `mini-trend` node (values + colour, rendered at 28×18) to the
WIT + host. It's **additive** (no change to existing plugins, no diff). API key →
`init` config; the quote host → a network grant. Self-throttle the 300s refresh
internally, exactly as `weather` does its 15-min cooldown over the 2s tick.

² **The "feed" tier — feasible only after dismantling part of the sandbox, and
worse even then.** cpu/memory/temperature/ping/net/battery are the six declared
`feed-kind`s. To render them a plugin needs: (a) feed delivery actually
implemented; (b) `set-timeout` so cadence matches (memory 3s, battery 5s, …); and
for cpu/mem/temp/ping (c) pointer events, because each has *click-to-toggle-graph*.
That's re-adding three host capabilities to the sandbox so a plugin can render a
number and a sparkline **the host already computed** — an ABI hop, a serialization,
a second actor thread, and a trap surface, in exchange for zero user-visible
benefit. net and battery can't even be expressed: net is two numbers (↓/↑) through
a one-`f64` feed; battery loses charging state and the desktop self-hide
(`visible()`), neither of which the plugin ABI has. This is the textbook "genuine
reason it's worse as a plugin." **Recommend: don't.**

³ **HTTP-but-not-only.** clock/calendar/github look like network widgets but each
trips the envelope: clock needs local tz + a 1s seconds tick; calendar reads a
secret-URL file, needs local tz, and blinks at 500ms; github needs a local token
(`gh`/env/file) and an *interactive* popup that opens URLs and marks notifications
read (mutations). Each is one or more hard diffs today.

## 3. Recommendation

1. **Port `stock`** (Tier PORT), gated on first adding the additive `mini-trend`
   sparkline primitive to `wit/since-v0.1.0` + the host renderer. Verify
   pixel-identity in the preview harness (`--check`) and against a screenshot
   before deleting the built-in.
2. **Keep the other 20 built-in.** Each has a stated, genuine reason above — not
   inertia.
3. **Do not build the "feed" tier.** If we ever want it, it's a separate decision
   with its own RFC, because it changes the sandbox's threat model (pointer events,
   feed delivery, timers) — not a free "port."
4. If the goal is a *bigger plugin surface*, the leverage isn't porting built-ins —
   it's making the SDK/ABI excellent for **new third-party network widgets**, which
   is the thing the sandbox is actually shaped for.

## 4. What "no diff" verification looks like (for `stock`)

- Build the `.wasm`, run it through `cargo run -p ezbar-wasm --example preview --
  --check` (node-count smoke test) and the visual preview.
- Side-by-side the built-in `stock` and the plugin on the same theme: chip glyph,
  colour thresholds (green up / red down / grey flat), sparkline size & gradient,
  and the 7-day hover chart must match.
- Only when identical: remove `src/modules/stock.rs` + its source, drop `stock`
  from the built-in registry, and ship the `.wasm`.

## Open question for the maintainer

Confirm the final list before any implementation:

- **(a)** Port `stock` only (this RFC's recommendation), or
- **(b)** also take on the "feed" tier (cpu/memory/temperature/ping) — accepting
  that it requires implementing feed delivery + `set-timeout` + pointer events
  first, and yields a more fragile copy of an identical widget, or
- **(c)** a different cut.
