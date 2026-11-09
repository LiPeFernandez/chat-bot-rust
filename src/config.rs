use crate::utils::AppError;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub llm_api_key: String,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_timeout_secs: u64,
    pub llm_max_tokens: u32,
    pub llm_temperature: f32,
    pub sandbox_image: String,
    pub sandbox_network: String,
    pub sandbox_memory_mb: u64,
    pub sandbox_cpus: f64,
    pub sandbox_timeout_secs: u64,
    pub sandbox_user: String,
    pub sandbox_pids_limit: i64,
    pub sandbox_shell: String,
    pub sandbox_python: String,
    pub max_iterations: u32,
    pub web_port: u16,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, AppError> {
        dotenvy::dotenv().ok();

        let llm_max_tokens = env_u64("LLM_MAX_TOKENS", 2048, 64, 8192)?;
        let pids = env_u64("SANDBOX_PIDS_LIMIT", 128, 16, 1024)?;
        let iterations = env_u64("AGENT_MAX_ITERATIONS", 10, 1, 30)?;

        let config = Self {
            llm_api_key: env_string("LLM_API_KEY", "ollama"),
            llm_base_url: env_string("LLM_BASE_URL", "http://localhost:11434/v1")
                .trim_end_matches('/')
                .to_string(),
            llm_model: env_string("LLM_MODEL", "qwen2.5-coder:7b"),
            llm_timeout_secs: env_u64("LLM_TIMEOUT_SECS", 120, 10, 600)?,
            llm_max_tokens: u32::try_from(llm_max_tokens).unwrap_or(8192),
            llm_temperature: env_f32("LLM_TEMPERATURE", 0.2, 0.0, 2.0)?,
            sandbox_image: env_string("SANDBOX_IMAGE", "python:3.11-slim"),
            sandbox_network: env_string("SANDBOX_NETWORK", "none"),
            sandbox_memory_mb: env_u64("SANDBOX_MEMORY_MB", 512, 64, 4096)?,
            sandbox_cpus: env_f64("SANDBOX_CPUS", 1.0, 0.1, 8.0)?,
            sandbox_timeout_secs: env_u64("SANDBOX_TIMEOUT_SECS", 30, 1, 300)?,
            sandbox_user: env_string("SANDBOX_USER", "1000:1000"),
            sandbox_pids_limit: i64::try_from(pids).unwrap_or(128),
            sandbox_shell: env_string("SANDBOX_SHELL", "/bin/sh"),
            sandbox_python: env_string("SANDBOX_PYTHON", "python"),
            max_iterations: u32::try_from(iterations).unwrap_or(10),
            web_port: u16::try_from(env_u64("WEB_PORT", 8787, 1024, 65535)?).unwrap_or(8787),
        };

        validate_image(&config.sandbox_image)?;
        if config.llm_model.len() > 256 || config.llm_model.contains([' ', '\n', '\0']) {
            return Err(AppError::Config("LLM_MODEL inválido".into()));
        }
        if !(config.llm_base_url.starts_with("http://")
            || config.llm_base_url.starts_with("https://"))
        {
            return Err(AppError::Config(
                "LLM_BASE_URL precisa começar com http:// ou https://".into(),
            ));
        }

        Ok(config)
    }
}

fn env_string(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

fn env_u64(key: &str, default: u64, min: u64, max: u64) -> Result<u64, AppError> {
    let raw = match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(default),
    };
    let parsed: u64 = raw.trim().parse().map_err(|_| {
        AppError::Config(format!(
            "{key} deve ser um inteiro, recebi '{}'",
            raw.trim()
        ))
    })?;
    if parsed < min || parsed > max {
        return Err(AppError::Config(format!(
            "{key} deve estar entre {min} e {max}"
        )));
    }
    Ok(parsed)
}

fn env_f64(key: &str, default: f64, min: f64, max: f64) -> Result<f64, AppError> {
    let raw = match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(default),
    };
    let parsed: f64 = raw.trim().parse().map_err(|_| {
        AppError::Config(format!("{key} deve ser um número, recebi '{}'", raw.trim()))
    })?;
    if !parsed.is_finite() || parsed < min || parsed > max {
        return Err(AppError::Config(format!(
            "{key} deve estar entre {min} e {max}"
        )));
    }
    Ok(parsed)
}

fn env_f32(key: &str, default: f32, min: f32, max: f32) -> Result<f32, AppError> {
    let value = env_f64(key, f64::from(default), f64::from(min), f64::from(max))?;
    Ok(value as f32)
}

fn validate_image(image: &str) -> Result<(), AppError> {
    let allowed = image
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '@' | '_' | '-'));
    if image.is_empty() || image.len() > 256 || !allowed {
        return Err(AppError::Config(
            "SANDBOX_IMAGE inválida. Use uma referência Docker como python:3.11-slim".into(),
        ));
    }
    Ok(())
}
