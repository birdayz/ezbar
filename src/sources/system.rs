//! System metrics read from /proc and /sys. Pure logic, no UI deps.
//! Faithful port of pkg/datasource/system.go.

use std::fs;
use std::thread::sleep;
use std::time::Duration;

/// Formats bytes with one decimal place (KB, MB, GB, ...).
pub fn humanize_with_decimals(bytes: u64) -> String {
    const UNIT: u64 = 1024;
    if bytes < UNIT {
        return format!("{} B", bytes);
    }
    let mut div = UNIT;
    let mut exp = 0usize;
    let mut n = bytes / UNIT;
    while n >= UNIT {
        div *= UNIT;
        exp += 1;
        n /= UNIT;
    }
    let val = bytes as f64 / div as f64;
    let units = ["KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    if exp >= units.len() {
        exp = units.len() - 1;
    }
    format!("{:.1}{}", val, units[exp])
}

fn parse_cpu_stat(stat: &str) -> Option<[i64; 4]> {
    let line = stat.lines().next()?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 5 || fields[0] != "cpu" {
        return None;
    }
    let mut values = [0i64; 4];
    for i in 0..4 {
        values[i] = fields[i + 1].parse::<i64>().ok()?;
    }
    Some(values)
}

/// Returns the CPU usage display string, e.g. "45%". Sleeps 100ms between samples.
pub fn get_cpu_usage() -> String {
    let stat1 = match fs::read_to_string("/proc/stat") {
        Ok(s) => s,
        Err(_) => return "--".to_string(),
    };
    sleep(Duration::from_millis(100));
    let stat2 = match fs::read_to_string("/proc/stat") {
        Ok(s) => s,
        Err(_) => return "--".to_string(),
    };

    let (cpu1, cpu2) = match (parse_cpu_stat(&stat1), parse_cpu_stat(&stat2)) {
        (Some(a), Some(b)) => (a, b),
        _ => return "--".to_string(),
    };

    format!("{}%", cpu_usage_percent(&cpu1, &cpu2))
}

/// CPU busy percentage between two /proc/stat samples (user, nice, system, idle).
fn cpu_usage_percent(c1: &[i64; 4], c2: &[i64; 4]) -> i64 {
    let idle = c2[3] - c1[3];
    let total = (c2[0] + c2[1] + c2[2] + c2[3]) - (c1[0] + c1[1] + c1[2] + c1[3]);
    if total == 0 {
        return 0;
    }
    100 - (idle * 100) / total
}

/// Returns the memory usage display string, e.g. "8.2GB/16.0GB".
pub fn get_memory_usage() -> String {
    let meminfo = match fs::read_to_string("/proc/meminfo") {
        Ok(s) => s,
        Err(_) => return "--".to_string(),
    };
    let mut mem_total: i64 = 0;
    let mut mem_available: i64 = 0;
    for line in meminfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        match fields[0] {
            "MemTotal:" => mem_total = fields[1].parse().unwrap_or(0),
            "MemAvailable:" => mem_available = fields[1].parse().unwrap_or(0),
            _ => {}
        }
    }
    if mem_total == 0 {
        return "--".to_string();
    }
    let mem_used = mem_total - mem_available;
    let used_bytes = (mem_used * 1024) as u64;
    let total_bytes = (mem_total * 1024) as u64;
    format!(
        "{}/{}",
        humanize_with_decimals(used_bytes),
        humanize_with_decimals(total_bytes)
    )
}

/// CPU temp drivers, in preference order (AMD k10temp/zenpower, Intel coretemp…).
const CPU_TEMP_DRIVERS: &[&str] = &["k10temp", "zenpower", "coretemp", "cpu_thermal", "k8temp"];
/// Per-driver labels that denote the package/control temperature.
const CPU_TEMP_LABELS: &[&str] = &["Tctl", "Tdie", "Package id 0", "Package", "Tccd1"];

