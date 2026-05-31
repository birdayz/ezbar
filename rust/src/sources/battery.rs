//! Battery status from /sys/class/power_supply/BAT0. Port of battery logic in cmd/ezbar/main.go.

use std::fs;
use std::path::Path;

pub fn has_battery() -> bool {
    Path::new("/sys/class/power_supply/BAT0").exists()
}

pub fn get_battery_status() -> String {
    let capacity = match fs::read_to_string("/sys/class/power_supply/BAT0/capacity") {
        Ok(s) => s.trim().to_string(),
        Err(_) => return "🔋 --".to_string(),
    };
    let status = match fs::read_to_string("/sys/class/power_supply/BAT0/status") {
        Ok(s) => s.trim().to_string(),
        Err(_) => return "🔋 --".to_string(),
    };

    let icon = match status.as_str() {
        "Charging" => "⚡",
        "Not charging" => "🔌",
        "Discharging" => "🔋",
        "Full" => "🔌",
        _ => "🔋",
    };

    let time_str = get_time_remaining(&status);
    format!("{} {}% [{}]", icon, capacity, time_str)
}

fn read_f64(path: &str) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse::<f64>().ok()
}

fn get_time_remaining(status: &str) -> String {
    let energy_now = match read_f64("/sys/class/power_supply/BAT0/energy_now") {
        Some(v) => v,
        None => return "--".to_string(),
    };
    let power_now = match read_f64("/sys/class/power_supply/BAT0/power_now") {
        Some(v) if v != 0.0 => v,
        _ => return "--".to_string(),
    };

    let hours = match status {
        "Charging" => {
            let energy_full = match read_f64("/sys/class/power_supply/BAT0/energy_full") {
                Some(v) => v,
                None => return "--".to_string(),
            };
            (energy_full - energy_now) / power_now
        }
        "Discharging" => energy_now / power_now,
        "Not charging" | "Full" => return "∞".to_string(),
        _ => return "--".to_string(),
    };

    let total_minutes = (hours * 60.0) as i64;
    if total_minutes < 0 {
        return "--".to_string();
    }
    let h = total_minutes / 60;
    let m = total_minutes % 60;
    if total_minutes < 60 {
        format!("{}m", m)
    } else {
        format!("{}h{}m", h, m)
    }
}
