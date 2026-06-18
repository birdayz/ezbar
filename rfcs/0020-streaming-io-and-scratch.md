# RFC 0020: large payloads in a small sandbox — streaming I/O + plugin scratch

- **Status:** **Proposed** (designed; not yet implemented).
- **Created:** 2026-06-16.
- **Target:** ezbar (the WASM host `crates/ezbar-wasm`, the bar `src/`, the guest SDKs, the WIT `v0.6.0`).
- **Depends on:** RFC 0006 (the sandbox + capability model), RFC 0008 (the reactor + its resource
  caps: epoch / wall-clock / `MEM_LIMIT`), RFC 0015 (`fs`/`exec` capability tiers + WASI preopens),
  RFC 0019 (the `local-timezone` host call — the most recent version-window bump, the template here).

## 1. Problem

The 2 MiB per-plugin memory cap (RFC 0008 §3.4) is **load-bearing** — it is precisely what makes
"a plugin can't OOM the bar" true, and it should stay. But the host's only fetch primitive,
`http-get`, hands the guest the **entire** response as one `list<u8>`:

```wit
http-get: func(url: string) -> result<list<u8>, string>;
```

So a plugin whose data source is large has no choice but to materialise the whole thing in its
2 MiB heap. The `calendar` plugin is the worked example that exposed this: a Google *secret iCal*
URL returns the user's **entire calendar history** — in the field, **18.7 MB / 5288 events**. The
host fetched it fine (it has real memory), but lowering an 18.7 MB list into the guest tried to
grow the guest's linear memory past 2 MiB, the `StoreLimits` limiter denied the grow, the guest
allocator returned null, the canonical ABI turned that into a **trap**, and the reactor disabled
the plugin. A disabled plugin keeps painting its last frame, so the chip **froze** (see the
post-mortem in the calendar work). The guest never even saw the bytes — it died *receiving* them.

The instinctive fix — a per-plugin `max_memory` override so the guest can hold the body — is the
**wrong** fix:

- **It's a treadmill.** The feed grows with the calendar's age; any fixed number eventually
  overflows and the chip freezes again until someone bumps it.
- **It sizes the cage for garbage.** The plugin holds 18.7 MB for the ~10 ms it takes to slice out
  the next day's events, then discards 99% of it — but `wasm` memory never shrinks, so the cap is
  raised *permanently* for a payload the plugin doesn't want.
- **It weakens the one guarantee** the cap exists to provide, for that plugin, forever.

