# RFC 0014: plugin registry, `ezbar install`, and the capability manifest

- **Status:** **Accepted** (v2) — ACK'd by a security/supply-chain reviewer and a CLI/ecosystem
  reviewer, both *conditional on* the fold-ins below (v1 took a NAK on the same root: grants are
  **id-keyed**, so the "informed consent" was a name the attacker picks). Cleared for
  implementation **in phases** (§8), Phase A first.
- **Created:** 2026-06-04
- **Target:** ezbar (the `ezbar` CLI + host plugin discovery + the guest SDKs + a `cargo ezbar`
  packaging tool)
- **Depends on:** RFC 0006 (sandbox + the **hash-keyed grant** §5 promised — which the current
  code does NOT do, §3), RFC 0012/0013 (the capabilities a manifest declares), RFC 0013's
  version window (install must negotiate the WIT version).

## What changed in v2 (review fold-ins)

Both reviewers ACK'd the *direction* (prebuilt + checksum + install-time capability prompt) and
converged on the same corrections. Two of them exposed **real gaps in shipped code**, not just
the RFC:

1. **Grants are id-keyed → confused-deputy. FIX: hash-keyed consent, runtime reads the embedded
   manifest (§3).** The grant key is the *config key* is the plugin *id* is the *.wasm stem*
   (`modules/mod.rs` → `[modules.<id>]`); the runtime never reads any embedded manifest. So a
   *different* `weather.wasm` dropped in the plugins dir (or an `install` of a malicious
   `weather`, or a file swap) **inherits the existing config grant with no re-prompt** — a
   classic confused-deputy. This silently abandons RFC 0006 §5's promise ("grants keyed by
   `hash(wasm ‖ manifest)` … can't swap a benign manifest under a granted hash to escalate").
   v2 restores it: **consent + the recorded grant are keyed on `hash(wasm ‖ manifest)`, and the
   host reads the embedded manifest at load and refuses to run a plugin whose declared caps
   exceed its grant.** Re-consent on **any** artifact/cap change (install, update, *or a manual
   drop-in*). (Trigger, corrected: the config-dir watcher is `NonRecursive`, so a *bare* drop into
   the `plugins/` subdir doesn't auto-reload — the swap is picked up on the next config reload /
   `ezbar msg reload` / restart, and a brand-new id needs a restart (`PLUGINS` is a startup
   `OnceLock`). Real on any reload/restart; not literally "live on mtime".) **This was filed as a
   standalone security TODO against the current code, independent of the registry — and its core is
   now SHIPPED:** see the Phase A status note in §8.
