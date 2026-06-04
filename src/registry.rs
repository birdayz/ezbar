//! Registry index entries + WIT-window version negotiation (RFC 0014 Phase C — the pure
//! core). `ezbar add <id>` resolves a plugin id to a concrete artifact by reading per-plugin
//! versioned index files `plugins/<id>/<version>.toml` and picking the **newest version
//! whose WIT is in this host's supported window** (RFC 0013's frozen-version window). The
//! fetch transport — a local directory today, HTTPS/git later — is a thin wrapper over this.
//!
//! (Plugin install is `ezbar add`, not `ezbar install`: the latter already means "add ezbar
//! to your sway config".)

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// One `plugins/<id>/<version>.toml` index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
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
    let id = req("id")?;
    Ok(Entry {
        name: s("name").unwrap_or_else(|| id.clone()),
        version: req("version")?,
        sha256: req("sha256")?,
        wit: s("wit").unwrap_or_else(|| "0.1.0".to_string()),
        artifact: s("artifact").unwrap_or_default(),
        publisher: s("publisher").unwrap_or_default(),
        description: s("description").unwrap_or_default(),
        id,
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
pub fn add(id: &str, registry: &str) -> Result<String, String> {
    let root = resolve_registry(registry)?;
    let dir = root.join("plugins").join(id);
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| format!("no plugin '{id}' in registry {registry}: {e}"))?;
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

    // TOFU publisher-pin (RFC 0014 §6): the first install pins the id's publisher; a later
    // install offering the SAME id under a DIFFERENT publisher is refused (a registry-takeover
    // guard, since a checksum only proves integrity, not authenticity). Unsigned entries (no
    // publisher) skip it.
    if !picked.publisher.is_empty() {
        match check_publisher(pinned(id).as_deref(), &picked.publisher) {
            PinCheck::Mismatch(p) => {
                return Err(format!(
                    "publisher changed for '{id}': pinned '{p}', registry now offers \
                     '{}' — refusing (possible takeover). Delete its line in publishers.toml \
                     to override.",
                    picked.publisher
                ))
            }
            PinCheck::Pin => {
                let _ = pin(id, &picked.publisher);
            }
            PinCheck::Ok => {}
        }
    }

    let bytes = read_artifact(&dir, picked)?;
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

// ── TOFU publisher-pin (RFC 0014 §6) ────────────────────────────────────────
/// The outcome of checking an offered publisher against the pinned one.
#[derive(Debug, PartialEq, Eq)]
enum PinCheck {
    /// First sight of this id — pin `offered`.
    Pin,
    /// Pinned publisher matches — proceed.
    Ok,
    /// Pinned publisher differs (carries the *pinned* one) — refuse.
    Mismatch(String),
}

/// Pure TOFU rule: unpinned ⇒ pin; same publisher ⇒ ok; different ⇒ mismatch.
fn check_publisher(pinned: Option<&str>, offered: &str) -> PinCheck {
    match pinned {
        None => PinCheck::Pin,
        Some(p) if p == offered => PinCheck::Ok,
        Some(p) => PinCheck::Mismatch(p.to_string()),
    }
}

/// `…/ezbar/publishers.toml` — the host-owned TOFU pin store (`id = "publisher"`).
fn pins_path() -> Option<PathBuf> {
    Some(ezbar::config::path()?.with_file_name("publishers.toml"))
}

fn load_pins() -> toml::value::Table {
    pins_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|b| b.parse::<toml::Value>().ok())
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default()
}

fn save_pins(t: toml::value::Table) -> bool {
    let Some(path) = pins_path() else { return false };
    let body = format!(
        "# ezbar registry publisher pins (TOFU) — host-owned. Delete a line to re-pin on the\n\
         # next `ezbar add`.\n\n{}",
        toml::to_string(&toml::Value::Table(t)).unwrap_or_default()
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("toml.ezbar-tmp");
    std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)).is_ok()
}

fn pinned(id: &str) -> Option<String> {
    load_pins().get(id).and_then(|v| v.as_str()).map(String::from)
}

fn pin(id: &str, publisher: &str) -> bool {
    let mut t = load_pins();
    t.insert(id.to_string(), toml::Value::String(publisher.to_string()));
    save_pins(t)
}