/// Returns the CPU temperature display string, e.g. "45°C".
pub fn get_cpu_temperature() -> String {
    match read_cpu_temp_millic() {
        Some(mc) => format!("{:.0}°C", mc as f64 / 1000.0),
        None => "--".to_string(),
    }
}

/// CPU package temperature in millidegrees, picking the *CPU* sensor (a coretemp/
/// k10temp hwmon) — never `acpitz`/ambient, which is what the old code grabbed.
fn read_cpu_temp_millic() -> Option<i64> {
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        // deterministic order so we don't depend on readdir ordering
        let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in &dirs {
            let name = fs::read_to_string(dir.join("name")).unwrap_or_default();
            if CPU_TEMP_DRIVERS.contains(&name.trim()) {
                if let Some(mc) = pick_cpu_temp(&read_temp_inputs(dir)) {
                    return Some(mc);
                }
            }
        }
    }
    // Fallback: a thermal zone whose type names the CPU (skip acpitz/ambient).
    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for dir in entries.flatten().map(|e| e.path()) {
            let t = fs::read_to_string(dir.join("type")).unwrap_or_default();
            let t = t.trim();
            if t == "x86_pkg_temp" || t == "cpu-thermal" || t.contains("cpu") {
                if let Ok(mc) = fs::read_to_string(dir.join("temp"))
                    .unwrap_or_default()
                    .trim()
                    .parse()
                {
                    return Some(mc);
                }
            }
        }
    }
    None
}

/// Reads all `tempN_input` values in a hwmon dir as (label, millidegrees) pairs.
fn read_temp_inputs(dir: &std::path::Path) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        let mut inputs: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
            })
            .collect();
        inputs.sort();
        for input in inputs {
            let Some(value) = fs::read_to_string(&input)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
            else {
                continue;
            };
            let label_name = input
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replace("_input", "_label");
            let label = fs::read_to_string(input.with_file_name(label_name)).unwrap_or_default();
            out.push((label.trim().to_string(), value));
        }
    }
    out
}

/// Picks the package/control temperature from a hwmon's labelled inputs, else
/// the first input. Pure; this is the crux of the wrong-sensor fix.
fn pick_cpu_temp(inputs: &[(String, i64)]) -> Option<i64> {
    for label in CPU_TEMP_LABELS {
        if let Some((_, v)) = inputs.iter().find(|(l, _)| l.eq_ignore_ascii_case(label)) {
            return Some(*v);
        }
    }
    inputs.first().map(|(_, v)| *v)
}

/// Extracts the numeric CPU percentage from a string like "45%".
pub fn extract_cpu_usage_value(s: &str) -> f64 {
    for part in s.split(' ') {
        if part.contains('%') {
            if let Ok(v) = part.replace('%', "").parse::<f64>() {
                return v;
            }
        }
    }
    0.0
}

fn parse_memory_size(size: &str) -> f64 {
    let s = size.trim();
    if let Some(v) = s.strip_suffix("GB") {
        return v.parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0 * 1024.0;
    }
    if let Some(v) = s.strip_suffix("MB") {
        return v.parse::<f64>().unwrap_or(0.0) * 1024.0 * 1024.0;
    }
    if let Some(v) = s.strip_suffix("KB") {
        return v.parse::<f64>().unwrap_or(0.0) * 1024.0;
    }
    0.0
}

/// Extracts the memory usage percentage from a string like "8.2GB/16.0GB".
pub fn extract_memory_usage_value(s: &str) -> f64 {
    for part in s.split(' ') {
        if part.contains('/') {
            let frac: Vec<&str> = part.split('/').collect();
            if frac.len() == 2 {
                let used = parse_memory_size(frac[0]);
                let total = parse_memory_size(frac[1]);
                if used > 0.0 && total > 0.0 {
                    return (used / total) * 100.0;
                }
            }
        }
    }
    0.0
}