2. **No unsigned auto-grant. Print-the-block, don't auto-write config (v1).** Checksum is
   *integrity*, not *authenticity* — a malicious publisher is trusted forever, and a compromised
   registry repo rewrites index + checksum + the embedded section atomically (so the
   "embedded-manifest == index" check is theater against a real adversary; keep it only as a
   *drift* check, don't sell it as tamper-evidence). v1: **TOFU-pin the publisher per id at first
   install**; and **`install` PRINTS the `[modules.<id>]` grant block for the user to paste** —
   it does **not** mutate `config.toml`. (`config.rs` is a read-only resolution pipeline —
   presets/`$palette`/deep-merge — that `toml_edit` doesn't understand; the grant target is
   ambiguous, inline-placement-spec vs. a `[modules.<id>]` table; and `load_result` is
   keep-last-good, so a mangled write *survives silently*. If auto-write ever lands, it MUST copy
   `src/install.rs`'s append-only + atomic + `.bak` + pure-tested pattern — never `toml_edit`
   surgery.)
3. **The `ezbar:api-version` "custom section" doesn't exist — emitting a manifest is net-new.**
   `plugin_version()` detects the WIT version by **import introspection** (`ezbar:plugin/host@x.y`),
   not a custom section. So `ezbar:manifest` is net-new machinery on the guest (emit) and host
   (parse, `wasmparser`). Multi-language parity forces the design: a Rust `#[link_section]` macro
   has **no TinyGo equivalent**, so manifest emission is a **shared post-compile `wasm-tools
   custom-section` step**, fed by a **sidecar `ezbar-plugin.toml`** (language-neutral, the same
   bytes pasted into the registry) — NOT `export_plugin!` macro args.
4. **Per-plugin versioned index files, not a flat `index.toml` (§4).** A monolith conflicts on
   every concurrent publish PR, is unreviewable, and carries one `wit`/`artifact` per plugin — so
   the first time a popular plugin bumps WIT past the user's ezbar, `install` just errors with no
   compatible fallback. `plugins/<id>/<version>.toml` (each with its own `wit`+`sha256`+caps) lets
   `install` pick the **newest entry within the host's WIT window** and makes `update`/pin/`--dry-run`
   additive. (Cargo's index is per-crate git files for exactly these reasons.)
5. **Ship the manifest reader FIRST (§8 Phase A), standalone.** The host reading `ezbar:manifest`
   + warning on declared-vs-granted mismatch is useful with zero registry, and it forces building
   the `wasmparser` reader + the shared emit step + the hash-keyed grant — the hard, risky core.
   The registry/install is then a thin layer.
6. **The producer gap is the real bottleneck — `cargo ezbar package`.** A shared, post-compile
   tool that emits `{id}.wasm` + `sha256` + injects the `ezbar:manifest` from `ezbar-plugin.toml`
   + prints the `plugins/<id>/<v>.toml` to commit. Without it, the manifest/checksum/index/section
   are hand-kept-in-sync across four places and the registry rots with drift on day one.

## 1. Problem

The platform is **done** (RFC 0009–0013) but there's **no ecosystem**: plugins are built by hand
and *copied* into `~/.config/ezbar/plugins/`, and the capability grant is *hand-edited* into
`config.toml`. No discovery, no install, no update — and the security gap: a `.wasm` dropped in
the dir **just loads**, with no moment where the user sees what it will be allowed to do. Worse
(§3), a granted id can be re-bound to a different binary. The north-star: *"Plugin registry +
`ezbar install` with a capability manifest the user approves on install."*

## 2. Goal & non-goals

**Goal.** `ezbar install weather` fetches a plugin, **shows exactly which capabilities it
requests**, gets explicit consent **bound to that exact binary** (`hash(wasm ‖ manifest)`),
verifies the artifact, installs it, and **prints the grant block to paste** — so the security
decision is explicit, informed, and *can't be inherited by a swapped binary*.

**Non-goals / deferred:** running build toolchains on install by default (`--from-source` is
best-effort, may fail on the TinyGo/Go skew already in-tree); a hosted server/web UI; cryptographic
**signatures** + a trust root (TOFU publisher-pin first, sigstore/minisign fast-follow); an
auto-update daemon; private registries.

## 3. The security core — declare / consent / enforce, keyed on the hash

Three layers, and the binding between them is the **content hash**, not the id:

- **Declare:** the plugin's `ezbar:manifest` custom section lists its requested capabilities.
- **Consent:** `install` (or first load) shows the manifest and records approval keyed on
  `hash(wasm ‖ manifest)` — in a `~/.config/ezbar/grants.toml` (host-owned, hash→caps), separate
  from the user-owned `config.toml`.
- **Enforce (the real gate, mostly already built):** at load, the host reads the embedded
  manifest, computes the hash, and **refuses to run** if (a) there's no recorded consent for this
  hash, or (b) the declared caps exceed the consented caps. The existing per-call checks
  (`http_get` allow-list, `granted_feeds`, `granted_sway`, `read_file` hard-deny) stay — but they
  now read from the **hash-keyed consent**, not the id-keyed config table. A swapped binary has a
  new hash ⇒ no consent ⇒ refused (+ "weather changed — re-approve with `ezbar install`").

The capability **set is closed and all read-only** (verified against the host): `network`
(allow-listed GET only), `feeds` (read-only metrics), `sway` (read-only snapshot), `read-file`
(path-scoped, currently hard-denied). There is **no `exec`/POST/write-sway/arbitrary-fs** to
request — the registry cannot introduce a grant more dangerous than the sandbox already supports.

## 4. The registry — per-plugin versioned files in a git repo

`github.com/birdayz/ezbar-plugins`, `plugins/<id>/<version>.toml`:

```toml
# plugins/weather/1.2.0.toml
id = "weather"; name = "Weather"; version = "1.2.0"
description = "Forecast chip with an hourly/daily hover panel."
wit = "0.2.0"                       # WIT version → host-window negotiation
publisher = "birdayz"               # TOFU-pinned per id at first install
artifact = "https://github.com/.../releases/download/v1.2.0/weather.wasm"
sha256 = "…"                        # pins artifact ↔ this entry
capabilities = { network = ["api.open-meteo.com","wttr.in"], feeds = [], sway = false }
```

`ezbar` fetches the small per-plugin file(s) over HTTPS (registry URL is a setting). PRs publish;
per-plugin files never conflict and are trivially auditable. `install` picks the **newest version
whose `wit` is in the host's supported window**.

## 5. The CLI

```
ezbar install <id>[@version]   # fetch entry → show manifest → confirm → download+verify → install + PRINT grant block
ezbar install <id> --dry-run   # show manifest + the exact grant block; write nothing  (== the print path)
ezbar install <id> --from-source   # clone+build (best-effort; needs the wasm toolchain — may fail)
ezbar update [<id>]            # install the newest in-window version; re-prompt if caps changed
ezbar list                     # installed plugins + their consented caps
ezbar remove <id>              # delete the .wasm; PRINT "you may want to remove [modules.<id>]" (don't touch config we didn't write)
ezbar search <term>
```

**`install` flow:** fetch entry → **WIT-window gate** (refuse + "upgrade ezbar" if out of window)
→ TOFU-check the `publisher` against the pinned key for this id (refuse on mismatch: "publisher
changed") → **show manifest + caps**, `[y/N]` → download, **verify `sha256`**, **verify embedded
`ezbar:manifest` == entry** (drift check) → compute `hash(wasm ‖ manifest)`, record consent in
`grants.toml` → write `~/.config/ezbar/plugins/<id>.wasm` → **PRINT the `[modules.<id>]` block to
paste** (capabilities the host will read from `grants.toml`; placement is the user's choice). The
bar's file-watch hot-loads it (and now checks consent by hash, §3).

## 6. Trust & safety
- **Consent bound to the binary** (`hash(wasm ‖ manifest)`) — closes the id-confused-deputy.
- **TOFU publisher-pin** kills silent publisher-swap on update without a full PKI; signatures are
  the fast-follow.
- **`sha256`** pins artifact↔entry (a dumb mirror swap is caught; a compromised repo is not — be
  honest about that; it's why TOFU + read-only-only matters).
- **User owns `config.toml`:** install *prints*, never writes it; `remove` never deletes config
  it didn't author.
- **Read-only capability set** — the registry can't escalate past the sandbox.

## 7. The producer path — `cargo ezbar package` (the DX linchpin)
One language-neutral, post-compile tool so the manifest, checksum, embedded section, and index
entry all **derive from one source** (`ezbar-plugin.toml`), instead of being hand-synced:
```
cargo ezbar package            # build → wasm-tools inject ezbar:manifest from ezbar-plugin.toml
                               # → emit {id}.wasm + sha256 + the plugins/<id>/<version>.toml to commit
```
Works for Rust and TinyGo (the inject step is `wasm-tools custom-section`, language-agnostic). The
§5 "embedded == entry" check is then a *safety net* against author error, not the primary defense.

## 8. Phasing
- **Phase A — manifest reader + hash-keyed grants (standalone, ships value + de-risks the core).**
  `ezbar-plugin.toml` + the `wasm-tools` emit step; the host parses `ezbar:manifest` at discovery;
  **move enforcement from id-keyed config to `hash(wasm ‖ manifest)` consent** (fixes the current
  confused-deputy) with a `grants.toml` + a load-time refuse-and-explain. Warn on declared-vs-
  consented mismatch. No registry yet. **This is the load-bearing, security-relevant PR.**
  - **Status — Phase A SHIPPED.** (1) The hash-keyed grant core (the confused-deputy is closed):
    `src/grants.rs` keys consent on the artifact **content hash** in a host-owned `grants.toml`
    (`id -> sha256(wasm)`), TOFU on first sight, **withholds every capability** on a hash mismatch
    (plugin still runs sandboxed), with `ezbar grant <id>` for explicit re-consent; `build()` gates
    the grant args through `grants::decide()`, the reactor is untouched (it enforces what it's
    handed). (2) The **`ezbar:manifest` reader** (`ezbar_wasm::manifest`): the host parses a
    plugin's optional top-level `ezbar:manifest` custom section (a declaration of needed
    network/feeds/sway) and **warns on declared-but-ungranted** caps so an inert widget explains
    itself — verified end-to-end against a real component. **Deferred to Phase B (coupled to the
    producer tool):** the domain-separated `hash(wasm ‖ manifest)` key and "declared ≤ consented"
    *enforcement* (vs. today's warning) — both need the manifest **emitted** into real plugins.
    Note: `wasm-tools` 1.251 dropped its `custom-section` subcommand, so the producer (§7) appends
    the section via `wasm-encoder` rather than the CLI the RFC first assumed. (3) **`ezbar inspect
    <plugin.wasm>`** delivers the install flow's **"print the grant block"** half (§5) standalone,
    with zero registry: it reads the embedded manifest and prints the sha256 + the `[modules.<id>]`
    block to paste (never auto-writing config) + points at `ezbar grant`. So the local lifecycle —
    **package** (author) → **inspect** (see/paste grant) → **grant** (consent to the bytes) — is
    complete; Phase C adds only the network fetch + the registry index on top.
- **Phase B — `cargo ezbar package`** (§7): the producer tool.
- **Phase C — the registry + `ezbar install`/`list`/`remove`/`search`/`update`** (§4/§5): the thin
  consumer layer, TOFU pin, print-the-block, WIT-window version negotiation.

## 9. Open questions — resolved
1. Manifest transport → embedded section (load-time enforce) **and** index entry (pre-download
   display); equality is a *drift* check, not tamper-evidence. Resolved.
2. Prebuilt vs source → **prebuilt default**; `--from-source` best-effort/may-fail. Resolved.
3. Signing → **TOFU publisher-pin in v1**, signatures fast-follow; no unsigned *auto-grant* (we
   print, don't auto-write). Resolved.
4. Auto-write vs print → **print the block** (v1); any future auto-write copies `install.rs`'s
   append-only/atomic/backup/pure-tested model, never `toml_edit`. Resolved.
5. UX → **CLI first**; a bar "plugin browser" popup is a later surface. Resolved.
6. Config writing → moot in v1 (print). Resolved.
