//! Hash-keyed capability consent — the security core of RFC 0014 Phase A.
//!
//! **The bug this closes.** Capability grants live in `config.toml` keyed by the
//! plugin *id* (`[modules.weather].network = [...]`), and the id is just the `.wasm`
//! stem. Nothing bound that grant to the *bytes* that actually run: swap
//! `weather.wasm` for a hostile binary of the same name and it silently inherits the
//! network grant — a textbook confused-deputy. RFC 0006 §5 always promised grants
//! keyed on the artifact hash; this is that promise, decoupled from the (later)
//! manifest/registry machinery so the security fix can ship on its own.
//!
//! **The model — trust on first use.** The host records `id -> sha256(wasm)` in a
//! host-owned `~/.config/ezbar/grants.toml` (separate from the user-owned
//! `config.toml`, which the user freely edits). At load, [`decide`] compares the
//! on-disk binary's hash to the recorded one:
//!
//! | recorded consent        | result                                              |
//! |-------------------------|-----------------------------------------------------|
//! | none (first sight)      | **TOFU**: record this hash, grant the config caps   |
//! | matches the on-disk wasm| grant the config caps                                |
//! | **differs** (bytes changed) | **withhold every capability** + log loudly; the |
//! |                         | plugin still runs, fully sandboxed                  |
//!
//! A swapped binary has a new hash ⇒ no matching consent ⇒ no capabilities. A
//! *legitimate* rebuild/update also changes the hash, so re-consent with
//! `ezbar grant <id>` (which records the current on-disk hash) — an explicit,
//! affirmative act, exactly as RFC 0014 wants.
//!
//! **Forward-compat.** Phase B/C add an embedded `ezbar:manifest`; the consent key
//! becomes a *domain-separated* `sha256(wasm ‖ manifest)` (RFC 0006 mandates
//! length-prefixing, not naive concatenation — a hash-confusion footgun). That key
//! won't equal today's bare `sha256(wasm)`, so Phase B re-keys the store and forces a
//! one-time re-consent; the on-disk schema (`id -> sha256`) is unchanged.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The verdict for one plugin load. The host passes the configured grants through on
/// [`Decision::Granted`] and drops *every* capability on [`Decision::Withheld`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The on-disk bytes match the consented hash (or this is first use) — grant.
    Granted,
    /// The bytes don't match the recorded consent — the host must withhold all caps.
    Withheld,
}

/// `…/ezbar/grants.toml` — host-owned consent store, sibling of `config.toml`.
fn grants_path() -> Option<PathBuf> {
    Some(crate::config::path()?.with_file_name("grants.toml"))
}

/// Lowercase hex `sha256` of `bytes` — the consent key.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The recorded consent hash for `id` in `grants.toml`, if any. Tolerant of a missing
/// or malformed file (returns `None`) — a broken consent store must never wedge a load.
fn recorded(id: &str) -> Option<String> {
    let body = std::fs::read_to_string(grants_path()?).ok()?;
    let doc: toml::Value = body.parse().ok()?;
    doc.get(id)?
        .get("sha256")?
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
}

/// Load `grants.toml` as a table (empty if missing/malformed — host-owned, full-rewrite).
fn load_grants() -> toml::value::Table {
    grants_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|b| b.parse::<toml::Value>().ok())
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default()
}

/// Serialize the consent table back to `grants.toml` atomically (with the header). Shared by
/// [`record`] and [`forget`]. Best-effort; returns whether it was written.
fn save_grants(doc: toml::value::Table) -> bool {
    let Some(path) = grants_path() else {
        return false;
    };
    let body = format!(
        "# ezbar capability consent — host-owned, do NOT hand-edit.\n\
         # Each entry binds a plugin id to the sha256 of the .wasm it was approved for\n\
         # (RFC 0014). A changed binary withholds all capabilities until you re-approve\n\
         # with `ezbar grant <id>`.\n\n{}",
        toml::to_string(&toml::Value::Table(doc)).unwrap_or_default()
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    write_atomic(&path, &body).is_ok()
}

/// Persist `id -> sha256` into `grants.toml` (best-effort, atomic read-modify-write).
/// Returns whether the record was written; failure is non-fatal (logged by the caller).
fn record(id: &str, hex: &str) -> bool {
    let mut doc = load_grants();
    let mut entry = toml::value::Table::new();
    entry.insert("sha256".into(), toml::Value::String(hex.to_string()));
    doc.insert(id.to_string(), toml::Value::Table(entry));
    save_grants(doc)
}

/// Drop `id`'s consent record from `grants.toml` (for `ezbar remove`). `grants.toml` is
/// host-authored, so cleaning our own entry is fine — unlike `config.toml`, which we never
/// touch. Returns whether an entry was actually removed.
pub fn forget(id: &str) -> bool {
    let mut doc = load_grants();
    if doc.remove(id).is_none() {
        return false;
    }
    save_grants(doc)
}

/// The pure decision over (recorded consent, current artifact hash) — no I/O, so the
/// security logic is unit-testable on its own. `Tofu` carries the verdict that this id
/// was never seen and the host should record `current` before granting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Grant,
    Tofu,
    Withhold,
}