The real defect is **architectural**: bulk data is forced *through* the tiny guest, when the guest
only ever wanted a small *derived view* of it (today's meetings). The reduction is happening on the
wrong side of the sandbox boundary.

## 2. Principle — the guest stays small; the host carries the bulk

> A plugin must be able to **process** an arbitrarily large source without **holding** it. Resident
> guest memory should be `O(working-set)`, never `O(payload)`. The 2 MiB cap is the contract, not
> the obstacle.

This is RFC 0006's founding idea — *the plugin describes intent; the host owns the heavy lifting* —
applied to **I/O**. It needs exactly two primitives, because there are exactly two ways a working
set can exceed the heap:

1. **Stream, don't slurp.** Pull the payload in bounded chunks and filter/reduce as it arrives,
   keeping only the result. (For the calendar: stream the feed, keep only the VEVENTs near today.)
2. **Spill, don't hoard.** When a plugin genuinely needs a staging area larger than its heap, give
   it the *host's disk* — a private scratch dir — instead of its own RAM.

Note the symmetry with what already works: **`fs` reads already stream** (a plugin `BufReader`s a
preopened file via `std::fs` and never slurps it — RFC 0015). So *pull-based, bounded I/O* is
already the norm for the filesystem; `http-get` was the outlier that slurps, and there was no
writable staging area that isn't a *user* directory. This RFC closes both gaps and makes the I/O
story uniform: **every input is streamable; every plugin has private scratch.**

## 3. What plugin authors get (the author-first view)

The test of this design is whether a plugin author reaches for the right thing without thinking.

### 3.1 Streaming a fetch — `ctx.http_open`

The SDK turns the stream into a plain `std::io::Read`, so authors use the Rust they already know:

```rust
// Calendar: stream a 19 MB feed, keep only the next two days — flat memory, any feed size.
let body = ctx.http_open("https://calendar.google.com/.../basic.ics")?; // impl Read; Drop closes
let mut slim = Slimmer::new(today, /*back*/ 1, /*fwd*/ 2);
for line in BufReader::new(body).lines() {
    slim.push(&line?);          // tracks the current VEVENT block; keeps only in-window ones
}
let events = parse_calendar(&slim.finish(), now);   // parses KB, not MB
```

The mental model is one sentence: **"I never hold the whole thing — I stream it through a small
window, or I stage it on `/scratch`."**

- **Same capability as `http-get`.** Streaming is the *same act* (an HTTP GET to a granted host),
  just delivered in chunks. It reuses the existing `network` grant — **no new capability, no new
  consent.** `ctx.http_get` stays for small bodies (it's simpler); `ctx.http_open` is for when the
  body might be big or you only want part of it.
- **It can't hang the bar.** Each chunk read is bounded by the HTTP client's own timeout, and is
  flagged a *blocking service* so the wall-clock backstop waits on legitimate network I/O instead
  of trapping it — the same mechanism `pick` and the `http-get` slow-download fix use.

### 3.2 Private scratch — `/scratch`

Every plugin is handed a private, writable directory preopened at `/scratch`. Use `std::fs` as
normal:

```rust
std::fs::write("/scratch/page-2.json", &bytes)?;   // stage a download, a cache, a work file
```

- **No grant, no config.** It is the plugin's *own* temp space — isolated per plugin, not your
  data, wiped when the plugin reloads or the bar exits. Unlike an RFC 0015 `fs` grant (which exposes
  a *user* directory and therefore needs consent), scratch is the plugin's sandbox-internal disk.
- **It's the disk, not the heap.** A plugin that must assemble something larger than 2 MiB before
  reducing it writes it to `/scratch` and reads it back streaming, never inflating its linear
  memory.

That is the whole author surface: one streaming reader, one scratch dir. No knobs, no per-plugin
memory tuning, no payload-size arithmetic.

## 4. The host mechanics

### 4.1 Streaming fetch — WIT `v0.6.0` (additive)

Three host imports, added with the established version-window bump (a new `since-v0.6.0` dir, a
`mod v6` bindgen remapping `types`/`ui`/`events` to v0.1.0's modules, a `linker_v6`,
`DrivenPlugin::V6`, and `plugin_version` detecting `host@0.6` — exactly the RFC 0019 shape):

```wit
// gated by `network` (same allow-list as http-get).
http-open:  func(url: string) -> result<u64, string>;            // open → opaque stream handle
http-read:  func(stream: u64, max: u32) -> result<list<u8>, string>;  // next ≤max bytes; empty = EOF
http-close: func(stream: u64);                                   // release early (idempotent)
```

**Why a `u64` handle, not a WIT `resource`.** A `resource` is the idiomatic ownership model (the
guest dropping it auto-calls a host `drop`), and it is the right answer *eventually*. But resources
do not remap through the `with:` clause the way `types`/`ui`/`events` do, and async resource-method
codegen is materially more complex than another plain function — every prior host call (`exec`,
`pick`, `sway-snapshot`, `local-timezone`) is an additive func, and the version-window infra is
built for that. A plain handle + a host-side table matches the pattern, keeps the bump mechanical,
and the SDK reclaims the ergonomics (a `Drop` impl calls `http-close`, so authors never see the
handle). We revisit `resource` if/when we grow a family of streaming APIs.

**Host state.** Each plugin's `Host` gains a `HashMap<u64, HttpStream>` and a monotonic id counter.
`HttpStream { resp: reqwest::Response, buf: Bytes }` holds the in-flight response and any
leftover bytes when the guest's `max` is smaller than a chunk. `http-read` drains `buf` first, else
`resp.chunk().await` for the next chunk; `None` ⇒ EOF (drop the entry, return empty). `http-close`
and `Store` teardown remove entries; reqwest closing the `Response` tears down the connection.

**Resource bounds.** A leaked/never-closed stream holds a connection + a chunk buffer until the
`Store` drops. We cap **concurrent open streams per plugin** (small, e.g. 8) so a buggy plugin can't
exhaust sockets, and the SDK's `Drop` closes promptly in the common path.

**CPU/wall-clock.** Each network `await` (`http-open`'s send, `http-read`'s chunk) sets the
`in_blocking_service` flag so the 12 s WALL backstop *waits* rather than disabling — a multi-second
large download is parked on I/O, not running guest code (the guest burns no epoch while parked).
Between reads the flag is clear, so guest *code* (the slimming) is still bounded by the epoch
deadline and by WALL — a plugin that opens a stream and then CPU-spins is still caught. The HTTP
client's per-read timeout is the hard bound on any single read. (Same trade-off as `pick`/RFC 0019.)

**Why not a size-capped `http-get`?** Truncating a payload at N bytes corrupts it (half an iCal is
not iCal). The guest has to *choose what to keep*, which requires seeing the bytes incrementally —
i.e. streaming. A cap only turns a trap into a silent wrong answer.

### 4.2 Scratch dir

On instantiate, the host creates a private directory — `$XDG_RUNTIME_DIR/ezbar/scratch/<id>/`
(tmpfs-backed on Linux; falls back to the system temp dir) — and preopens it **read-write** at the
guest path `/scratch`, reusing the RFC 0015 `FsGrant`/`build_wasi` machinery (it is just an
auto-added, host-owned grant). It is removed on plugin teardown and re-created empty on reload, so
state never leaks between versions of a plugin.

It is deliberately **not** a capability: it exposes nothing of the user's, escapes nowhere (WASI
jails the preopen), and is wiped automatically — so there is nothing to consent to, unlike a `fs`
grant onto a real directory.

**Disk bound (honest limitation).** WASI preopens don't carry a quota, and a strict per-dir byte
limit needs OS-level quotas or a privileged sized-tmpfs mount we can't assume. So enforcement is
**best-effort**: scratch lives on a tmpfs (RAM-bounded by the OS already), the reactor samples the
dir's size on its existing tick and logs + may refuse further service if a plugin blows a soft cap
(default 64 MiB), and teardown reclaims it. Strict quotas are a future hardening, called out rather
than pretended.

## 5. Security & safety

- **Streaming adds no capability surface.** It is the same `network` grant and the same bytes a
  plugin could already `http-get`, just chunked — nothing new to inspect or consent to (RFC 0014).
- **Scratch needs no consent** (isolated, plugin-private, auto-wiped, non-user data). The only risk
  it introduces is disk pressure, answered by the soft cap above; the RFC 0008 resource-sandbox
  philosophy (bound cpu, then memory) simply extends to *disk*.
- **Streaming makes memory feed-*independent* — that is the win, not "everyone fits in 2 MiB."**
  A plugin's *working set* over the payload becomes `O(window)`, so it no longer tracks the data
  source (no creep, no treadmill). What streaming does **not** shrink is a plugin's fixed
  **baseline** — its code's static data. The calendar dogfood proves both halves: streaming cut its
  requirement from *64 MiB-and-creeping* to a *fixed ~3 MiB*, but it doesn't fit in 2 MiB because
  `chrono-tz` embeds the whole IANA tz database (~2.5 MiB). So a `max_memory` override is no longer
  a size-tracking stopgap but a one-time, **fixed** sizing of the baseline (the calendar sets `8M`).
  The lesson for plugin authors: stream the payload (memory stops growing); size `max_memory` once
  for your library baseline if it exceeds 2 MiB. (A future option: a lighter tz path, or nudging the
  2 MiB default up — but that weakens every plugin's bound, so it stays per-plugin for now.)

## 6. Migration & impact

- **Calendar** is the dogfood: switch its fetch to `ctx.http_open` + a streaming `Slimmer`
  (in `calendar-logic`, host-unit-tested incl. a VEVENT block split across a chunk boundary). Result:
  memory drops from *64 MiB-and-creeping* to a *fixed ~3 MiB* — independent of the feed (verified at
  the default 2 MiB → traps on the chrono-tz baseline; at `4M`/`8M` → renders). It keeps a small,
  **fixed** `max_memory = "8M"` for that baseline, not a size-tracking value.
- **`max_memory`** stops being a feed-size treadmill and becomes a one-time baseline sizing. New
  plugins should stream / use `/scratch` so their *working set* never needs it; they set it only if
  their static baseline exceeds 2 MiB.
- **WIT `v0.6.0`** is additive; v0.1.0–v0.5.0 plugins keep loading unchanged via the version window.
- **SDKs**: Rust gains `ctx.http_open` (→ `impl Read`); the Go SDK mirrors it. `/scratch` needs no
  SDK change (plain `std::fs`), only documentation in the plugin-author skills.

## 7. Alternatives considered (and rejected)

- **Per-plugin `max_memory` as *the* answer.** A treadmill; sizes the cage for thrown-away data;
  permanently weakens the cap for that plugin (see §1). Fine as a stopgap, wrong as the model.
- **A host-side payload transform** (host fetches *and* slims, returns the small result). This
  special-cases the host for one data shape — effectively an "iCal capability." It is exactly the
  "bespoke host-mediated capability *per tool*" that RFC 0015 §1 calls a dead end: it doesn't
  generalise and it's plugin-author-hostile. Streaming is the generic version of the same idea.
- **A WIT `resource` for the stream.** Cleaner ownership, but heavier version-window + async codegen
  for no author-visible benefit over an SDK `Drop` (§4.1). Deferred, not dismissed.
- **Raise the global `MEM_LIMIT`.** Voids "a plugin can't OOM the bar" for *every* plugin to serve
  one — the opposite of the principle.

## 8. Status / phasing

1. **Streaming `http` (v0.6.0)** — the WIT bump + host funcs (table, blocking-service, per-plugin
   stream cap) + SDK `http_open`/`Read` + the calendar dogfood.
2. **`/scratch`** — host preopen + teardown + the soft size cap + a worked example in the skills.
3. **Calendar migration** — stream + `Slimmer`; verify the big real feed renders under the live
   reactor at a small *fixed* `max_memory` (`8M`, feed-independent) instead of the old creeping 64M.
4. **Docs** — update the plugin-author skills with the "never hold the whole thing" model.
