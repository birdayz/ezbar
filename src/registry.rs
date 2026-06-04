//! Registry index entries + WIT-window version negotiation (RFC 0014 Phase C — the pure
//! core). `ezbar add <id>` resolves a plugin id to a concrete artifact by reading per-plugin
//! versioned index files `plugins/<id>/<version>.toml` and picking the **newest version
//! whose WIT is in this host's supported window** (RFC 0013's frozen-version window). The
//! fetch transport — a local directory today, HTTPS/git later — is a thin wrapper over this.
//!
//! (Plugin install is `ezbar add`, not `ezbar install`: the latter already means "add ezbar
//! to your sway config".)

use std::cmp::Ordering;
use std::path::Path;

/// One `plugins/<id>/<version>.toml` index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub version: String,
    pub wit: String,
    pub sha256: String,
    pub artifact: String,
    pub publisher: String,
    pub description: String,
}

/// The WIT versions this host can load — the frozen-version window (RFC 0006 §4 / 0013).
/// An entry whose `wit` is outside this can't be loaded, so `add` skips it (and errors
/// "upgrade ezbar" if that leaves nothing).
pub const SUPPORTED_WIT: &[&str] = &["0.1.0", "0.2.0"];

/// Parse a `plugins/<id>/<version>.toml`. `id`, `version`, `sha256` are required (an entry
/// that can't pin its artifact is useless); `wit` defaults to the first frozen version.
pub fn parse_entry(toml_body: &str) -> Result<Entry, String> {
    let doc: toml::Value = toml_body.parse().map_err(|e| format!("registry entry: {e}"))?;
    let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let req = |k: &str| s(k).ok_or_else(|| format!("registry entry: missing `{k}`"));
    Ok(Entry {
        id: req("id")?,
        version: req("version")?,
        sha256: req("sha256")?,
        wit: s("wit").unwrap_or_else(|| "0.1.0".to_string()),
        artifact: s("artifact").unwrap_or_default(),
        publisher: s("publisher").unwrap_or_default(),
        description: s("description").unwrap_or_default(),
    })
}

/// Pick the newest entry whose `wit` is in `supported`. `None` when none are in-window — the
/// caller reports "no compatible version; upgrade ezbar". "Newest" is the highest dotted
/// numeric version, so `1.10.0` correctly beats `1.9.0` (not a lexical compare).
pub fn pick_in_window<'a>(entries: &'a [Entry], supported: &[&str]) -> Option<&'a Entry> {
    entries
        .iter()
        .filter(|e| supported.contains(&e.wit.as_str()))
        .max_by(|a, b| cmp_version(&a.version, &b.version))
}

