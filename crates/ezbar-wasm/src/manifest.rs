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
/// (`network` / `feeds` / `sway`) so the host can diff declared-vs-granted directly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub network: Vec<String>,
    pub feeds: Vec<String>,
    pub sway: bool,
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
            Some(toml::Value::Array(a)) => {
                a.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            }
            _ => Vec::new(),
        }
    };
    Some(Manifest {
        network: list("network"),
        feeds: list("feeds"),
        sway: caps.get("sway").and_then(|v| v.as_bool()).unwrap_or(false),
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

        let nested =
            parse("[capabilities]\nnetwork = [\"a\", \"b\"]\nsway = false").unwrap();
        assert_eq!(nested.network, ["a", "b"]);
        assert!(!nested.sway);
        assert!(nested.feeds.is_empty());
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
        assert_eq!(read(&[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]), None); // bare module
        assert_eq!(read(&wasm_with_section("producers", b"x")), None); // unrelated section
        assert_eq!(read(b"not wasm at all"), None); // garbage → None, no panic
    }
}
