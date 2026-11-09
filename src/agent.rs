use serde::Serialize;
use tokio::sync::mpsc;

use crate::config::AppConfig;
use crate::llm::{
    argument_field, normalize_tool_calls, parse_textual_tool_calls, system_prompt,
    ConversationMessage, LlmClient, ToolCall, TOOL_BASH, TOOL_PYTHON,
};
use crate::sandbox::Sandbox;
use crate::utils::AppError;

const MAX_CALLS_PER_STEP: usize = 8;

pub type EventTx = mpsc::UnboundedSender<AgentEvent>;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
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
}

fn emit(events: &EventTx, event: AgentEvent) {
    let _ = events.send(event);
}

pub struct Agent {
    llm: LlmClient,
    sandbox: Sandbox,
    max_iterations: u32,
    history: Vec<ConversationMessage>,
}

impl Agent {
    pub fn new(config: &AppConfig, sandbox: Sandbox) -> Result<Self, AppError> {
        Ok(Self {
            llm: LlmClient::new(config)?,
            sandbox,
            max_iterations: config.max_iterations,
            history: vec![ConversationMessage::System(system_prompt().to_string())],
        })
    }

    pub async fn handle_turn(
        &mut self,
        prompt: &str,
        events: &EventTx,
    ) -> Result<String, AppError> {
        self.history
            .push(ConversationMessage::User(prompt.to_string()));
        match self.reason(events).await {
            Ok(text) => Ok(text),
            Err(err) => {
                if matches!(self.history.last(), Some(ConversationMessage::User(_))) {
                    self.history.pop();
                }
                Err(err)
            }
        }
    }

    pub fn history(&self) -> Vec<ConversationMessage> {
        self.history.clone()
    }

    pub async fn adopt(&mut self, history: Vec<ConversationMessage>) -> Result<(), AppError> {
        self.history = normalize_history(history);
        self.sandbox.shutdown().await?;
        Ok(())
    }

    pub async fn reset(&mut self) -> Result<(), AppError> {
        self.history.truncate_to_system();
        self.sandbox.shutdown().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), AppError> {
        self.sandbox.shutdown().await
    }

    async fn reason(&mut self, events: &EventTx) -> Result<String, AppError> {
        for step in 1..=self.max_iterations {
            let mut response = self.llm.complete(&self.history).await?;
            let native_tools = !response.tool_calls.is_empty();
            if !native_tools {
                if let Some(content) = &response.content {
                    let parsed = parse_textual_tool_calls(content);
                    if !parsed.is_empty() {
                        response.tool_calls = parsed;
                    }
                }
            }

            if response.tool_calls.is_empty() {
                let text = response
                    .content
                    .filter(|content| !content.trim().is_empty())
                    .unwrap_or_else(|| "(o modelo não devolveu texto)".to_string());
                self.history.push(ConversationMessage::Assistant {
                    content: Some(text.clone()),
                    tool_calls: Vec::new(),
                });
                emit(
                    events,
                    AgentEvent::Assistant {
                        content: text.clone(),
                    },
                );
                return Ok(text);
            }

            if response.tool_calls.len() > MAX_CALLS_PER_STEP {
                tracing::warn!(
                    requested = response.tool_calls.len(),
                    kept = MAX_CALLS_PER_STEP,
                    "a resposta pediu chamadas demais; o excedente foi descartado"
                );
                response.tool_calls.truncate(MAX_CALLS_PER_STEP);
            }
            response.tool_calls = normalize_tool_calls(response.tool_calls);

            if native_tools {
                if let Some(content) = response
                    .content
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    emit(
                        events,
                        AgentEvent::Note {
                            content: content.to_string(),
                        },
                    );
                }
            }

            tracing::info!(
                step,
                calls = response.tool_calls.len(),
                "rodada de ferramentas"
            );
            self.history.push(ConversationMessage::Assistant {
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
            });

            let mut stop_round = false;
            for call in response.tool_calls {
                emit(
                    events,
                    AgentEvent::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    },
                );
                let (content, timed_out) = if stop_round {
                    (
                        "não executado: o container foi destruído após timeout da chamada anterior nesta rodada".to_string(),
                        false,
                    )
                } else {
                    let (rendered, timed_out) = self.dispatch_tool(&call).await;
                    (rendered, timed_out)
                };
                if timed_out {
                    stop_round = true;
                }
                emit(
                    events,
                    AgentEvent::ToolResult {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        output: content.clone(),
                        timed_out,
                    },
                );
                self.history.push(ConversationMessage::Tool {
                    tool_call_id: call.id,
                    content,
                });
            }
        }

        let note = format!(
            "Interrompi esta tarefa após {} rodadas de ferramentas sem uma resposta final.",
            self.max_iterations
        );
        self.history.push(ConversationMessage::Assistant {
            content: Some(note),
            tool_calls: Vec::new(),
        });
        Err(AppError::CircuitBreaker(self.max_iterations))
    }

    async fn dispatch_tool(&mut self, call: &ToolCall) -> (String, bool) {
        let timeout_secs = self.sandbox.timeout_secs();
        let result = match call.name.as_str() {
            TOOL_BASH => match argument_field(&call.arguments, "command") {
                Ok(command) => self.sandbox.execute_bash(&command).await,
                Err(err) => return (format!("erro ao ler argumentos: {err}"), false),
            },
            TOOL_PYTHON => match argument_field(&call.arguments, "code") {
                Ok(code) => self.sandbox.execute_python(&code).await,
                Err(err) => return (format!("erro ao ler argumentos: {err}"), false),
            },
            other => {
                return (
                    format!("ferramenta desconhecida: {other}. Use {TOOL_BASH} ou {TOOL_PYTHON}."),
                    false,
                );
            }
        };

        match result {
            Ok(output) => {
                let timed_out = output.timed_out;
                (output.render(timeout_secs), timed_out)
            }
            Err(err) => (format!("erro da sandbox: {err}"), false),
        }
    }
}

fn normalize_history(history: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    if let Some(ConversationMessage::System(prompt)) = history
        .iter()
        .find(|message| matches!(message, ConversationMessage::System(_)))
        .cloned()
    {
        messages.push(ConversationMessage::System(prompt));
    } else {
        messages.push(ConversationMessage::System(system_prompt().to_string()));
    }
    messages.extend(
        history
            .into_iter()
            .filter(|message| !matches!(message, ConversationMessage::System(_))),
    );
    messages
}

trait TruncateHistory {
    fn truncate_to_system(&mut self);
}

impl TruncateHistory for Vec<ConversationMessage> {
    fn truncate_to_system(&mut self) {
        if let Some(ConversationMessage::System(prompt)) = self.first().cloned() {
            self.clear();
            self.push(ConversationMessage::System(prompt));
        } else {
            self.clear();
            self.push(ConversationMessage::System(system_prompt().to_string()));
        }
    }
}
