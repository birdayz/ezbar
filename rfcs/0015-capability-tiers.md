# RFC 0015: capability tiers — `fs`/`exec` + a global `yolo`

- **Status:** **Implemented** — `fs`, `yolo`, and `exec` (the `v0.3.0` WIT) all shipped.
- **Created:** 2026-06-04
- **Target:** ezbar (the WASM host `crates/ezbar-wasm`, the bar `src/`, the guest SDKs, the WIT).
- **Depends on:** RFC 0006 (sandbox + the capability model), RFC 0012/0013 (the read-only caps),
  RFC 0014 (the hash-keyed consent + manifest these grants flow through).

## 1. Problem

RFC 0012/0013 gave plugins a **read-only** capability set (`network` GET allow-list, `feeds`,
`sway` snapshot). That was deliberately conservative — but it makes a whole class of useful
widgets impossible: anything that reads a config/notes file, or shells out to a tool
(`kubectl`, `git`, `docker`, `systemctl`, …). The earlier instinct — a bespoke host-mediated
capability *per tool* (a `kube` cap, a `git` cap) — is a dead end: it doesn't generalise and
it's plugin-author-hostile. The generic answer is two capabilities — **`fs`** and **`exec`** —
plus a way to make them usable without making them a footgun.

## 2. The model — fine-grained default-deny, plus a global `yolo`

Mirrors Claude Code / Codex permission modes:

- **Default: fine-grained, default-deny.** Each capability is granted per-plugin in
  `[modules.<id>]`, scoped:
  - `network = ["api.example.com", "*"]` — host allow-list (`"*"` = any).
  - `feeds = ["cpu", "*"]` — host-sampled metrics.
  - `sway = true` — read-only sway snapshot.
  - `fs = [{ path = "~/notes", at = "/notes", mode = "rw" }]` — preopened dirs (`mode`: `r`|`rw`).
  - `exec = ["kubectl", "git"]` — program allow-list (any args).
- **`[plugins] yolo = true`** — the global "I trust my plugins, stop asking" switch. Every
  plugin gets the **full** set (any host, `/` read-write, all feeds, sway, any program),
  bypassing the per-module grants *and* the hash-consent.

**What `yolo` does and doesn't void.** It voids the *capability gates* — and, honestly, the
RFC 0014 invariant that "the registry can't grant anything more dangerous than the sandbox."
That's the point: yolo is the user saying they trust their plugins like they trust a shell
script (which ezbar already runs via the `custom` module). It does **not** void the **resource
sandbox** — cpu (epoch), memory (`MEM_LIMIT`), and the wall-clock timeout still hold, so a yolo
plugin can read your files but still can't hang or OOM the bar. That's strictly better than a
native shell-script bar.

## 3. `fs` — WASI preopens (Implemented)

We already link `wasi:filesystem`; we just built an empty `WasiCtx`. The `fs` grant maps
straight to `WasiCtxBuilder::preopened_dir(host, guest, DirPerms, FilePerms)`, so the guest
uses **normal `std::fs`** and **WASI enforces the jail** — no ambient authority, no `..`/symlink
escape, write only where `mode = "rw"`. We do not hand-roll path scoping; the runtime does it.
Default-deny: no grant ⇒ no filesystem. (Verified e2e: granted read works; ungranted →
"No such file or directory"; `../../etc/hostname` → "Operation not permitted".)

## 4. `exec` — a host function + a program allow-list (designed)

WASI p2 has no process spawn, so `exec` is a host import → the **first `v0.3.0` WIT bump**
(additive; the frozen-version-window infra from RFC 0013 already co-loads it — add a `mod v3`
bindgen remapping `types`/`ui`/`events` to v0.1.0, a `linker_v3`, `DrivenPlugin::V3`, and
extend `plugin_version` to detect `host@0.3`; the v3 host impl delegates to v2 except `exec`).

```wit
record exec-out { code: s32, stdout: list<u8>, stderr: list<u8> }
exec: func(program: string, args: list<string>, stdin: option<list<u8>>) -> result<exec-out, string>;
// streaming form (long-running, mirrors custom's listen_cmd) — a fast-follow, not v1.
```

Gate: `Host { granted_exec: Vec<String> }`; the host checks `program ∈ granted_exec` (`"*"` =
any), then `Command::new(program).args(args)` on the blocking pool. The program allow-list is
**transparency + a speed-bump, not a jail** — `kubectl`/`git` can themselves exec, so
`exec = ["kubectl"]` honestly means "I trust this plugin like a shell script that runs kubectl."
That's fine for the *fine-grained, user-granted, hash-pinned* path; it must never be reachable
from a silent registry `add` (see §5).

## 5. Keeping it safe — the dangerous tier (the one structural rule)

`fs`-write and `exec` are a **dangerous tier**. The rule isn't "don't have them" — it's "a
*fetched* plugin never gets them silently":

- `ezbar inspect` flags `fs`/`exec` declarations **prominently** (they're not read-only).
- `ezbar add` installs the `.wasm` but **does not auto-activate** dangerous caps — it prints the
  grant block to paste and warns; the user opts in by hand (the RFC 0014 consent UX already does
  print-don't-write).
- `yolo` is the deliberate global override; without it, dangerous caps are default-deny.

The reader/consent machinery (RFC 0014: manifest declares → `inspect` shows → hash-pinned
`grant`) extends to `fs`/`exec` by adding fields to `Manifest` + `grant_block` — no new concepts.

## 6. Status / phasing

- **`fs` (WASI preopens) — DONE.** `[modules.<id>].fs`, `FsGrant`, `build_wasi`; the `fstest`
  example plugin; `preview --fs`.
- **`yolo` — DONE.** `[plugins] yolo`, `"*"` wildcards (network/feeds), `modules::set_yolo`.
- **`exec` — DONE.** The `v0.3.0` WIT (`wit/since-v0.3.0`) + the gated host fn + `ctx.exec` in
  the SDK + the program allow-list; the version-window now carries v1/v2/v3. The `kube` example
  plugin (kubectl context) is the dogfood. Verified e2e (detect/deny/grant/stdout) + v0.1.0
  still loads.
- **manifest/inspect surface for `fs`/`exec` — DONE.** `Manifest` parses declared `fs`/`exec`;
  `ezbar inspect` shows them flagged (`⚠ exec: kubectl`), `grant_block` emits a `# DANGEROUS`
  block, and the host's declared-vs-granted warning covers them. RFC 0015 fully implemented.
