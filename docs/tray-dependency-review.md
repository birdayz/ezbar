# Tray dependency review — 2026-09-19

Baseline: `3ff2b3e047378482cc890d326eed6f79b128078c` (v0.1.17).
This is a scoped review, **not a claim that the entire dependency graph is safe**.
Registry checksums establish archive integrity against the lockfile, not author
trustworthiness or absence of malicious code.

## Dependency delta

The tray adds exact direct references to packages already locked at these versions:

| Package | Version | Reason |
| --- | --- | --- |
| zbus | 5.16.0 | StatusNotifier watcher/host and dbusmenu; Tokio backend |
| x11rb | 0.13.2 | Legacy XEmbed; Composite, Damage and XFixes extensions |
| png | 0.17.16 | Bounded PNG decoding without enabling an image-codec bundle |
| resvg | 0.45.1 | Existing iced SVG renderer, now used with image resolution disabled |

No new package names are added. The only registry version change is
`event-listener 5.4.1 → 5.4.2`, the narrow fix for RUSTSEC-2026-0221 (incorrect
Send/Sync bounds on event tags). Its `concurrent-queue` edge disappears. Other
lockfile changes are root direct-dependency edges and zbus/Tokio feature edges
(`zbus → tokio`, `tokio → tracing`); no unrelated packages were upgraded.

Downloaded archives were SHA-256 checked against the lockfile for `arrayref 0.3.9`,
`png 0.17.16`, `x11rb`/`x11rb-protocol 0.13.2`, `zbus`/`zbus_macros 5.16.0`,
`zbus_names 4.3.2`, `zvariant`/`zvariant_derive 5.12.0`, `zvariant_utils 3.4.0`,
`resvg 0.45.1`, and the replacement `event-listener 5.4.2`. None of these archives
contains a build.rs. Manifests were inspected; the new event-listener fix and
SVG image-resolution behavior were checked in source. Targeted source searches
of the zbus/zvariant macro crates found no process/network execution patterns;
this is not an exhaustive macro-code audit.

`event-listener 5.4.2` archive SHA-256:
`5a23add41df1562121a9393cb065eab5146a1242410f23a644851e90cfd669d2`.

Builds used official Arch Rust/Cargo 1.98.1 and a fresh registry cache, never code
or caches from the recovered disk. Compilation/tests ran with `--frozen` inside
bubblewrap without network, credentials, the recovered disk, or the real desktop.
Only the private X11 integration test was given its disposable X server socket.
This limits build-time exposure; it does not sandbox the installed application.

## Known malicious versions: block before building

`scripts/check-lock.py` uses Python 3.11+ stdlib and runs before Cargo in CI and
release builds. `deny.toml` has matching bans:

- `arrayref 0.3.10` (the locked version remains 0.3.9)
- every version of `proc-macro1` (malicious lookalike, not `proc-macro2`)
- `internment 0.8.7`
- `append-only-vec 0.1.9`

Run locally before builds:

```sh
python3 scripts/check-lock.py --self-test
python3 scripts/check-lock.py
cargo build --locked --release
```

CI/release compilation now uses `--locked`. The guard tests malicious fixtures
without downloading or compiling them. It complements, rather than replaces,
source review, trusted caches, advisory checks and sandboxed builds. Unknown
malicious releases will not be detected by this small denylist.

## Outstanding pre-existing advisories

OSV batch queries scanned all 704 registry packages before and after the change.
The event-listener finding is gone. The following nine flagged packages remain
from the baseline; they have not been silently suppressed or called clean:

| Locked package | Finding(s) |
| --- | --- |
| lru 0.16.4 | RUSTSEC-2026-0253: use-after-free during panic; fix 0.18.2 |
| memmap2 0.9.10 | RUSTSEC-2026-0186: unchecked-offset UB; fix 0.9.11 |
| quick-xml 0.39.4 | RUSTSEC-2026-0194/0195: attribute/namespace resource exhaustion; fix 0.41 |
| quinn-proto 0.11.14 | RUSTSEC-2026-0185 / GHSA-4w2j-m93h-cj5j: memory exhaustion; fix 0.11.15 |
| rustls 0.23.40 | RUSTSEC-2026-0285: TLS boundary checks; fix 0.23.45 |
| wasmtime 46.0.2 | RUSTSEC-2026-0268/0269: WASIp3 allocation and filesystem sandbox escape; fix 46.0.3 |
| paste 1.0.15 | RUSTSEC-2024-0436: unmaintained (existing cargo-deny exception) |
| rustybuzz 0.20.1 | RUSTSEC-2026-0206: unmaintained |
| ttf-parser 0.25.1 | RUSTSEC-2026-0192: unmaintained |

Affected APIs/features and upgrade compatibility need a separate review; package
presence alone does not establish exploitability. In particular, do not treat the
WASM sandbox as fully patched merely because this tray implementation is native.
The existing advisory CI gate may fail until these findings are addressed.

## Build verification caveat

The workspace tests, doctests, private D-Bus/Xwayland tests and release build
passed locally. Strict Clippy on Rust 1.98 stops at the pre-existing
`clippy::drain_collect` warning in `crates/ezbar-wasm/src/lib.rs:1254`. Running the
full workspace/all-targets check with only that lint allowed passed. The existing
code and CI warning policy were left unchanged rather than hiding the finding.
