//! Claude Code status: running instances, the active 5-hour usage block (via
//! `ccusage`), and account rate limits (via the statusline JSON tee written to
//! ~/.claude/ezbar-status.json by ezbar-statusline-wrapper.sh).

use std::collections::HashSet;
use std::fs;
use std::process::Command;

use chrono::{DateTime, Local, TimeZone};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Instance {
    pub project: String,
    pub waiting: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub cost: f64,
    pub burn_per_hour: f64,
    pub minutes_left: i64,
    pub reset: String,
    pub projected_cost: f64,
    pub model: String,
}

#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub five_h_left: Option<f64>,
    pub five_h_reset: String,
    pub weekly_left: Option<f64>,
    pub weekly_reset: String,
}

fn read_cmdline(pid: &str) -> String {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
}

fn ppid_of(pid: &str) -> Option<String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Enumerates running `claude` processes and flags which are waiting for input
/// (detected via their `notify-send … "waiting for your input"` child).
pub fn instances() -> Vec<Instance> {
    let mut claude_pids: Vec<String> = Vec::new();
    let mut waiting_ppids: HashSet<String> = HashSet::new();

    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid = e.file_name().to_string_lossy().to_string();
            if !pid.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            match comm.trim() {
                "claude" => claude_pids.push(pid),
                "notify-send" => {
                    if read_cmdline(&pid).contains("waiting for your input") {
                        if let Some(ppid) = ppid_of(&pid) {
                            waiting_ppids.insert(ppid);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut out: Vec<Instance> = claude_pids
        .into_iter()
        .map(|pid| {
            let project = fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
                .unwrap_or_else(|| "?".to_string());
            let waiting = waiting_ppids.contains(&pid);
            Instance { project, waiting }
        })
        .collect();
    out.sort_by(|a, b| a.project.cmp(&b.project));
    out
}

fn parse_rfc3339_hm(s: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
}

fn unix_hm(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn unix_date(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%H:%M %-d %b").to_string())
        .unwrap_or_default()
}

/// Runs `ccusage blocks --active --json` (via bunx) and parses the active block.
pub async fn block() -> Option<Block> {
    let home = std::env::var("HOME").ok()?;
    let bunx = format!("{home}/.bun/bin/bunx");
    let output = tokio::task::spawn_blocking(move || {
        Command::new(&bunx)
            .args(["ccusage", "blocks", "--active", "--json"])
            .output()
    })
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&output.stdout).ok()?;
    let b = v["blocks"]
        .as_array()?
        .iter()
        .find(|b| b["isActive"].as_bool().unwrap_or(false))?;
    Some(Block {
        cost: b["costUSD"].as_f64().unwrap_or(0.0),
        burn_per_hour: b["burnRate"]["costPerHour"].as_f64().unwrap_or(0.0),
        minutes_left: b["projection"]["remainingMinutes"].as_i64().unwrap_or(0),
        reset: b["endTime"].as_str().and_then(parse_rfc3339_hm).unwrap_or_default(),
        projected_cost: b["projection"]["totalCost"].as_f64().unwrap_or(0.0),
        model: b["models"][0].as_str().unwrap_or("").to_string(),
    })
}

/// Reads the rate limits captured from the Claude Code statusline JSON.
pub fn limits() -> Option<Limits> {
    let home = std::env::var("HOME").ok()?;
    let data = fs::read_to_string(format!("{home}/.claude/ezbar-status.json")).ok()?;
    let v: Value = serde_json::from_str(&data).ok()?;
    let rl = &v["rate_limits"];
    if rl.is_null() {
        return None;
    }
    let five_used = rl["five_hour"]["used_percentage"].as_f64();
    let week_used = rl["seven_day"]["used_percentage"].as_f64();
    Some(Limits {
        five_h_left: five_used.map(|u| 100.0 - u),
        five_h_reset: rl["five_hour"]["resets_at"].as_i64().map(unix_hm).unwrap_or_default(),
        weekly_left: week_used.map(|u| 100.0 - u),
        weekly_reset: rl["seven_day"]["resets_at"].as_i64().map(unix_date).unwrap_or_default(),
    })
}