/// The one place the three-way rule lives: unseen ⇒ TOFU, same bytes ⇒ grant, changed
/// bytes ⇒ withhold. Case-folded compare (hashes are stored lowercase, but be lenient).
fn verdict(recorded: Option<&str>, current: &str) -> Verdict {
    match recorded {
        None => Verdict::Tofu,
        Some(h) if h.eq_ignore_ascii_case(current) => Verdict::Grant,
        Some(_) => Verdict::Withhold,
    }
}

/// Decide whether `id`'s configured capabilities may be granted to the binary at
/// `wasm_path`. Trust-on-first-use records a consent for an unseen id; a hash mismatch
/// withholds every capability and logs loudly. I/O failures fail *open* to today's
/// behaviour (grant + warn) — losing the ability to *persist* consent must not break a
/// user's working widget, and the real gate (deny on mismatch) still fires whenever a
/// record exists.
pub fn decide(id: &str, wasm_path: &Path) -> Decision {
    // We hash the bytes here and the reactor re-reads + loads them separately, so a
    // local-FS attacker could in principle swap the file between the two reads (TOCTOU).
    // That's out of scope by design: the threat is a same-named binary *at rest*
    // (supply-chain), and anyone who can win that race already owns `grants.toml` and
    // can self-approve. So a single read for the consent check is sufficient.
    let bytes = match std::fs::read(wasm_path) {
        Ok(b) => b,
        // Unreadable here ⇒ the reactor can't load it either; caps are moot. Withhold.
        Err(e) => {
            log::warn!("ezbar grants: can't read {wasm_path:?} for consent check: {e}");
            return Decision::Withheld;
        }
    };
    let current = sha256_hex(&bytes);
    let short = &current[..current.len().min(12)];
    match verdict(recorded(id).as_deref(), &current) {
        Verdict::Grant => Decision::Granted,
        // The artifact changed under a recorded consent. Withhold and explain.
        Verdict::Withhold => {
            log::warn!(
                "ezbar grants: '{id}' binary changed (sha256 {short}…) — capabilities WITHHELD; \
                 it runs sandboxed. If you updated it on purpose, re-approve: `ezbar grant {id}`."
            );
            Decision::Withheld
        }
        // First sight — trust on first use: record the hash and grant.
        Verdict::Tofu => {
            if record(id, &current) {
                log::info!("ezbar grants: recorded first-use consent for '{id}' (sha256 {short}…)");
            } else {
                log::warn!(
                    "ezbar grants: couldn't persist consent for '{id}' (read-only config dir?); \
                     granting this run but the swap-protection is inactive until it persists."
                );
            }
            Decision::Granted
        }
    }
}

/// What `apply_to_config` did to `[modules.<id>]`, for the human summary.
#[derive(Default)]
pub struct Applied {
    /// Capability keys newly written.
    pub written: Vec<String>,
    /// Capability keys left untouched because the user already set them.
    pub present: Vec<String>,
    /// Dangerous-tier keys (`fs`/`exec`) the manifest declared but we withheld without `--dangerous`.
    pub skipped_dangerous: Vec<String>,
}

/// Merge the capabilities `m` declares into `[modules.<id>]` in the user's `config.toml`, in
/// place and **format-preserving** (comments and ordering survive — that's why this uses
/// `toml_edit`, not the value crate). Existing keys are never clobbered: if the user already set
/// `network`/`fs`/… we leave their value alone and just report it. The dangerous tier (`fs`,
/// `exec`) is only written when `include_dangerous` is set; otherwise it's reported as skipped.
///
/// This is the one place that writes `config.toml`. RFC 0014's "print, never auto-write" still
/// holds for `ezbar inspect` (a look); `ezbar grant` is the explicit, affirmative act that opts
/// into the write — the whole point of "easy to ack".
pub fn apply_to_config(
    id: &str,
    m: &ezbar_wasm::manifest::Manifest,
    include_dangerous: bool,
) -> Result<Applied, String> {
    let path = crate::config::path().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
    let src = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = src
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let a = apply_to_doc(&mut doc, id, m, include_dangerous)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    write_atomic(&path, &doc.to_string()).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(a)
}