/// Drop `id`'s publisher pin (for `ezbar remove`). Returns whether a pin existed.
fn unpin(id: &str) -> bool {
    let mut t = load_pins();
    if t.remove(id).is_none() {
        return false;
    }
    save_pins(t)
}

/// Resolve a picked entry to its `.wasm` bytes. Prefer a `<version>.wasm` co-located with the
/// index entry (a small/personal registry that commits artifacts); otherwise download the
/// entry's `artifact` release URL (the production model — keeps the git repo small). Either
/// way the caller verifies `sha256` before installing, so a wrong download is caught.
fn read_artifact(dir: &Path, entry: &Entry) -> Result<Vec<u8>, String> {
    let local = dir.join(format!("{}.wasm", entry.version));
    if local.is_file() {
        return std::fs::read(&local).map_err(|e| format!("read artifact {}: {e}", local.display()));
    }
    let url = entry.artifact.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        download(url)
    } else {
        Err(format!(
            "no artifact for '{}' {}: neither a co-located {}.wasm nor an http(s) `artifact` URL",
            entry.id, entry.version, entry.version
        ))
    }
}

/// One-shot HTTPS GET of a release artifact (the CLI is synchronous, so drive a small
/// current-thread runtime). reqwest is the same client stack the bar's modules use.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let resp = reqwest::get(url).await.map_err(|e| format!("GET {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GET {url}: HTTP {}", resp.status()));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("read body {url}: {e}"))
    })
}

/// Resolve a registry location to a local directory to read from: a filesystem path is used
/// as-is; a **git URL** (`…://…`, `git@…`, or a `.git` suffix — including the future official
/// `https://github.com/birdayz/ezbar-plugins.git`) is shallow-cloned (or fast-forward-pulled)
/// into a per-URL cache dir first. So `ezbar add` works against a hosted registry, not only a
/// local folder, while reusing the exact same local resolution/verify/install core.
fn resolve_registry(loc: &str) -> Result<PathBuf, String> {
    if is_git_url(loc) {
        clone_or_pull(loc)
    } else {
        Ok(PathBuf::from(loc))
    }
}

/// A git remote we should clone rather than read as a local path.
fn is_git_url(s: &str) -> bool {
    s.contains("://") || s.starts_with("git@") || s.trim_end_matches('/').ends_with(".git")
}

/// Shallow-clone `url` into `~/.cache/ezbar/registry/<hash>` (or `git pull --ff-only` if
/// already cloned). A pull failure falls back to the cached clone (offline-tolerant); only a
/// first-clone failure is fatal.
fn clone_or_pull(url: &str) -> Result<PathBuf, String> {
    use std::process::Command;
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or("no cache dir (set HOME or XDG_CACHE_HOME)")?;
    let key = &ezbar::grants::sha256_hex(url.as_bytes())[..16];
    let dir = base.join("ezbar").join("registry").join(key);
    if dir.join(".git").is_dir() {
        // best-effort refresh; keep the cached clone if offline / the pull fails
        let _ = Command::new("git")
            .args(["-C", &dir.to_string_lossy(), "pull", "--ff-only", "--quiet"])
            .status();
        return Ok(dir);
    }
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ok = Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", url, &dir.to_string_lossy()])
        .status()
        .map_err(|e| format!("run git: {e} (is git installed?)"))?
        .success();
    if ok {
        Ok(dir)
    } else {
        Err(format!("git clone {url} failed"))
    }
}

