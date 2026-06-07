//! `ezbar package` — the Phase B producer (RFC 0014 §7).
//!
//! Takes an already-built plugin `.wasm` plus a sidecar `ezbar-plugin.toml`, embeds it as the
//! `ezbar:manifest` custom section, and prints the `plugins/<id>/<version>.toml` registry
//! entry — with the `sha256` taken over the *shipped* artifact (the one carrying the
//! manifest). Building the wasm stays the author's job (cargo / tinygo); this is the
//! language-neutral post-compile step, so the embedded manifest, the checksum, and the index
//! entry all derive from **one** source instead of being hand-synced across four places.
//!
//! ```text
//! ezbar package weather.wasm                 # sidecar: ./ezbar-plugin.toml, writes in place
//! ezbar package weather.wasm meta.toml -o dist/weather.wasm
//! ```

use std::path::{Path, PathBuf};

/// The author-facing fields of `ezbar-plugin.toml` the registry entry needs. Capabilities
/// are carried verbatim (the whole sidecar is embedded as the manifest), so they aren't
/// re-modelled here.
struct Meta {
    id: String,
    name: String,
    version: String,
    wit: String,
    description: String,
}

/// Run the packager. `sidecar` defaults to `ezbar-plugin.toml` beside the wasm; `out`
/// defaults to overwriting the input. Returns the registry-entry text to print on success.
pub fn run(wasm: &Path, sidecar: Option<&Path>, out: Option<&Path>) -> Result<String, String> {
    let sidecar = sidecar
        .map(PathBuf::from)
        .unwrap_or_else(|| wasm.with_file_name("ezbar-plugin.toml"));
    let sidecar_bytes = std::fs::read(&sidecar).map_err(|e| {
        format!(
            "read {}: {e} (write an ezbar-plugin.toml, RFC 0014 §4)",
            sidecar.display()
        )
    })?;
    let meta = parse_sidecar(&String::from_utf8_lossy(&sidecar_bytes))?;

    let wasm_bytes = std::fs::read(wasm).map_err(|e| format!("read {}: {e}", wasm.display()))?;
    // Refuse to double-embed — re-packaging a fresh build is the workflow, not stacking
    // sections onto an already-packaged artifact (which would leave a stale manifest behind).
    if ezbar_wasm::manifest::read(&wasm_bytes).is_some() {
        return Err(format!(
            "{} already carries an ezbar:manifest — package a fresh build instead",
            wasm.display()
        ));
    }

    let packaged = ezbar_wasm::manifest::inject(&wasm_bytes, &sidecar_bytes);
    let out = out.unwrap_or(wasm);
    std::fs::write(out, &packaged).map_err(|e| format!("write {}: {e}", out.display()))?;

    // Checksum the SHIPPED bytes (with the manifest), so the registry entry pins what loads.
    let sha = ezbar::grants::sha256_hex(&packaged);
    Ok(registry_entry(&meta, &sha))
}

/// Parse the required/optional fields out of a sidecar `ezbar-plugin.toml`. `id` and
/// `version` are required (the registry path is `plugins/<id>/<version>.toml`); `wit`
/// defaults to the first frozen version, `name` to `id`, `description` to empty.
fn parse_sidecar(body: &str) -> Result<Meta, String> {
    let doc: toml::Value = body
        .parse()
        .map_err(|e| format!("ezbar-plugin.toml: {e}"))?;
    let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let id = s("id").ok_or("ezbar-plugin.toml: missing `id`")?;
    let version = s("version").ok_or("ezbar-plugin.toml: missing `version`")?;
    Ok(Meta {
        name: s("name").unwrap_or_else(|| id.clone()),
        wit: s("wit").unwrap_or_else(|| "0.1.0".to_string()),
        description: s("description").unwrap_or_default(),
        id,
        version,
    })
}

/// Format the `plugins/<id>/<version>.toml` registry entry for the author to commit. The
/// `capabilities` are intentionally NOT re-emitted here — they live in the embedded manifest
/// (one source of truth); the index keeps `publisher`/`artifact` placeholders the author
/// fills at publish time (TOFU pin + release URL, RFC 0014 §4/§5).
fn registry_entry(m: &Meta, sha256: &str) -> String {
    format!(
        "# plugins/{id}/{version}.toml — commit this to the registry\n\
         id = {id:?}\n\
         name = {name:?}\n\
         version = {version:?}\n\
         wit = {wit:?}\n\
         description = {desc:?}\n\
         sha256 = {sha256:?}\n\
         # publisher = \"<your-handle>\"   # TOFU-pinned at first install\n\
         # artifact = \"https://.../{id}.wasm\"   # release download URL\n\
         # capabilities are carried in the embedded ezbar:manifest",
        id = m.id,
        name = m.name,
        version = m.version,
        wit = m.wit,
        desc = m.description,
        sha256 = sha256,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sidecar_fills_defaults_and_requires_id_version() {
        let m = parse_sidecar("id = \"weather\"\nversion = \"1.2.0\"").unwrap();
        assert_eq!(m.id, "weather");
        assert_eq!(m.version, "1.2.0");
        assert_eq!(m.name, "weather"); // defaults to id
        assert_eq!(m.wit, "0.1.0"); // default frozen version
        assert!(m.description.is_empty());

        assert!(parse_sidecar("version = \"1.0.0\"").is_err()); // missing id
        assert!(parse_sidecar("id = \"x\"").is_err()); // missing version
    }

    #[test]
    fn registry_entry_pins_id_version_and_sha() {
        let m = parse_sidecar(
            "id = \"weather\"\nname = \"Weather\"\nversion = \"1.2.0\"\nwit = \"0.2.0\"\n\
             description = \"Forecast chip.\"",
        )
        .unwrap();
        let entry = registry_entry(&m, "deadbeef");
        assert!(entry.contains("plugins/weather/1.2.0.toml"));
        assert!(entry.contains("id = \"weather\""));
        assert!(entry.contains("wit = \"0.2.0\""));
        assert!(entry.contains("sha256 = \"deadbeef\""));
        assert!(entry.contains("name = \"Weather\""));
    }
}