/// Compare dotted numeric versions component-wise (`1.2.0` vs `1.10.0`). Non-numeric
/// components sort as 0 — good enough for the registry's semver-ish strings without a dep.
fn cmp_version(a: &str, b: &str) -> Ordering {
    let parts = |s: &str| {
        s.split('.')
            .map(|x| x.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(a).cmp(&parts(b))
}

/// Does `bytes` hash to `expected` (the entry's `sha256`)? Case-insensitive hex compare.
/// This is the integrity check between the index entry and the downloaded artifact.
pub fn verify_sha256(bytes: &[u8], expected: &str) -> bool {
    ezbar::grants::sha256_hex(bytes).eq_ignore_ascii_case(expected.trim())
}

/// `ezbar add <id> --registry <dir>` — install `<id>` from a **local** registry directory
/// (the network/git transport is a later wrapper over the same core). Layout:
/// `<registry>/plugins/<id>/<version>.toml` index files + a co-located `<version>.wasm`
/// artifact. Resolves the newest in-WIT-window version, verifies its `sha256`, installs it
/// to the plugins dir, and prints the grant block — **never** touching `config.toml`. Returns
/// a human-facing summary. (TOFU publisher-pin is deferred; a local dir is already trusted.)
pub fn add(id: &str, registry: &Path) -> Result<String, String> {
    let dir = registry.join("plugins").join(id);
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| format!("no plugin '{id}' in registry {}: {e}", registry.display()))?;
    let entries: Vec<Entry> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|body| parse_entry(&body).ok())
        .collect();
    if entries.is_empty() {
        return Err(format!("no index entries for '{id}' under {}", dir.display()));
    }
    let picked = pick_in_window(&entries, SUPPORTED_WIT).ok_or_else(|| {
        format!("no version of '{id}' supports this ezbar's WIT window {SUPPORTED_WIT:?} — upgrade ezbar")
    })?;

    let artifact = dir.join(format!("{}.wasm", picked.version));
    let bytes = std::fs::read(&artifact)
        .map_err(|e| format!("read artifact {}: {e}", artifact.display()))?;
    if !verify_sha256(&bytes, &picked.sha256) {
        return Err(format!(
            "sha256 mismatch for '{id}' {} — refusing (corrupt or tampered artifact)",
            picked.version
        ));
    }

    let plugins = ezbar::config::plugins_dir().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
    std::fs::create_dir_all(&plugins).map_err(|e| format!("mkdir {}: {e}", plugins.display()))?;
    let dest = plugins.join(format!("{id}.wasm"));
    std::fs::write(&dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))?;

    let publisher = if picked.publisher.is_empty() {
        "unsigned".to_string()
    } else {
        format!("publisher {}", picked.publisher)
    };
    // Print what to grant, from the just-installed artifact's embedded manifest (RFC 0014:
    // print, never auto-write). Falls back gracefully if it carries no manifest.
    let grants = ezbar::grants::inspect(&dest, id).unwrap_or_default();
    Ok(format!(
        "installed '{id}' {} ({publisher}) → {}\n\n{grants}",
        picked.version,
        dest.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(version: &str, wit: &str) -> Entry {
        Entry {
            id: "weather".into(),
            version: version.into(),
            wit: wit.into(),
            sha256: "deadbeef".into(),
            artifact: String::new(),
            publisher: String::new(),
            description: String::new(),
        }
    }

    #[test]
    fn parse_entry_requires_id_version_sha() {
        let e = parse_entry(
            "id = \"weather\"\nversion = \"1.2.0\"\nwit = \"0.2.0\"\nsha256 = \"abc\"\n\
             publisher = \"birdayz\"",
        )
        .unwrap();
        assert_eq!(e.id, "weather");
        assert_eq!(e.version, "1.2.0");
        assert_eq!(e.wit, "0.2.0");
        assert_eq!(e.sha256, "abc");
        assert_eq!(e.publisher, "birdayz");
        // missing required fields → error, not a partial entry
        assert!(parse_entry("id = \"x\"\nversion = \"1.0.0\"").is_err()); // no sha256
        assert!(parse_entry("version = \"1.0.0\"\nsha256 = \"a\"").is_err()); // no id
    }

    #[test]
    fn picks_newest_version_within_the_wit_window() {
        let entries = vec![entry("1.0.0", "0.1.0"), entry("1.10.0", "0.2.0"), entry("1.9.0", "0.1.0")];
        // numeric compare: 1.10.0 wins over 1.9.0 (both in window)
        assert_eq!(pick_in_window(&entries, SUPPORTED_WIT).unwrap().version, "1.10.0");
    }

    #[test]
    fn skips_out_of_window_and_errors_when_all_too_new() {
        // newest is 2.0.0 but its wit (0.3.0) is beyond the host window → fall back to 1.5.0
        let entries = vec![entry("1.5.0", "0.2.0"), entry("2.0.0", "0.3.0")];
        assert_eq!(pick_in_window(&entries, SUPPORTED_WIT).unwrap().version, "1.5.0");
        // everything out of window → None (caller: "upgrade ezbar")
        let all_new = vec![entry("2.0.0", "0.3.0"), entry("3.0.0", "0.4.0")];
        assert!(pick_in_window(&all_new, SUPPORTED_WIT).is_none());
    }

    #[test]
    fn verify_sha256_matches_case_insensitively() {
        let h = ezbar::grants::sha256_hex(b"artifact-bytes");
        assert!(verify_sha256(b"artifact-bytes", &h));
        assert!(verify_sha256(b"artifact-bytes", &h.to_uppercase())); // hex case-insensitive
        assert!(!verify_sha256(b"tampered", &h)); // a mismatch is caught
    }
}