/// `ezbar search [<term>]` — each registry plugin's newest in-window version whose
/// id/name/description contains `term` (everything if empty). Discovery before `add`; reuses
/// the same registry resolution (a local dir or a git clone).
pub fn search(term: &str, registry: &str) -> Result<String, String> {
    let root = resolve_registry(registry)?;
    let pdir = root.join("plugins");
    let rd = std::fs::read_dir(&pdir).map_err(|e| format!("read registry {}: {e}", pdir.display()))?;
    let needle = term.to_lowercase();
    let mut hits: Vec<(String, String, String)> = Vec::new(); // id, version, description
    for ent in rd.flatten().filter(|e| e.path().is_dir()) {
        let entries: Vec<Entry> = std::fs::read_dir(ent.path())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
            .filter_map(|p| std::fs::read_to_string(&p).ok())
            .filter_map(|b| parse_entry(&b).ok())
            .collect();
        if let Some(e) = pick_in_window(&entries, SUPPORTED_WIT) {
            let hay = format!("{} {} {}", e.id, e.name, e.description).to_lowercase();
            if needle.is_empty() || hay.contains(&needle) {
                hits.push((e.id.clone(), e.version.clone(), e.description.clone()));
            }
        }
    }
    if hits.is_empty() {
        return Ok(match term.is_empty() {
            true => "registry has no plugins in this ezbar's WIT window".into(),
            false => format!("no plugins match {term:?}"),
        });
    }
    hits.sort();
    Ok(hits
        .into_iter()
        .map(|(id, ver, desc)| format!("{id:<16} {ver:<10} {desc}\n"))
        .collect())
}

/// `ezbar list` — the installed plugins, each with its short content hash, consent state,
/// and declared capabilities. Read-only (never records consent). A management view so a user
/// can see what's installed and what still needs `ezbar grant`.
pub fn list() -> Result<String, String> {
    use ezbar::grants::ConsentState::*;
    let dir = ezbar::config::plugins_dir().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
    let plugins = ezbar_wasm::discover(&dir);
    if plugins.is_empty() {
        return Ok(format!("no plugins in {}", dir.display()));
    }
    let mut out = String::new();
    for (id, path) in plugins {
        let state = match ezbar::grants::consent_state(&id, &path) {
            Consented => "consented",
            Changed => "CHANGED — caps withheld (ezbar grant)",
            Unseen => "not yet consented",
            Unreadable => "unreadable",
        };
        let caps = ezbar_wasm::manifest::read_file(&path)
            .map(|m| ezbar::grants::cap_summary(&m))
            .unwrap_or_else(|| "no manifest".to_string());
        out.push_str(&format!("{id:<16} {state:<38} {caps}\n"));
    }
    Ok(out)
}

/// `ezbar remove <id>` — delete an installed plugin's `.wasm` and drop its host-owned
/// consent record. **Never** edits `config.toml` (the user authored it) — it only points out
/// the `[modules.<id>]` block they may want to remove. Errors if the plugin isn't installed.
pub fn remove(id: &str) -> Result<String, String> {
    let dir = ezbar::config::plugins_dir().ok_or("no config dir (set HOME or XDG_CONFIG_HOME)")?;
    let path = ezbar_wasm::discover(&dir)
        .into_iter()
        .find(|(pid, _)| pid == id)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("no plugin '{id}' installed in {}", dir.display()))?;
    std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
    let forgot = ezbar::grants::forget(id);
    let unpinned = unpin(id);
    let cleaned = match (forgot, unpinned) {
        (true, true) => " and its consent + publisher records",
        (true, false) => " and its consent record",
        (false, true) => " and its publisher pin",
        (false, false) => "",
    };
    Ok(format!(
        "removed '{id}' ({}){cleaned}.\nezbar did NOT touch config.toml — you may want to delete \
         its [modules.{id}] block yourself.",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(version: &str, wit: &str) -> Entry {
        Entry {
            id: "weather".into(),
            name: "Weather".into(),
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
    fn publisher_tofu_pins_then_guards() {
        assert_eq!(check_publisher(None, "birdayz"), PinCheck::Pin); // first sight → pin
        assert_eq!(check_publisher(Some("birdayz"), "birdayz"), PinCheck::Ok); // same → ok
        // a different publisher for a pinned id → refused (takeover guard)
        assert_eq!(
            check_publisher(Some("birdayz"), "attacker"),
            PinCheck::Mismatch("birdayz".to_string())
        );
    }

    #[test]
    fn verify_sha256_matches_case_insensitively() {
        let h = ezbar::grants::sha256_hex(b"artifact-bytes");
        assert!(verify_sha256(b"artifact-bytes", &h));
        assert!(verify_sha256(b"artifact-bytes", &h.to_uppercase())); // hex case-insensitive
        assert!(!verify_sha256(b"tampered", &h)); // a mismatch is caught
    }
}
