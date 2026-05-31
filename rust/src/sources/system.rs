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

/// Returns the CPU usage display string, e.g. "🖥️ 45%". Sleeps 100ms between samples.
pub fn get_cpu_usage() -> String {
    let stat1 = match fs::read_to_string("/proc/stat") {
        Ok(s) => s,
        Err(_) => return "🖥️ --".to_string(),
    };
    sleep(Duration::from_millis(100));
    let stat2 = match fs::read_to_string("/proc/stat") {
        Ok(s) => s,
        Err(_) => return "🖥️ --".to_string(),
    };

    let (cpu1, cpu2) = match (parse_cpu_stat(&stat1), parse_cpu_stat(&stat2)) {
        (Some(a), Some(b)) => (a, b),
        _ => return "🖥️ --".to_string(),
    };

    let idle = cpu2[3] - cpu1[3];
    let total = (cpu2[0] + cpu2[1] + cpu2[2] + cpu2[3]) - (cpu1[0] + cpu1[1] + cpu1[2] + cpu1[3]);
    if total == 0 {
        return "🖥️ 0%".to_string();
    }
    let usage = 100 - (idle * 100) / total;
    format!("🖥️ {}%", usage)
}

/// Returns the memory usage display string, e.g. "💾 8.2GB/16.0GB".
pub fn get_memory_usage() -> String {
    let meminfo = match fs::read_to_string("/proc/meminfo") {
        Ok(s) => s,
        Err(_) => return "💾 --".to_string(),
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
        return "💾 --".to_string();
    }
    let mem_used = mem_total - mem_available;
    let used_bytes = (mem_used * 1024) as u64;
    let total_bytes = (mem_total * 1024) as u64;
    format!(
        "💾 {}/{}",
        humanize_with_decimals(used_bytes),
        humanize_with_decimals(total_bytes)
    )
}

/// Returns the CPU temperature display string, e.g. "🌡️ 45°C".
pub fn get_cpu_temperature() -> String {
    let temp_paths = [
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/class/thermal/thermal_zone1/temp",
        "/sys/devices/platform/thinkpad_hwmon/hwmon/hwmon7/temp1_input",
    ];
    for path in temp_paths {
        if let Ok(temp) = fs::read_to_string(path) {
            if let Ok(tv) = temp.trim().parse::<f64>() {
                let c = tv / 1000.0;
                return format!("🌡️ {:.0}°C", c);
            }
        }
    }
    // Scan hwmon
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.flatten() {
            let p = entry.path().join("temp1_input");
            if let Ok(temp) = fs::read_to_string(&p) {
                if let Ok(tv) = temp.trim().parse::<f64>() {
                    let c = tv / 1000.0;
                    return format!("🌡️ {:.0}°C", c);
                }
            }
        }
    }
    "🌡️ --".to_string()
}

/// Extracts the numeric CPU percentage from a string like "🖥️ 45%".
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

/// Extracts the memory usage percentage from a string like "💾 8.2GB/16.0GB".
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

/// Extracts the numeric temperature from a string like "🌡️ 45°C".
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
