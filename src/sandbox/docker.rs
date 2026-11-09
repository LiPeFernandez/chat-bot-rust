use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bollard::container::{
    Config, CreateContainerOptions, KillContainerOptions, LogOutput, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::HostConfig;
use bollard::Docker;
use futures_util::{StreamExt, TryStreamExt};
use tokio::time::timeout;

use super::security::SecurityPolicy;
use crate::config::AppConfig;
use crate::utils::AppError;

use super::ExecOutput;

/// Runtime Docker. O socket do daemon nunca é montado no container e o
/// comando do modelo nunca é passado a `std::process` no host.
pub struct DockerRuntime {
    docker: Docker,
    policy: SecurityPolicy,
    image: String,
    shell: String,
    python: String,
    container_id: Option<String>,
    image_ready: bool,
}

impl DockerRuntime {
    pub async fn connect(config: &AppConfig, policy: SecurityPolicy) -> Result<Self, AppError> {
        let docker = connect_docker().await?;
        Ok(Self {
            docker,
            policy,
            image: config.sandbox_image.clone(),
            shell: config.sandbox_shell.clone(),
            python: config.sandbox_python.clone(),
            container_id: None,
            image_ready: false,
        })
    }

    pub async fn exec_shell(&mut self, command: &str) -> Result<ExecOutput, AppError> {
        self.exec(vec![
            self.shell.clone(),
            "-c".to_string(),
            command.to_string(),
        ])
        .await
    }

    pub async fn exec_python(&mut self, code: &str) -> Result<ExecOutput, AppError> {
        self.exec(vec![
            self.python.clone(),
            "-c".to_string(),
            code.to_string(),
        ])
        .await
    }

    pub async fn shutdown(&mut self) {
        self.force_destroy().await;
    }

    async fn exec(&mut self, cmd: Vec<String>) -> Result<ExecOutput, AppError> {
        self.ensure_container().await?;
        let Some(container_id) = self.container_id.clone() else {
            return Err(AppError::Sandbox(
                "container da sandbox não está disponível".into(),
            ));
        };

        let docker = self.docker.clone();
        let user = self.policy.user.clone();
        let limit = self.policy.max_output_bytes;
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let stdout_task = Arc::clone(&stdout);
        let stderr_task = Arc::clone(&stderr);

        let run = async move {
            let created = docker
                .create_exec(
                    &container_id,
                    CreateExecOptions {
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        attach_stdin: Some(false),
                        tty: Some(false),
                        privileged: Some(false),
                        cmd: Some(cmd),
                        user: Some(user),
                        working_dir: Some("/tmp".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|err| AppError::Sandbox(format!("create_exec: {err}")))?;

            let mut output = match docker
                .start_exec(&created.id, None)
                .await
                .map_err(|err| AppError::Sandbox(format!("start_exec: {err}")))?
            {
                StartExecResults::Attached { output, .. } => output,
                StartExecResults::Detached => {
                    return Err(AppError::Sandbox(
                        "a exec entrou em modo detached; recusei continuar".into(),
                    ));
                }
            };

            while let Some(item) = output.next().await {
                match item.map_err(|err| AppError::Sandbox(format!("stream da exec: {err}")))? {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        push_chunk(&stdout_task, &message, limit);
                    }
                    LogOutput::StdErr { message } => push_chunk(&stderr_task, &message, limit),
                    LogOutput::StdIn { .. } => {}
                }
            }

            let inspect = docker
                .inspect_exec(&created.id)
                .await
                .map_err(|err| AppError::Sandbox(format!("inspect_exec: {err}")))?;
            Ok(inspect.exit_code)
        };

        let exit_code = match timeout(self.policy.timeout, run).await {
            Ok(Ok(code)) => code,
            Ok(Err(err)) => {
                self.force_destroy().await;
                return Err(err);
            }
            Err(_) => {
                tracing::warn!(
                    timeout_secs = self.policy.timeout.as_secs(),
                    "execução estourou o tempo; destruindo o container"
                );
                self.force_destroy().await;
                return Ok(ExecOutput {
                    stdout: lock_string(&stdout),
                    stderr: lock_string(&stderr),
                    exit_code: None,
                    timed_out: true,
                });
            }
        };

        Ok(ExecOutput {
            stdout: lock_string(&stdout),
            stderr: lock_string(&stderr),
            exit_code,
            timed_out: false,
        })
    }

    async fn ensure_container(&mut self) -> Result<(), AppError> {
        if let Some(id) = self.container_id.clone() {
            let running = self
                .docker
                .inspect_container(&id, None)
                .await
                .ok()
                .and_then(|inspect| inspect.state)
                .and_then(|state| state.running)
                .unwrap_or(false);
            if running {
                return Ok(());
            }
            self.force_destroy().await;
        }

        self.ensure_image().await?;
        self.create_and_start().await
    }

    async fn ensure_image(&mut self) -> Result<(), AppError> {
        if self.image_ready {
            return Ok(());
        }
        match self.docker.inspect_image(&self.image).await {
            Ok(_) => {
                tracing::info!(image = %self.image, "imagem da sandbox já está presente");
            }
            Err(_) => {
                tracing::info!(image = %self.image, "baixando imagem da sandbox");
                let options = CreateImageOptions {
                    from_image: self.image.clone(),
                    ..Default::default()
                };
                let infos = self
                    .docker
                    .create_image(Some(options), None, None)
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(|err| AppError::Sandbox(format!("pull da imagem: {err}")))?;
                for info in infos {
                    if let Some(error) = info.error.as_ref().filter(|value| !value.is_empty()) {
                        return Err(AppError::Sandbox(format!("pull da imagem: {error}")));
                    }
                }
            }
        }
        self.image_ready = true;
        Ok(())
    }

    async fn create_and_start(&mut self) -> Result<(), AppError> {
        let name = container_name();
        let policy = &self.policy;
        let mut tmpfs = HashMap::new();
        tmpfs.insert("/tmp".to_string(), "rw,nosuid,nodev,size=128m".to_string());

        let host_config = HostConfig {
            auto_remove: Some(true),
            memory: Some(policy.memory_bytes),
            memory_swap: Some(policy.memory_bytes),
            nano_cpus: Some(policy.nano_cpus),
            pids_limit: Some(policy.pids_limit),
            network_mode: Some(policy.network_mode.clone()),
            cap_drop: Some(vec!["ALL".to_string()]),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            privileged: Some(false),
            readonly_rootfs: Some(false),
            init: Some(true),
            oom_kill_disable: Some(false),
            tmpfs: Some(tmpfs),
            publish_all_ports: Some(false),
            ..Default::default()
        };

        let mut labels = HashMap::new();
        labels.insert("app".to_string(), "uncensored-grok-bot".to_string());

        let config = Config {
            image: Some(self.image.clone()),
            user: Some(policy.user.clone()),
            entrypoint: Some(vec!["/bin/sleep".to_string()]),
            cmd: Some(vec!["infinity".to_string()]),
            working_dir: Some("/tmp".to_string()),
            env: Some(vec![
                "HOME=/tmp".to_string(),
                "LANG=C.UTF-8".to_string(),
                "PYTHONDONTWRITEBYTECODE=1".to_string(),
                "PYTHONUNBUFFERED=1".to_string(),
                "DEBIAN_FRONTEND=noninteractive".to_string(),
            ]),
            labels: Some(labels),
            network_disabled: Some(policy.network_disabled()),
            tty: Some(false),
            open_stdin: Some(false),
            attach_stdin: Some(false),
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            host_config: Some(host_config),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: name.clone(),
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|err| AppError::Sandbox(format!("create_container: {err}")))?;

        self.docker
            .start_container(&created.id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|err| AppError::Sandbox(format!("start_container {}: {err}", created.id)))?;

        tracing::info!(
            container = %created.id,
            user = %policy.user,
            network = %policy.network_mode,
            memory_bytes = policy.memory_bytes,
            nano_cpus = policy.nano_cpus,
            cpu_quota = policy.cpu_quota,
            "sandbox iniciada"
        );
        self.container_id = Some(created.id);
        Ok(())
    }

    async fn force_destroy(&mut self) {
        let Some(id) = self.container_id.take() else {
            return;
        };
        if let Err(err) = self
            .docker
            .kill_container(
                &id,
                Some(KillContainerOptions {
                    signal: "SIGKILL".to_string(),
                }),
            )
            .await
        {
            tracing::debug!(%err, %id, "kill do container não confirmado");
        }
        if let Err(err) = self
            .docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    link: false,
                }),
            )
            .await
        {
            tracing::debug!(%err, %id, "remove do container não confirmado");
        }
    }
}

