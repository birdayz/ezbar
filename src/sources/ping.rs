//! Ping latency via the `ping` subprocess. Port of pkg/datasource/ping.go.

use std::process::Command;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct PingData {
    pub latency: f64,
    pub string: String,
    pub is_up: bool,
}

impl Default for PingData {
    fn default() -> Self {
        PingData {
            latency: 0.0,
            string: " --".to_string(),
            is_up: false,
        }
    }
}

pub fn perform_ping(target: &str) -> PingData {
    let output = Command::new("ping")
        .args(["-c", "1", "-W", "2", target])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            match extract_ping_latency(&s) {
                Some(latency) => PingData {
                    latency,
                    string: format!(" {:.1}ms", latency),
                    is_up: true,
                },
                None => PingData {
                    latency: 0.0,
                    string: " ERROR".to_string(),
                    is_up: false,
                },
            }
        }
        _ => PingData {
            latency: 0.0,
            string: " DOWN".to_string(),
            is_up: false,
        },
    }
}

pub fn extract_ping_latency(output: &str) -> Option<f64> {
    let re = Regex::new(r"time=([0-9.]+)\s*ms").ok()?;
    let caps = re.captures(output)?;
    caps.get(1)?.as_str().trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_latency_from_ping_output() {
        assert_eq!(
            extract_ping_latency("64 bytes from 8.8.8.8: icmp_seq=1 ttl=117 time=12.3 ms"),
            Some(12.3)
        );
        assert_eq!(extract_ping_latency("time=1.05ms"), Some(1.05));
        assert_eq!(extract_ping_latency("time=0 ms"), Some(0.0));
        assert_eq!(extract_ping_latency("no latency here"), None);
        assert_eq!(extract_ping_latency(""), None);
    }

    #[test]
    fn default_is_down() {
        let d = PingData::default();
        assert!(!d.is_up);
        assert_eq!(d.string, " --");
    }
}
