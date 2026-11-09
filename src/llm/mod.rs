mod types;

use std::time::Duration;

use async_openai::config::OpenAIConfig;
use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageArgs,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessageArgs, ChatCompletionTool,
    ChatCompletionToolType, CreateChatCompletionRequestArgs, FunctionCall, FunctionObject,
};
use async_openai::Client;
use reqwest::Client as HttpClient;

use crate::config::AppConfig;
use crate::utils::AppError;

pub use types::{
    argument_field, normalize_tool_calls, parse_textual_tool_calls, system_prompt,
    ConversationMessage, LlmResponse, ToolCall, TOOL_BASH, TOOL_PYTHON,
};

pub struct LlmClient {
    client: Client<OpenAIConfig>,
    model: String,
    temperature: f32,
    max_tokens: u16,
    tools: Vec<ChatCompletionTool>,
}

impl LlmClient {
    pub fn new(config: &AppConfig) -> Result<Self, AppError> {
        let http = HttpClient::builder()
            .timeout(Duration::from_secs(config.llm_timeout_secs))
            .user_agent("uncensored-grok-bot/0.1")
            .build()
            .map_err(|err| AppError::Llm(format!("cliente HTTP: {err}")))?;

        let openai = OpenAIConfig::new()
            .with_api_key(&config.llm_api_key)
            .with_api_base(&config.llm_base_url);

        let client = Client::with_config(openai).with_http_client(http);
        let max_tokens = u16::try_from(config.llm_max_tokens).unwrap_or(u16::MAX);

        Ok(Self {
            client,
            model: config.llm_model.clone(),
            temperature: config.llm_temperature,
            max_tokens,
            tools: agent_tools(),
        })
    }

    pub async fn complete(&self, history: &[ConversationMessage]) -> Result<LlmResponse, AppError> {
        let messages = history
            .iter()
            .map(to_api_message)
            .collect::<Result<Vec<_>, _>>()?;

        let request = CreateChatCompletionRequestArgs::default()
            .model(self.model.clone())
            .messages(messages)
            .tools(self.tools.clone())
            .temperature(self.temperature)
            .max_tokens(self.max_tokens)
            .build()
            .map_err(|err| AppError::Llm(format!("pedido inválido: {err}")))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|err| AppError::Llm(err.to_string()))?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Llm("a API devolveu uma resposta sem choices".into()))?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|call| ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: call.function.arguments,
            })
            .collect();

        Ok(LlmResponse {
            content: choice.message.content,
            tool_calls,
        })
    }
}

fn agent_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            types::TOOL_BASH,
            "Executa um comando de shell dentro da sandbox isolada e devolve stdout, stderr e o código de saída.",
            types::bash_parameters(),
        ),
        tool(
            types::TOOL_PYTHON,
            "Executa código Python dentro da sandbox isolada e devolve stdout, stderr e o código de saída.",
            types::python_parameters(),
        ),
    ]
}

fn tool(name: &str, description: &str, parameters: serde_json::Value) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: name.to_string(),
            description: Some(description.to_string()),
            parameters: Some(parameters),
            strict: Some(false),
        },
    }
}

fn to_api_message(message: &ConversationMessage) -> Result<ChatCompletionRequestMessage, AppError> {
    match message {
        ConversationMessage::System(content) => {
            let message = ChatCompletionRequestSystemMessageArgs::default()
                .content(content.clone())
                .build()
                .map_err(|err| AppError::Llm(err.to_string()))?;
            Ok(ChatCompletionRequestMessage::System(message))
        }
        ConversationMessage::User(content) => {
            let message = ChatCompletionRequestUserMessageArgs::default()
                .content(content.clone())
                .build()
                .map_err(|err| AppError::Llm(err.to_string()))?;
            Ok(ChatCompletionRequestMessage::User(message))
        }
        ConversationMessage::Assistant {
            content,
            tool_calls,
        } => {
            let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
            if let Some(content) = content {
                builder.content(content.clone());
            }
            if !tool_calls.is_empty() {
                builder.tool_calls(tool_calls.iter().map(to_api_tool_call).collect::<Vec<_>>());
            }
            let message = builder
                .build()
                .map_err(|err| AppError::Llm(err.to_string()))?;
            Ok(ChatCompletionRequestMessage::Assistant(message))
        }
        ConversationMessage::Tool {
            tool_call_id,
            content,
        } => {
            let message = ChatCompletionRequestToolMessageArgs::default()
                .tool_call_id(tool_call_id.clone())
                .content(content.clone())
                .build()
                .map_err(|err| AppError::Llm(err.to_string()))?;
            Ok(ChatCompletionRequestMessage::Tool(message))
        }
    }
}

fn to_api_tool_call(call: &ToolCall) -> ChatCompletionMessageToolCall {
    ChatCompletionMessageToolCall {
        id: call.id.clone(),
        r#type: ChatCompletionToolType::Function,
        function: FunctionCall {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        },
    }
}