/// The pure, format-preserving document edit behind [`apply_to_config`] — no I/O, so it's
/// unit-testable. Descends to (or creates) `[modules.<id>]`, writes each declared capability
/// the user hasn't already set, and withholds the dangerous tier unless `include_dangerous`.
fn apply_to_doc(
    doc: &mut toml_edit::DocumentMut,
    id: &str,
    m: &ezbar_wasm::manifest::Manifest,
    include_dangerous: bool,
) -> Result<Applied, String> {
    use toml_edit::{value, Array, InlineTable, Item, Table, Value};

    let modules = doc
        .as_table_mut()
        .entry("modules")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("[modules] in config.toml is not a table")?;
    modules.set_implicit(true); // render `[modules.<id>]`, not a bare `[modules]`
    let m_tbl = modules
        .entry(id)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("[modules.{id}] in config.toml is not a table"))?;

    let mut a = Applied::default();
    // Set `key` to `val` only if the user hasn't already set it (never clobber their config).
    fn put(tbl: &mut Table, key: &str, val: Item, a: &mut Applied) {
        if tbl.contains_key(key) {
            a.present.push(key.to_string());
        } else {
            tbl.insert(key, val);
            a.written.push(key.to_string());
        }
    }
    let str_array = |xs: &[String]| {
        let mut arr = Array::new();
        for x in xs {
            arr.push(x.as_str());
        }
        arr
    };

    if !m.network.is_empty() {
        put(m_tbl, "network", value(str_array(&m.network)), &mut a);
    }
    if !m.feeds.is_empty() {
        put(m_tbl, "feeds", value(str_array(&m.feeds)), &mut a);
    }
    if m.sway {
        put(m_tbl, "sway", value(true), &mut a);
    }
    if !m.fs.is_empty() {
        if include_dangerous {
            let mut arr = Array::new();
            for p in &m.fs {
                let mut it = InlineTable::new();
                it.insert("path", p.as_str().into());
                it.insert("mode", "r".into());
                arr.push(Value::InlineTable(it));
            }
            put(m_tbl, "fs", value(arr), &mut a);
        } else {
            a.skipped_dangerous.push("fs".to_string());
        }
    }
    if !m.exec.is_empty() {
        if include_dangerous {
            put(m_tbl, "exec", value(str_array(&m.exec)), &mut a);
        } else {
            a.skipped_dangerous.push("exec".to_string());
        }
    }
    Ok(a)
}

/// `ezbar grant <id> [--dangerous]` — the one-command ack. Resolves the installed `.wasm`,
/// merges the capabilities its manifest declares into `[modules.<id>]` in `config.toml`
/// (format-preserving; `fs`/`exec` only with `include_dangerous`), then records consent for the
/// current bytes. Returns a human-facing summary. With no embedded manifest it just records
/// consent (the old behaviour) — there's nothing to auto-grant.
pub fn grant_cli(id: &str, include_dangerous: bool) -> Result<String, String> {
    let dir = crate::config::plugins_dir().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
    let path = ezbar_wasm::discover(&dir)
        .into_iter()
        .find(|(pid, _)| pid == id)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("no plugin '{id}' in {}", dir.display()))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let hex = sha256_hex(&bytes);
    let short = &hex[..hex.len().min(12)];

    let mut lines = vec![format!("ezbar: approved '{id}' (sha256 {short}…).")];
    match ezbar_wasm::manifest::read(&bytes) {
        Some(m) => {
            let a = apply_to_config(id, &m, include_dangerous)?;
            if a.written.is_empty() && a.present.is_empty() {
                lines.push(format!("  [modules.{id}] needs no capabilities."));
            } else if a.written.is_empty() {
                lines.push(format!("  [modules.{id}] already configured ({}).", a.present.join(", ")));
            } else {
                lines.push(format!("  wrote [modules.{id}]: {}", a.written.join(", ")));
            }
            if !a.skipped_dangerous.is_empty() {
                lines.push(format!(
                    "  withheld DANGEROUS: {} — re-run `ezbar grant {id} --dangerous` to grant them",
                    a.skipped_dangerous.join(", ")
                ));
            }
        }
        None => lines.push(format!(
            "  no embedded manifest — grant capabilities by hand in [modules.{id}] if it needs any."
        )),
    }

    if !record(id, &hex) {
        return Err(format!(
            "wrote config but couldn't record consent for '{id}' to {:?}",
            grants_path()
        ));
    }
    lines.push("  Reload the bar to apply.".to_string());
    Ok(lines.join("\n"))
}

