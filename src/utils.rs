use crate::config::AppConfig;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuração: {0}")]
    Config(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("sandbox: {0}")]
    Sandbox(String),
    #[error(
        "limite de {0} iterações de ferramentas atingido; interrompi o ciclo para evitar loop infinito"
    )]
    CircuitBreaker(u32),
}

pub fn print_banner(config: &AppConfig) {
    let key_state = if config.llm_api_key.is_empty()
        || config.llm_api_key.eq_ignore_ascii_case("ollama")
        || config.llm_api_key.eq_ignore_ascii_case("none")
    {
        "local (sem chave real)"
    } else {
        "configurada"
    };

    println!("\x1b[1muncensored-grok-bot\x1b[0m");
    println!("  modelo     {model}", model = config.llm_model);
    println!("  base url   {url}", url = config.llm_base_url);
    println!("  api key    {key_state}");
    println!(
        "  sandbox    {image}  user={user}  rede={network}  ram={ram}MB  cpu={cpu}  timeout={timeout}s",
        image = config.sandbox_image,
        user = config.sandbox_user,
        network = config.sandbox_network,
        ram = config.sandbox_memory_mb,
        cpu = config.sandbox_cpus,
        timeout = config.sandbox_timeout_secs,
    );
    println!(
        "  circuito   no máximo {n} rodadas de ferramentas por mensagem",
        n = config.max_iterations
    );
    println!("  comandos   :help  :reset  sair");
    println!();
}

pub fn print_help() {
    println!(
        "\
:help   mostra estes comandos
:reset  apaga o histórico da conversa (o container atual é destruído)
sair    encerra e remove a sandbox
exit / quit / :q  equivalentes a sair

A mensagem é enviada ao modelo. Se ele devolver tool_calls, o comando
roda dentro do container e a saída volta para o modelo."
    );
}

pub fn print_info(message: &str) {
    println!("\x1b[2m[info]\x1b[0m {message}");
}

pub fn print_error(message: &str) {
    eprintln!("\x1b[31m[erro]\x1b[0m {message}");
}

pub fn print_assistant(message: &str) {
    println!("\x1b[36m[agente]\x1b[0m {message}");
}

pub fn print_tool_call(name: &str, arguments: &str) {
    println!("\x1b[33m[ferramenta]\x1b[0m {name} {arguments}");
}

pub fn print_sandbox(output: &str) {
    println!("\x1b[32m[sandbox]\x1b[0m");
    println!("{output}");
}

pub fn prompt() {
    use std::io::{stdout, Write};
    print!("\x1b[1mvocê>\x1b[0m ");
    let _ = stdout().flush();
}
