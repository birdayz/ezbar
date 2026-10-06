//! GPU metrics from DRM sysfs (amdgpu's `gpu_busy_percent` + device-local hwmon).
//! No subprocesses, privileges, or vendor library required. Other drivers can use
//! this collector if they expose the same ABI; otherwise their metrics are unknown.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GpuData {
    pub usage: Option<f64>,
    pub temperature: Option<f64>,
}

/// Read one GPU. An empty card selects the lowest-numbered DRM card exposing
/// `gpu_busy_percent`; an explicit `cardN` never falls back to a different GPU.
/// Missing/unreadable metrics remain unknown, not a misleading idle/0°C reading.
pub fn read_gpu(card: &str) -> GpuData {
    read_gpu_at(Path::new("/sys/class/drm"), card)
}

fn read_gpu_at(drm: &Path, card: &str) -> GpuData {
    let Some(device) = select_device(drm, card) else {
        return GpuData::default();
    };
    GpuData {
        usage: read_number(&device.join("gpu_busy_percent"))
            .filter(|v| (0..=100).contains(v))
            .map(|v| v as f64),
        temperature: read_temperature(&device),
    }
}

fn card_number(name: &str) -> Option<u32> {
    let n = name.strip_prefix("card")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

fn select_device(drm: &Path, card: &str) -> Option<PathBuf> {
    if !card.is_empty() {
        card_number(card)?;
        return Some(drm.join(card).join("device"));
    }
    let mut cards: Vec<_> = fs::read_dir(drm)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let n = card_number(entry.file_name().to_str()?)?;
            let device = entry.path().join("device");
            device
                .join("gpu_busy_percent")
                .exists()
                .then_some((n, device))
        })
        .collect();
    cards.sort_by_key(|(n, _)| *n);
    cards.into_iter().next().map(|(_, device)| device)
}

fn read_number(path: &Path) -> Option<i64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_temperature(device: &Path) -> Option<f64> {
    let mut hwmons: Vec<_> = fs::read_dir(device.join("hwmon"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    hwmons.sort();
    for hwmon in hwmons {
        let Ok(entries) = fs::read_dir(&hwmon) else {
            continue;
        };
        let mut inputs: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix("temp")?.strip_suffix("_input"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect();
        inputs.sort();
        // amdgpu exposes edge, junction and memory temperatures. Prefer edge
        // (the conventional GPU temperature), not whichever sensor is hottest.
        // Older/unlabelled hwmon implementations use temp1_input.
        let mut fallback = None;
        for input in inputs {
            let Some(mc) = read_number(&input).filter(|v| (1..=200_000).contains(v)) else {
                continue;
            };
            let name = input.file_name()?.to_str()?;
            let label = fs::read_to_string(input.with_file_name(name.replace("_input", "_label")))
                .unwrap_or_default();
            if label.trim().eq_ignore_ascii_case("edge") {
                return Some(mc as f64 / 1000.0);
            }
            if name == "temp1_input" {
                fallback = Some(mc as f64 / 1000.0);
            }
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ezbar-gpu-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, value: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }

        fn gpu(&self, card: &str, usage: &str, temperature: &str) {
            self.write(&format!("{card}/device/gpu_busy_percent"), usage);
            self.write(
                &format!("{card}/device/hwmon/hwmon7/temp1_input"),
                temperature,
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_amd_usage_and_edge_temperature() {
        let f = Fixture::new();
        f.gpu("card1", "37\n", "54000\n");
        f.write("card1/device/hwmon/hwmon7/temp1_label", "edge\n");
        f.write("card1/device/hwmon/hwmon7/temp2_input", "65000\n");
        f.write("card1/device/hwmon/hwmon7/temp2_label", "junction\n");
        f.write("card1/device/hwmon/hwmon7/temp3_input", "76000\n");
        f.write("card1/device/hwmon/hwmon7/temp3_label", "mem\n");
        assert_eq!(
            read_gpu_at(&f.0, ""),
            GpuData {
                usage: Some(37.0),
                temperature: Some(54.0),
            }
        );
    }

    #[test]
    fn prefers_edge_label_over_input_number() {
        let f = Fixture::new();
        f.gpu("card1", "0", "65000");
        f.write("card1/device/hwmon/hwmon7/temp1_label", "junction");
        f.write("card1/device/hwmon/hwmon7/temp2_input", "54250");
        f.write("card1/device/hwmon/hwmon7/temp2_label", "EDGE\n");
        assert_eq!(read_gpu_at(&f.0, "").temperature, Some(54.25));
    }

    #[test]
    fn autodetection_skips_connectors_and_sorts_cards_numerically() {
        let f = Fixture::new();
        f.gpu("card10", "80", "70000");
        f.gpu("card2", "20", "42000");
        f.gpu("card0-DP-1", "99", "90000");
        f.gpu("renderD128", "99", "90000");
        f.write("card0/device/vendor", "0x8086"); // no compatible utilization ABI
        assert_eq!(read_gpu_at(&f.0, "").usage, Some(20.0));
        assert_eq!(read_gpu_at(&f.0, "").temperature, Some(42.0));
        assert_eq!(read_gpu_at(&f.0, "card10").usage, Some(80.0));
        assert_eq!(read_gpu_at(&f.0, "card10").temperature, Some(70.0));
    }

    #[test]
    fn missing_metrics_stay_unknown_and_never_mix_devices() {
        let f = Fixture::new();
        assert_eq!(read_gpu_at(&f.0, ""), GpuData::default());
        f.gpu("card1", "0", "54000");
        f.write("card2/device/gpu_busy_percent", "100");
        assert_eq!(
            read_gpu_at(&f.0, "card2"),
            GpuData {
                usage: Some(100.0),
                temperature: None,
            }
        );
        assert_eq!(read_gpu_at(&f.0, "card3"), GpuData::default());
        for card in ["card1-DP-1", "../card1", "/card1", "card", "card+1"] {
            assert_eq!(read_gpu_at(&f.0, card), GpuData::default());
        }
        fs::remove_file(f.0.join("card1/device/gpu_busy_percent")).unwrap();
        assert_eq!(read_gpu_at(&f.0, "card1").usage, None);
        assert_eq!(read_gpu_at(&f.0, "card1").temperature, Some(54.0));
        fs::remove_dir_all(f.0.join("card1")).unwrap();
        assert_eq!(read_gpu_at(&f.0, "card1"), GpuData::default());
    }

    #[test]
    fn rejects_malformed_and_out_of_range_readings() {
        let f = Fixture::new();
        for (usage, temperature) in [
            ("garbage", "garbage"),
            ("NaN", "inf"),
            ("-1", "-1000"),
            ("101", "200001"),
            ("", "0"),
        ] {
            f.gpu("card1", usage, temperature);
            assert_eq!(read_gpu_at(&f.0, ""), GpuData::default());
        }
    }
}
