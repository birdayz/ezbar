//! The `ezbar:manifest` capability manifest (RFC 0014 Phase A).
//!
//! A plugin may embed an `ezbar:manifest` custom section (a small TOML body) **declaring**
//! the read-only capabilities it needs. The host reads it at load and warns when a plugin's
//! *declared* needs exceed what the user actually *granted* in `[modules.<id>]` — so a
//! widget that silently does nothing because a capability is ungranted explains itself,
//! instead of failing mute. It is the basis for the registry's "grant block to paste"
//! (Phase C) and is useful with **zero** registry.
//!
//! It is a *declaration*, never an *authority*: enforcement stays the per-call host checks
//! against the (hash-keyed) grant. A manifest can only name the closed, read-only capability
//! set — it can never request more than the sandbox supports — so reading it is safe.
//!
//! The section is a top-level custom section named `ezbar:manifest` (verified to read back
//! from a real component). Producing it is Phase B: `wasm-tools` 1.251 dropped its
//! `custom-section` subcommand, so the producer tool appends the section via `wasm-encoder`
//! (or an equivalent byte-level append) — language-neutral, fed by a sidecar
//! `ezbar-plugin.toml`. Reading it is standalone-useful with no registry.

use std::path::Path;

/// The capabilities a plugin declares it needs. Mirrors the `[modules.<id>]` grant keys
/// (`network`/`feeds`/`sway`/`fs`/`exec`) so the host can diff declared-vs-granted directly.
/// `fs`/`exec` are the **dangerous tier** (RFC 0015) — a declaration of them is what `ezbar
/// inspect` flags loud and `ezbar add` refuses to silently activate.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub network: Vec<String>,
    pub feeds: Vec<String>,
    pub sway: bool,
    /// declared fs dirs (the `path` of each `fs` grant entry), e.g. `["~/notes"]`
    pub fs: Vec<String>,
    /// declared programs, e.g. `["kubectl"]`
    pub exec: Vec<String>,
}

/// The custom-section name carrying the manifest TOML.
pub const SECTION: &str = "ezbar:manifest";

