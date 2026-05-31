//! Kubectl current-context. Port of pkg/datasource/kubectl.go.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct KubectlData {
    pub context: String,
    pub string: String,
    pub is_production: bool,
}

impl Default for KubectlData {
    fn default() -> Self {
        KubectlData {
            context: "--".to_string(),
            string: "⚙️ --".to_string(),
            is_production: false,
        }
    }
}

pub fn update_context() -> KubectlData {
    let context = get_kubectl_context();
    let is_production = is_production_context(&context);
    KubectlData {
        string: format!("⚙️ {}", context),
        is_production,
        context,
    }
}

pub fn get_kubectl_context() -> String {
    let output = Command::new("kubectl")
        .args(["config", "current-context"])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let ctx = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if ctx.is_empty() {
                "--".to_string()
            } else {
                ctx
            }
        }
        _ => "--".to_string(),
    }
}

pub fn is_production_context(context: &str) -> bool {
    let lower = context.to_lowercase();
    lower.contains("prod") || lower.contains("prd")
}

pub fn get_all_contexts() -> Vec<String> {
    let output = Command::new("kubectl")
        .args(["config", "get-contexts", "-o", "name"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

pub fn set_context(context: &str) {
    let _ = Command::new("kubectl")
        .args(["config", "use-context", context])
        .status();
}

pub fn clear_context() {
    let _ = Command::new("kubectl")
        .args(["config", "unset", "current-context"])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_detection() {
        assert!(is_production_context("prod-cluster"));
        assert!(is_production_context("my-PROD-eu"));
        assert!(is_production_context("acme-prd-1"));
        assert!(!is_production_context("staging"));
        assert!(!is_production_context("dev-local"));
        assert!(!is_production_context("--"));
    }
}