/// Format the `[modules.<id>]` grant block a user pastes into `config.toml` to grant a
/// plugin the capabilities its manifest declares (RFC 0014 — **print, never auto-write**;
/// `config.toml` is the user's). Only declared (non-empty) capabilities are emitted, so a
/// plugin that asks for nothing yields just the header.
pub fn grant_block(id: &str, m: &ezbar_wasm::manifest::Manifest) -> String {
    let join = |xs: &[String]| {
        xs.iter()
            .map(|x| format!("{x:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut s = format!("[modules.{id}]\n");
    if !m.network.is_empty() {
        s.push_str(&format!("network = [{}]\n", join(&m.network)));
    }
    if !m.feeds.is_empty() {
        s.push_str(&format!("feeds = [{}]\n", join(&m.feeds)));
    }
    if m.sway {
        s.push_str("sway = true\n");
    }
    if !m.fs.is_empty() {
        // fs needs a mode the manifest doesn't pin — emit a read-only template to edit.
        let entries =
            m.fs.iter()
                .map(|p| format!("{{ path = {p:?}, mode = \"r\" }}"))
                .collect::<Vec<_>>()
                .join(", ");
        s.push_str(&format!(
            "fs = [{entries}]   # DANGEROUS — review; set mode = \"rw\" if needed\n"
        ));
    }
    if !m.exec.is_empty() {
        s.push_str(&format!(
            "exec = [{}]   # DANGEROUS — runs these programs\n",
            join(&m.exec)
        ));
    }
    s
}

/// `ezbar inspect <plugin.wasm>` — show what a plugin declares + the exact config to grant
/// it, without installing or running anything. The security decision stays the user's: we
/// print the hash (so they can match it to a source) and the grant block (to paste), and
/// point at `ezbar grant` to consent. `id` is the placement id (the `.wasm` stem).
pub fn inspect(wasm_path: &Path, id: &str) -> Result<String, String> {
    let bytes =
        std::fs::read(wasm_path).map_err(|e| format!("read {}: {e}", wasm_path.display()))?;
    let hash = sha256_hex(&bytes);
    let mut out = format!(
        "plugin '{id}'  ({})\n  sha256: {hash}\n\n",
        wasm_path.display()
    );
    match ezbar_wasm::manifest::read(&bytes) {
        Some(m) => {
            let caps = cap_summary(&m);
            out.push_str(&format!("declares: {caps}\n\n"));
            out.push_str("# paste into ~/.config/ezbar/config.toml to grant it:\n");
            out.push_str(&grant_block(id, &m));
            out.push_str(&format!(
                "\n# then approve these exact bytes:\n#   ezbar grant {id}\n"
            ));
        }
        None => {
            out.push_str(&format!(
                "declares: nothing (no ezbar:manifest) — grant capabilities manually in\n\
                 [modules.{id}] (network/feeds/sway) if it needs them; see the plugin's docs.\n"
            ));
        }
    }
    Ok(out)
}

/// Read-only consent state for `id` against its on-disk bytes — for `ezbar list` (does NOT
/// trust-on-first-use record, unlike [`decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentState {
    /// Recorded consent matches the on-disk bytes.
    Consented,
    /// A consent exists but the bytes changed since — capabilities are withheld until re-grant.
    Changed,
    /// Never recorded (a first load would TOFU it).
    Unseen,
    /// The artifact couldn't be read.
    Unreadable,
}

/// Report `id`'s consent state without mutating `grants.toml` (read-only; for `ezbar list`).
pub fn consent_state(id: &str, wasm_path: &Path) -> ConsentState {
    let Ok(bytes) = std::fs::read(wasm_path) else {
        return ConsentState::Unreadable;
    };
    let current = sha256_hex(&bytes);
    match recorded(id) {
        None => ConsentState::Unseen,
        Some(h) if h.eq_ignore_ascii_case(&current) => ConsentState::Consented,
        Some(_) => ConsentState::Changed,
    }
}

/// A one-line human summary of the declared capabilities ("network: a, b · sway"), or
/// "no capabilities" when it asks for nothing.
pub fn cap_summary(m: &ezbar_wasm::manifest::Manifest) -> String {
    let mut parts = Vec::new();
    if !m.network.is_empty() {
        parts.push(format!("network: {}", m.network.join(", ")));
    }
    if !m.feeds.is_empty() {
        parts.push(format!("feeds: {}", m.feeds.join(", ")));
    }
    if m.sway {
        parts.push("sway (read-only)".to_string());
    }
    if !m.fs.is_empty() {
        parts.push(format!("\u{26a0} fs: {}", m.fs.join(", "))); // ⚠ dangerous tier
    }
    if !m.exec.is_empty() {
        parts.push(format!("\u{26a0} exec: {}", m.exec.join(", ")));
    }
    if parts.is_empty() {
        "no capabilities".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Write `contents` to `path` atomically: sibling temp file, then rename over the
/// target (mirrors `install::write_atomic` — a crash mid-write leaves the old file).
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad grants path"))?;
    let tmp = path.with_file_name(format!(".{name}.ezbar-tmp"));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_stable_lowercase_hex() {
        // Known vector: sha256("") = e3b0c442…
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let h = sha256_hex(b"weather-bytes");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(sha256_hex(b"benign.wasm"), sha256_hex(b"hostile.wasm"));
    }

    #[test]
    fn grant_block_emits_only_declared_caps() {
        use ezbar_wasm::manifest::Manifest;
        let m = Manifest {
            network: vec!["api.open-meteo.com".into(), "wttr.in".into()],
            sway: true,
            ..Default::default()
        };
        let block = grant_block("weather", &m);
        assert_eq!(
            block,
            "[modules.weather]\nnetwork = [\"api.open-meteo.com\", \"wttr.in\"]\nsway = true\n"
        );
        // a plugin that declares nothing → just the header (nothing to grant)
        assert_eq!(grant_block("x", &Manifest::default()), "[modules.x]\n");
    }

    fn manifest() -> ezbar_wasm::manifest::Manifest {
        ezbar_wasm::manifest::Manifest {
            network: vec!["calendar.google.com".into()],
            fs: vec!["~/.config/ezbar".into()],
            exec: vec!["xdg-open".into()],
            ..Default::default()
        }
    }

    #[test]
    fn apply_writes_safe_tier_and_withholds_dangerous() {
        let mut doc = "".parse::<toml_edit::DocumentMut>().unwrap();
        let a = apply_to_doc(&mut doc, "calendar", &manifest(), false).unwrap();
        assert_eq!(a.written, ["network"]);
        assert_eq!(a.skipped_dangerous, ["fs", "exec"]);
        let out = doc.to_string();
        assert!(out.contains("[modules.calendar]"));
        assert!(out.contains("network = [\"calendar.google.com\"]"));
        assert!(!out.contains("exec"), "exec must be withheld without --dangerous");
        assert!(!out.contains("fs ="), "fs must be withheld without --dangerous");
    }

    #[test]
    fn apply_dangerous_writes_fs_and_exec() {
        let mut doc = "".parse::<toml_edit::DocumentMut>().unwrap();
        let a = apply_to_doc(&mut doc, "calendar", &manifest(), true).unwrap();
        assert_eq!(a.written, ["network", "fs", "exec"]);
        assert!(a.skipped_dangerous.is_empty());
        let out = doc.to_string();
        assert!(out.contains("exec = [\"xdg-open\"]"));
        assert!(out.contains("path = \"~/.config/ezbar\""));
        assert!(out.contains("mode = \"r\""));
    }

    #[test]
    fn apply_never_clobbers_existing_keys_and_preserves_comments() {
        // The user already tuned `network` (an extra host) and left a comment — both must survive,
        // and the pre-set key is reported `present`, not overwritten.
        let src = "\
# my bar config — keep me!
[modules.calendar]
network = [\"calendar.google.com\", \"extra.example.com\"]
";
        let mut doc = src.parse::<toml_edit::DocumentMut>().unwrap();
        let a = apply_to_doc(&mut doc, "calendar", &manifest(), true).unwrap();
        assert_eq!(a.present, ["network"]);
        assert_eq!(a.written, ["fs", "exec"]);
        let out = doc.to_string();
        assert!(out.contains("# my bar config — keep me!"), "comment preserved");
        assert!(out.contains("extra.example.com"), "user's extra host preserved");
        assert!(out.contains("exec = [\"xdg-open\"]"), "new dangerous cap added");
    }

    #[test]
    fn verdict_is_the_confused_deputy_gate() {
        let benign = sha256_hex(b"benign.wasm");
        let hostile = sha256_hex(b"hostile.wasm");
        // never seen → trust on first use
        assert_eq!(verdict(None, &benign), Verdict::Tofu);
        // same bytes we consented to → grant
        assert_eq!(verdict(Some(&benign), &benign), Verdict::Grant);
        // a swapped binary under the same id → withheld (the whole point)
        assert_eq!(verdict(Some(&benign), &hostile), Verdict::Withhold);
        // stored hash is compared case-insensitively
        assert_eq!(
            verdict(Some(&benign.to_uppercase()), &benign),
            Verdict::Grant
        );
    }
}