/// Parse a manifest TOML body. Accepts the capabilities either at the top level or under a
/// `[capabilities]` table (the shape of the registry's `ezbar-plugin.toml`). Lenient: a
/// malformed body or unknown keys yield a best-effort/empty manifest rather than an error,
/// so a broken manifest never produces false "exceeds grant" noise.
pub fn parse(body: &str) -> Option<Manifest> {
    let doc: toml::Value = body.parse().ok()?;
    let caps = doc.get("capabilities").unwrap_or(&doc);
    let list = |k: &str| -> Vec<String> {
        match caps.get(k) {
            Some(toml::Value::String(s)) => vec![s.clone()],
            Some(toml::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        }
    };
    // `fs` is a list of `{ path, mode, at }` tables (the grant shape) — collect the paths.
    let fs = match caps.get("fs") {
        Some(toml::Value::Array(a)) => a
            .iter()
            .filter_map(|e| e.get("path").and_then(|p| p.as_str()).map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    Some(Manifest {
        network: list("network"),
        feeds: list("feeds"),
        sway: caps.get("sway").and_then(|v| v.as_bool()).unwrap_or(false),
        fs,
        exec: list("exec"),
    })
}

/// Extract and parse the `ezbar:manifest` custom section from a wasm component's bytes.
/// `None` if there is no such section (the common case today) or it doesn't parse.
pub fn read(wasm: &[u8]) -> Option<Manifest> {
    use wasmparser::{Parser, Payload};
    for payload in Parser::new(0).parse_all(wasm) {
        // A truncated/invalid binary just ends iteration — never panics the host.
        if let Ok(Payload::CustomSection(c)) = payload {
            if c.name() == SECTION {
                return parse(&String::from_utf8_lossy(c.data()));
            }
        }
    }
    None
}

/// Read a plugin file's embedded manifest, if any. Convenience over [`read`] for the host's
/// load path. `None` on any I/O error or absent section.
pub fn read_file(path: &Path) -> Option<Manifest> {
    read(&std::fs::read(path).ok()?)
}

/// Append an `ezbar:manifest` custom section carrying `toml_body` to `wasm` (the Phase B
/// producer step). A top-level custom section is valid anywhere in a module/component, so a
/// plain append works for any well-formed input (the result reads back via [`read`] and
/// validates as a component). Symmetric with [`read`]; dependency-free (no `wasm-encoder`,
/// and `wasm-tools` 1.251 no longer has `custom-section`).
pub fn inject(wasm: &[u8], toml_body: &[u8]) -> Vec<u8> {
    let name = SECTION.as_bytes();
    let mut payload = leb128(name.len()); // name length …
    payload.extend_from_slice(name); // … name …
    payload.extend_from_slice(toml_body); // … data
    let mut out = Vec::with_capacity(wasm.len() + payload.len() + 6);
    out.extend_from_slice(wasm);
    out.push(0x00); // custom section id
    out.extend_from_slice(&leb128(payload.len())); // section size
    out.extend_from_slice(&payload);
    out
}

/// Unsigned LEB128 — the wasm integer encoding for section sizes / name lengths.
fn leb128(mut n: usize) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let mut byte = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            byte |= 0x80;
        }
        b.push(byte);
        if n == 0 {
            return b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid wasm (core module: header only) with one custom section. Exercises
    /// the same `Payload::CustomSection` path a real component's top-level section takes.
    fn wasm_with_section(name: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = vec![name.len() as u8]; // name length (LEB128, names here < 128)
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);
        let mut w = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]; // \0asm v1
        w.push(0x00); // custom section id
        w.push(payload.len() as u8); // section size (LEB128, < 128 here)
        w.extend_from_slice(&payload);
        w
    }

    #[test]
    fn parses_top_level_and_capabilities_table() {
        let top = parse("network = [\"api.example.com\"]\nfeeds = \"cpu\"\nsway = true").unwrap();
        assert_eq!(top.network, ["api.example.com"]);
        assert_eq!(top.feeds, ["cpu"]); // a bare string is accepted as a one-element list
        assert!(top.sway);

        let nested = parse("[capabilities]\nnetwork = [\"a\", \"b\"]\nsway = false").unwrap();
        assert_eq!(nested.network, ["a", "b"]);
        assert!(!nested.sway);
        assert!(nested.feeds.is_empty());
    }

    #[test]
    fn parses_the_dangerous_tier_fs_and_exec() {
        let m = parse(
            "[capabilities]\nexec = [\"kubectl\", \"git\"]\n\
             fs = [{ path = \"~/.kube\", mode = \"r\" }, { path = \"~/notes\", mode = \"rw\" }]",
        )
        .unwrap();
        assert_eq!(m.exec, ["kubectl", "git"]);
        assert_eq!(m.fs, ["~/.kube", "~/notes"]); // the paths of each fs grant entry
    }

    #[test]
    fn malformed_or_empty_is_forgiving() {
        assert_eq!(parse("not = = toml"), None); // unparseable → None (no false warnings)
        assert_eq!(parse(""), Some(Manifest::default())); // empty TOML → declares nothing
    }

    #[test]
    fn reads_the_section_from_wasm_bytes() {
        let wasm = wasm_with_section(SECTION, b"network = [\"api.open-meteo.com\"]\nsway = true");
        let m = read(&wasm).expect("manifest section found + parsed");
        assert_eq!(m.network, ["api.open-meteo.com"]);
        assert!(m.sway);
    }

    #[test]
    fn no_section_or_other_section_is_none() {
        assert_eq!(
            read(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
            None
        ); // bare module
        assert_eq!(read(&wasm_with_section("producers", b"x")), None); // unrelated section
        assert_eq!(read(b"not wasm at all"), None); // garbage → None, no panic
    }

    #[test]
    fn inject_then_read_round_trips() {
        let base = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]; // bare module
        let out = inject(&base, b"network = [\"api.x.com\"]\nsway = true");
        let m = read(&out).expect("injected manifest reads back");
        assert_eq!(m.network, ["api.x.com"]);
        assert!(m.sway);
    }

    #[test]
    fn inject_handles_multibyte_leb128_sizes() {
        // a body > 127 bytes forces a 2-byte LEB128 section size — make sure framing still
        // parses (a naive single-byte size would corrupt the section).
        let host = "h".repeat(200);
        let body = format!("network = [\"{host}\"]");
        let out = inject(
            &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00],
            body.as_bytes(),
        );
        assert_eq!(read(&out).unwrap().network, [host]);
    }

    #[test]
    fn leb128_encoding() {
        assert_eq!(leb128(0), [0x00]);
        assert_eq!(leb128(127), [0x7f]);
        assert_eq!(leb128(128), [0x80, 0x01]);
        assert_eq!(leb128(300), [0xac, 0x02]);
    }
}
