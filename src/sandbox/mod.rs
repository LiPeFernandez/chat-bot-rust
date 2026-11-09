mod docker;
mod security;

use crate::config::AppConfig;
use crate::utils::AppError;
use security::{validate_payload, SecurityPolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
}

impl ExecOutput {
    pub fn render(&self, timeout_secs: u64) -> String {
        let mut out = String::new();
        if self.timed_out {
            out.push_str(&format!(
                "TIMEOUT após {timeout_secs}s. O processo foi morto e o container foi destruído.\n"
            ));
        } else {
            match self.exit_code {
                Some(code) => out.push_str(&format!("exit_code: {code}\n")),
                None => out.push_str("exit_code: desconhecido\n"),
            }
        }
        push_stream(&mut out, "--- stdout ---", &self.stdout);
        push_stream(&mut out, "--- stderr ---", &self.stderr);
        out
    }
}

fn push_stream(out: &mut String, label: &str, content: &str) {
    out.push_str(label);
    out.push('\n');
    if content.is_empty() {
        out.push_str("(vazio)\n");
        return;
    }
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
}

pub struct Sandbox {
    runtime: docker::DockerRuntime,
    policy: SecurityPolicy,
    timeout_secs: u64,
}

impl Sandbox {
    pub async fn connect(config: &AppConfig) -> Result<Self, AppError> {
        let policy = SecurityPolicy::from_config(config)?;
        let runtime = docker::DockerRuntime::connect(config, policy.clone()).await?;
        Ok(Self {
            runtime,
            policy,
            timeout_secs: config.sandbox_timeout_secs,
        })
    }

    pub async fn execute_bash(&mut self, command: &str) -> Result<ExecOutput, AppError> {
        validate_payload(command, self.policy.max_command_bytes)?;
        self.runtime.exec_shell(command).await
    }

    pub async fn execute_python(&mut self, code: &str) -> Result<ExecOutput, AppError> {
        validate_payload(code, self.policy.max_command_bytes)?;
        self.runtime.exec_python(code).await
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    pub async fn shutdown(&mut self) -> Result<(), AppError> {
        self.runtime.shutdown().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_marks_timeout() {
        let output = ExecOutput {
            stdout: "parcial".into(),
            stderr: String::new(),
            exit_code: None,
            timed_out: true,
        };
        let rendered = output.render(30);
        assert!(rendered.contains("TIMEOUT após 30s"));
        assert!(rendered.contains("parcial"));
        assert!(rendered.contains("(vazio)"));
    }
}
