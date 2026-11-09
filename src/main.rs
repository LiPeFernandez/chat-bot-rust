#![forbid(unsafe_code)]

mod agent;
mod config;
mod llm;
mod sandbox;
mod server;
mod sessions;
mod utils;

use std::io::Write;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::signal;
use tokio::sync::mpsc;

use agent::{Agent, AgentEvent};
use config::AppConfig;
use sandbox::Sandbox;
use utils::AppError;

#[tokio::main]
async fn main() {
    let cli = std::env::args().any(|arg| arg == "--cli");
    let result = if cli {
        run_cli().await
    } else {
        run_web().await
    };
    if let Err(err) = result {
        utils::print_error(&err.to_string());
        std::process::exit(1);
    }
}

async fn run_web() -> Result<(), AppError> {
    let (config, agent) = boot().await?;
    server::serve(config, agent).await
}

async fn run_cli() -> Result<(), AppError> {
    let (_config, mut agent) = boot().await?;
    utils::print_info("Modo terminal. A interface web fica de fora neste processo.");
    let repl_result = repl(&mut agent).await;
    let shutdown_result = agent.shutdown().await;
    repl_result?;
    shutdown_result?;
    Ok(())
}

async fn boot() -> Result<(AppConfig, Agent), AppError> {
    let config = AppConfig::from_env()?;
    init_tracing();
    utils::print_banner(&config);
    utils::print_info("Conectando ao Docker e preparando a imagem da sandbox...");
    let sandbox = Sandbox::connect(&config).await?;
    utils::print_info("Sandbox pronta. O container nasce no primeiro comando.");
    let agent = Agent::new(&config, sandbox)?;
    Ok((config, agent))
}

async fn repl(agent: &mut Agent) -> Result<(), AppError> {
    let mut reader = BufReader::new(tokio::io::stdin());
    loop {
        utils::prompt();
        let mut line = String::new();
        let read = reader.read_line(&mut line);
        tokio::pin!(read);

        let finished = tokio::select! {
            _ = signal::ctrl_c() => {
                println!();
                utils::print_info("interrupção recebida, encerrando a sandbox");
                break;
            }
            result = &mut read => result,
        };

        match finished {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => return Err(AppError::Config(format!("leitura do terminal: {err}"))),
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "sair" | "exit" | "quit" | ":q") {
            break;
        }
        if line == ":help" {
            utils::print_help();
            continue;
        }
        if line == ":reset" {
            agent.reset().await?;
            utils::print_info("histórico apagado e sandbox encerrada");
            continue;
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let printer = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                print_event(&event);
            }
        });
        let mut interrupted = false;
        let outcome = {
            let turn = agent.handle_turn(line, &tx);
            tokio::pin!(turn);
            tokio::select! {
                _ = signal::ctrl_c() => {
                    interrupted = true;
                    Ok(String::new())
                }
                outcome = &mut turn => outcome,
            }
        };
        drop(tx);
        let _ = printer.await;
        if interrupted {
            println!();
            utils::print_info("interrupção recebida, encerrando a sandbox");
            break;
        }
        if let Err(err) = outcome {
            utils::print_error(&err.to_string());
        }
    }
    let _ = std::io::stdout().flush();
    Ok(())
}

fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::Note { content } => utils::print_info(content),
        AgentEvent::ToolCall {
            name, arguments, ..
        } => utils::print_tool_call(name, arguments),
        AgentEvent::ToolResult { output, .. } => utils::print_sandbox(output),
        AgentEvent::Assistant { content } => utils::print_assistant(content),
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
