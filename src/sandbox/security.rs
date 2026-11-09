use std::time::Duration;

use crate::config::AppConfig;
use crate::utils::AppError;

pub const CPU_PERIOD: i64 = 100_000;
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub cpu_quota: i64,
    pub pids_limit: i64,
    pub timeout: Duration,
    pub network_mode: String,
    pub user: String,
    pub max_command_bytes: usize,
    pub max_output_bytes: usize,
}

impl SecurityPolicy {
    pub fn from_config(config: &AppConfig) -> Result<Self, AppError> {
        validate_network(&config.sandbox_network)?;
        validate_user(&config.sandbox_user)?;
        validate_binary("SANDBOX_SHELL", &config.sandbox_shell)?;
        validate_binary("SANDBOX_PYTHON", &config.sandbox_python)?;

        let memory_bytes = i64::try_from(config.sandbox_memory_mb.saturating_mul(1024 * 1024))
            .map_err(|_| AppError::Config("SANDBOX_MEMORY_MB excede o intervalo de i64".into()))?;
        let nano = (config.sandbox_cpus * 1_000_000_000.0).round();
        let quota = (config.sandbox_cpus * (CPU_PERIOD as f64)).round();
        if !nano.is_finite() || !quota.is_finite() || nano < 1.0 || quota < 1.0 {
            return Err(AppError::Config("SANDBOX_CPUS inválido".into()));
        }

        Ok(Self {
            memory_bytes,
            nano_cpus: nano as i64,
            cpu_quota: quota as i64,
            pids_limit: config.sandbox_pids_limit,
            timeout: Duration::from_secs(config.sandbox_timeout_secs),
            network_mode: config.sandbox_network.clone(),
            user: config.sandbox_user.clone(),
            max_command_bytes: MAX_COMMAND_BYTES,
            max_output_bytes: MAX_OUTPUT_BYTES,
        })
    }

    pub fn network_disabled(&self) -> bool {
        self.network_mode == "none"
    }
}

pub fn validate_payload(payload: &str, max_bytes: usize) -> Result<(), AppError> {
    if payload.trim().is_empty() {
        return Err(AppError::Sandbox(
            "payload vazio; nada foi executado".into(),
        ));
    }
    if payload.len() > max_bytes {
        return Err(AppError::Sandbox(format!(
            "payload excede {max_bytes} bytes"
        )));
    }
    if payload
        .chars()
        .any(|c| c == '\0' || (c.is_control() && c != '\n' && c != '\r' && c != '\t'))
    {
        return Err(AppError::Sandbox(
            "payload contém caractere de controle não permitido".into(),
        ));
    }
    Ok(())
}

pub fn append_capped(buf: &mut String, chunk: &str, max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let remaining = max_bytes - buf.len();
    if chunk.len() <= remaining {
        buf.push_str(chunk);
        return;
    }
    let mut end = remaining;
    while end > 0 && !chunk.is_char_boundary(end) {
        end -= 1;
    }
    buf.push_str(&chunk[..end]);
    buf.push_str("\n...[saída truncada]");
}

fn validate_network(mode: &str) -> Result<(), AppError> {
    if mode == "none" || mode == "bridge" {
        return Ok(());
    }
    Err(AppError::Config(format!(
        "SANDBOX_NETWORK deve ser 'none' ou 'bridge', recebi '{mode}'"
    )))
}

fn validate_user(user: &str) -> Result<(), AppError> {
    let parts: Vec<&str> = user.split(':').collect();
    let shape_ok = parts.len() <= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 32
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        });
    if !shape_ok {
        return Err(AppError::Config(format!(
            "SANDBOX_USER inválido ({user}). Use um uid:gid como 1000:1000"
        )));
    }
    let uid = parts[0];
    if uid == "0" || uid.eq_ignore_ascii_case("root") {
        return Err(AppError::Config(
            "SANDBOX_USER não pode ser root. A sandbox executa sem privilégios.".into(),
        ));
    }
    Ok(())
}

fn validate_binary(name: &str, binary: &str) -> Result<(), AppError> {
    let ok = !binary.is_empty()
        && binary.len() <= 128
        && !binary.contains("..")
        && binary
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+'));
    if !ok {
        return Err(AppError::Config(format!(
            "{name} inválido. Use um caminho simples como /bin/sh ou python"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AppConfig {
        AppConfig {
            llm_api_key: "ollama".into(),
            llm_base_url: "http://localhost:11434/v1".into(),
            llm_model: "demo".into(),
            llm_timeout_secs: 30,
            llm_max_tokens: 256,
            llm_temperature: 0.0,
            sandbox_image: "python:3.11-slim".into(),
            sandbox_network: "none".into(),
            sandbox_memory_mb: 512,
            sandbox_cpus: 1.0,
            sandbox_timeout_secs: 30,
            sandbox_user: "1000:1000".into(),
            sandbox_pids_limit: 64,
            sandbox_shell: "/bin/sh".into(),
            sandbox_python: "python".into(),
            max_iterations: 10,
            web_port: 8787,
        }
    }

    #[test]
    fn builds_one_cpu_and_512_mb() {
        let policy = SecurityPolicy::from_config(&config()).unwrap();
        assert_eq!(policy.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.nano_cpus, 1_000_000_000);
        assert_eq!(policy.cpu_quota, CPU_PERIOD);
        assert!(policy.network_disabled());
    }

    #[test]
    fn rejects_host_network_and_root() {
        let mut host = config();
        host.sandbox_network = "host".into();
        assert!(SecurityPolicy::from_config(&host).is_err());

        let mut root = config();
        root.sandbox_network = "bridge".into();
        root.sandbox_user = "0:0".into();
        assert!(SecurityPolicy::from_config(&root).is_err());
    }

    #[test]
    fn rejects_null_and_caps_output_on_char_boundary() {
        assert!(validate_payload("echo ok", 32).is_ok());
        assert!(validate_payload("echo \0", 32).is_err());

        let mut buf = String::new();
        append_capped(&mut buf, "áááá", 3);
        assert!(buf.ends_with("...[saída truncada]"));
        assert!(buf.is_char_boundary(buf.find('\n').unwrap()));
    }
}