async fn connect_docker() -> Result<Docker, AppError> {
    let mut errors = Vec::new();
    for docker in docker_candidates() {
        match docker.ping().await {
            Ok(_) => return Ok(docker),
            Err(err) => errors.push(err.to_string()),
        }
    }
    Err(AppError::Sandbox(format!(
        "não foi possível falar com o daemon Docker. Confirme que o Docker está aberto. Detalhe: {}",
        if errors.is_empty() {
            "nenhum socket encontrado".to_string()
        } else {
            errors.join(" | ")
        }
    )))
}

fn docker_candidates() -> Vec<Docker> {
    let mut clients = Vec::new();
    if let Ok(docker) = Docker::connect_with_local_defaults() {
        clients.push(docker);
    }
    if let Ok(home) = std::env::var("HOME") {
        let desktop = format!("{home}/.docker/run/docker.sock");
        if let Ok(docker) = Docker::connect_with_socket(&desktop, 120, bollard::API_DEFAULT_VERSION)
        {
            clients.push(docker);
        }
    }
    clients
}

fn container_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("grok-sbx-{n}-{nanos}")
}

fn push_chunk(buf: &Mutex<String>, message: &[u8], max_bytes: usize) {
    let text = String::from_utf8_lossy(message);
    let mut guard = buf.lock().unwrap_or_else(|err| err.into_inner());
    super::security::append_capped(&mut guard, &text, max_bytes);
}

fn lock_string(buf: &Mutex<String>) -> String {
    buf.lock().unwrap_or_else(|err| err.into_inner()).clone()
}
