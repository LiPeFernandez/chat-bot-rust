use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};

use crate::agent::{Agent, AgentEvent};
use crate::config::AppConfig;
use crate::sessions::{SessionMeta, SessionStore, UiItem};
use crate::utils::{self, AppError};

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    agent: Arc<Mutex<Agent>>,
    sessions: Arc<Mutex<SessionStore>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    User {
        content: String,
        #[serde(default)]
        display: String,
    },
    New,
    Open {
        session_id: String,
    },
    Delete {
        session_id: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outbound {
    Note {
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
        timed_out: bool,
    },
    Assistant {
        content: String,
    },
    Error {
        content: String,
    },
    Done,
    Snapshot {
        current_id: String,
        sessions: Vec<SessionMeta>,
        items: Vec<UiItem>,
        model: String,
    },
    Sessions {
        current_id: String,
        sessions: Vec<SessionMeta>,
    },
}

impl From<AgentEvent> for Outbound {
    fn from(event: AgentEvent) -> Self {
        match event {
            AgentEvent::Note { content } => Self::Note { content },
            AgentEvent::ToolCall {
                id,
                name,
                arguments,
            } => Self::ToolCall {
                id,
                name,
                arguments,
            },
            AgentEvent::ToolResult {
                id,
                name,
                output,
                timed_out,
            } => Self::ToolResult {
                id,
                name,
                output,
                timed_out,
            },
            AgentEvent::Assistant { content } => Self::Assistant { content },
        }
    }
}

#[derive(Serialize)]
struct StatusBody {
    model: String,
    base_url: String,
    image: String,
    network: String,
    memory_mb: u64,
    cpus: f64,
    timeout_secs: u64,
    user: String,
    max_iterations: u32,
    shell: String,
    python: String,
}

pub async fn serve(config: AppConfig, agent: Agent) -> Result<(), AppError> {
    let port = config.web_port;
    let sessions = SessionStore::load()?;
    let state = AppState {
        config: Arc::new(config),
        agent: Arc::new(Mutex::new(agent)),
        sessions: Arc::new(Mutex::new(sessions)),
    };
    {
        let sessions = state.sessions.lock().await;
        let history = sessions.current_history();
        drop(sessions);
        state.agent.lock().await.adopt(history).await?;
    }
    let app = Router::new()
        .route("/", get(index))
        .route("/app.css", get(css))
        .route("/app.js", get(javascript))
        .route("/api/status", get(status))
        .route("/ws", get(ws))
        .with_state(state.clone());

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|err| AppError::Config(format!("não consegui abrir a porta {port}: {err}")))?;
    let url = format!("http://127.0.0.1:{port}");
    utils::print_info(&format!("Interface em {url}"));
    let browser_url = url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        open_browser(&browser_url);
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|err| AppError::Config(format!("servidor HTTP: {err}")))?;

    state.agent.lock().await.shutdown().await?;
    Ok(())
}

fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    if let Err(err) = command.arg(url).spawn() {
        tracing::warn!(%err, "não abri o navegador");
    }
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../web/index.html"),
    )
}

async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/app.css"),
    )
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}

async fn status(State(state): State<AppState>) -> Json<StatusBody> {
    let config = &state.config;
    Json(StatusBody {
        model: config.llm_model.clone(),
        base_url: config.llm_base_url.clone(),
        image: config.sandbox_image.clone(),
        network: config.sandbox_network.clone(),
        memory_mb: config.sandbox_memory_mb,
        cpus: config.sandbox_cpus,
        timeout_secs: config.sandbox_timeout_secs,
        user: config.sandbox_user.clone(),
        max_iterations: config.max_iterations,
        shell: config.sandbox_shell.clone(),
        python: config.sandbox_python.clone(),
    })
}

async fn ws(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Outbound>();
    let _ = tx.send(snapshot_message(&state).await);
    let mut busy = false;

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                let Some(frame) = incoming else { break };
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::debug!(%err, "websocket encerrou");
                        break;
                    }
                };
                let Message::Text(text) = frame else {
                    continue;
                };
                let message: ClientMessage = match serde_json::from_str(&text) {
                    Ok(message) => message,
                    Err(_) => {
                        let _ = tx.send(Outbound::Error {
                            content: "mensagem do cliente não é JSON válido".into(),
                        });
                        continue;
                    }
                };
                match message {
                    ClientMessage::User { content, display } => {
                        let content = content.trim().to_string();
                        if content.is_empty() {
                            continue;
                        }
                        if content.len() > 80_000 {
                            let _ = tx.send(Outbound::Error {
                                content: "mensagem longa demais".into(),
                            });
                            continue;
                        }
                        if busy {
                            let _ = tx.send(Outbound::Error {
                                content: "Aguarde a resposta terminar.".into(),
                            });
                            continue;
                        }
                        busy = true;
                        let display = if display.trim().is_empty() {
                            content.clone()
                        } else {
                            display.trim().to_string()
                        };
                        spawn_turn(state.clone(), tx.clone(), content, display);
                    }
                    ClientMessage::New => {
                        if busy {
                            let _ = tx.send(Outbound::Error {
                                content: "Aguarde a resposta terminar.".into(),
                            });
                            continue;
                        }
                        spawn_new(state.clone(), tx.clone());
                    }
                    ClientMessage::Open { session_id } => {
                        if busy {
                            let _ = tx.send(Outbound::Error {
                                content: "Aguarde a resposta terminar.".into(),
                            });
                            continue;
                        }
                        spawn_open(state.clone(), tx.clone(), session_id);
                    }
                    ClientMessage::Delete { session_id } => {
                        if busy {
                            let _ = tx.send(Outbound::Error {
                                content: "Aguarde a resposta terminar.".into(),
                            });
                            continue;
                        }
                        spawn_delete(state.clone(), tx.clone(), session_id);
                    }
                }
            }
            outgoing = rx.recv() => {
                let Some(message) = outgoing else { break };
                let done = matches!(message, Outbound::Done);
                let payload = serde_json::to_string(&message).unwrap_or_else(|_| {
                    r#"{"type":"error","content":"falha ao serializar o evento"}"#.to_string()
                });
                if sender.send(Message::Text(payload)).await.is_err() {
                    break;
                }
                if done {
                    busy = false;
                }
            }
        }
    }
}