/// Extracts the numeric temperature from a string like "45°C".
pub fn extract_temperature_value(s: &str) -> f64 {
    for part in s.split(' ') {
        if part.contains("°C") {
            if let Ok(v) = part.replace("°C", "").parse::<f64>() {
                return v;
            }
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_scales() {
        assert_eq!(humanize_with_decimals(0), "0 B");
        assert_eq!(humanize_with_decimals(512), "512 B");
        assert_eq!(humanize_with_decimals(1024), "1.0KB");
        assert_eq!(humanize_with_decimals(1536), "1.5KB");
        assert_eq!(humanize_with_decimals(1024 * 1024), "1.0MB");
        assert_eq!(humanize_with_decimals(1024 * 1024 * 1024), "1.0GB");
        assert_eq!(
            humanize_with_decimals(3 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "3.5GB"
        );
    }

    #[test]
    fn parse_cpu_stat_handles_proc_format() {
        // real /proc/stat uses two spaces after the aggregate "cpu" line
        assert_eq!(
            parse_cpu_stat("cpu  10 20 30 40 50 60\ncpu0 1 2 3 4\n"),
            Some([10, 20, 30, 40])
        );
        assert_eq!(parse_cpu_stat(""), None);
        assert_eq!(parse_cpu_stat("intr 1 2 3"), None);
        assert_eq!(parse_cpu_stat("cpu 1 2"), None); // fewer than 5 fields
        assert_eq!(parse_cpu_stat("cpu0 1 2 3 4 5"), None); // not the aggregate line
    }

    #[test]
    fn cpu_usage_math() {
        assert_eq!(cpu_usage_percent(&[0, 0, 0, 0], &[0, 0, 0, 100]), 0); // all idle
        assert_eq!(cpu_usage_percent(&[0, 0, 0, 0], &[100, 0, 0, 0]), 100); // all busy
        assert_eq!(cpu_usage_percent(&[0, 0, 0, 0], &[50, 0, 0, 50]), 50); // half
        assert_eq!(cpu_usage_percent(&[1, 2, 3, 4], &[1, 2, 3, 4]), 0); // no movement
    }

    #[test]
    fn extract_values_from_display_strings() {
        assert_eq!(extract_cpu_usage_value("45%"), 45.0);
        assert_eq!(extract_cpu_usage_value("0%"), 0.0);
        assert_eq!(extract_cpu_usage_value("--"), 0.0);

        assert_eq!(extract_temperature_value("45°C"), 45.0);
        assert_eq!(extract_temperature_value("--"), 0.0);

        let mem = extract_memory_usage_value("8.0GB/16.0GB");
        assert!((mem - 50.0).abs() < 0.001, "got {mem}");
        assert_eq!(extract_memory_usage_value("--"), 0.0);
    }

    #[test]
    fn parse_memory_size_units() {
        assert_eq!(parse_memory_size("8.0GB"), 8.0 * 1024.0 * 1024.0 * 1024.0);
        assert_eq!(parse_memory_size("512MB"), 512.0 * 1024.0 * 1024.0);
        assert_eq!(parse_memory_size("2.0KB"), 2.0 * 1024.0);
        assert_eq!(parse_memory_size("garbage"), 0.0);
    }

    #[test]
    fn pick_cpu_temp_prefers_package_sensor() {
        // AMD k10temp: Tctl is the package sensor (the user's real ~57°C, not acpitz 17°C)
        assert_eq!(
            pick_cpu_temp(&[("Tctl".into(), 57000), ("Tccd1".into(), 55000)]),
            Some(57000)
        );
        // Intel coretemp: "Package id 0" over individual cores
        assert_eq!(
            pick_cpu_temp(&[("Core 0".into(), 58000), ("Package id 0".into(), 61000)]),
            Some(61000)
        );
        // no recognised label -> first input
        assert_eq!(
            pick_cpu_temp(&[("".into(), 50000), ("".into(), 48000)]),
            Some(50000)
        );
        assert_eq!(pick_cpu_temp(&[]), None);
    }
}