fn spawn_turn(
    state: AppState,
    tx: mpsc::UnboundedSender<Outbound>,
    content: String,
    display: String,
) {
    tokio::spawn(async move {
        if let Err(err) = state.sessions.lock().await.push_user(&display) {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
            let _ = tx.send(Outbound::Done);
            return;
        }
        let mut agent = state.agent.lock().await;
        let (events, mut incoming) = mpsc::unbounded_channel();
        let mut recorded = Vec::new();
        let result = {
            let mut turn = std::pin::pin!(agent.handle_turn(&content, &events));
            loop {
                tokio::select! {
                    biased;
                    event = incoming.recv() => {
                        if let Some(event) = event {
                            recorded.push(event.clone());
                            let _ = tx.send(event.into());
                        }
                    }
                    result = &mut turn => break result,
                }
            }
        };
        drop(events);
        while let Ok(event) = incoming.try_recv() {
            recorded.push(event.clone());
            let _ = tx.send(event.into());
        }
        if let Err(err) = &result {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
        }
        let history = agent.history();
        let error_text = result.err().map(|err| err.to_string());
        drop(agent);
        for event in &recorded {
            record_event(&state, event).await;
        }
        if let Some(content) = error_text {
            let _ = state
                .sessions
                .lock()
                .await
                .push_item(UiItem::Error { content });
        }
        let _ = state.sessions.lock().await.set_history(history);
        let _ = tx.send(session_list(&state).await);
        let _ = tx.send(Outbound::Done);
    });
}

fn spawn_new(state: AppState, tx: mpsc::UnboundedSender<Outbound>) {
    tokio::spawn(async move {
        let mut agent = state.agent.lock().await;
        if let Err(err) = agent.reset().await {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
            return;
        }
        let history = agent.history();
        drop(agent);
        let mut sessions = state.sessions.lock().await;
        if let Err(err) = sessions.create() {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
            return;
        }
        let _ = sessions.set_history(history);
        drop(sessions);
        let _ = tx.send(snapshot_message(&state).await);
    });
}

fn spawn_open(state: AppState, tx: mpsc::UnboundedSender<Outbound>, session_id: String) {
    tokio::spawn(async move {
        let history = {
            let mut sessions = state.sessions.lock().await;
            if let Err(err) = sessions.open(&session_id) {
                let _ = tx.send(Outbound::Error {
                    content: err.to_string(),
                });
                return;
            }
            sessions.current_history()
        };
        if let Err(err) = state.agent.lock().await.adopt(history).await {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
            return;
        }
        let _ = tx.send(snapshot_message(&state).await);
    });
}

fn spawn_delete(state: AppState, tx: mpsc::UnboundedSender<Outbound>, session_id: String) {
    tokio::spawn(async move {
        let history = {
            let mut sessions = state.sessions.lock().await;
            if let Err(err) = sessions.delete(&session_id) {
                let _ = tx.send(Outbound::Error {
                    content: err.to_string(),
                });
                return;
            }
            sessions.current_history()
        };
        if let Err(err) = state.agent.lock().await.adopt(history).await {
            let _ = tx.send(Outbound::Error {
                content: err.to_string(),
            });
            return;
        }
        let _ = tx.send(snapshot_message(&state).await);
    });
}

async fn record_event(state: &AppState, event: &AgentEvent) {
    let stored = match event {
        AgentEvent::Note { content } => UiItem::Note {
            content: content.clone(),
        },
        AgentEvent::ToolCall {
            id,
            name,
            arguments,
        } => UiItem::Tool {
            id: id.clone(),
            name: name.clone(),
            arguments: arguments.clone(),
            output: String::new(),
            timed_out: false,
            pending: true,
        },
        AgentEvent::ToolResult {
            id,
            output,
            timed_out,
            ..
        } => {
            let _ = state
                .sessions
                .lock()
                .await
                .finish_tool(id, output, *timed_out);
            return;
        }
        AgentEvent::Assistant { content } => UiItem::Assistant {
            content: content.clone(),
        },
    };
    let _ = state.sessions.lock().await.push_item(stored);
}

async fn snapshot_message(state: &AppState) -> Outbound {
    let sessions = state.sessions.lock().await;
    Outbound::Snapshot {
        current_id: sessions.current_id().to_string(),
        sessions: sessions.metas(),
        items: sessions.current_items(),
        model: state.config.llm_model.clone(),
    }
}

async fn session_list(state: &AppState) -> Outbound {
    let sessions = state.sessions.lock().await;
    Outbound::Sessions {
        current_id: sessions.current_id().to_string(),
        sessions: sessions.metas(),
    }
}
