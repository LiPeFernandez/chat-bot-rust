use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::llm::{system_prompt, ConversationMessage};
use crate::utils::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiItem {
    User {
        content: String,
    },
    Note {
        content: String,
    },
    Tool {
        id: String,
        name: String,
        arguments: String,
        output: String,
        timed_out: bool,
        pending: bool,
    },
    Assistant {
        content: String,
    },
    Error {
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    title: String,
    updated_at: i64,
    items: Vec<UiItem>,
    history: Vec<ConversationMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreFile {
    current: String,
    sessions: Vec<Session>,
}

pub struct SessionStore {
    path: PathBuf,
    current: String,
    sessions: Vec<Session>,
}

impl SessionStore {
    pub fn load() -> Result<Self, AppError> {
        let path = PathBuf::from("data/sessions.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                AppError::Config(format!("não criei a pasta de conversas: {err}"))
            })?;
        }
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .map_err(|err| AppError::Config(format!("não li as conversas salvas: {err}")))?;
            let file: StoreFile = serde_json::from_str(&raw)
                .map_err(|err| AppError::Config(format!("conversas salvas inválidas: {err}")))?;
            if file.sessions.is_empty() {
                return Ok(Self::fresh(path));
            }
            let current = if file
                .sessions
                .iter()
                .any(|session| session.id == file.current)
            {
                file.current
            } else {
                file.sessions[0].id.clone()
            };
            return Ok(Self {
                path,
                current,
                sessions: file.sessions,
            });
        }
        let store = Self::fresh(path);
        store.save()?;
        Ok(store)
    }

    fn fresh(path: PathBuf) -> Self {
        let session = blank_session();
        Self {
            path,
            current: session.id.clone(),
            sessions: vec![session],
        }
    }

    pub fn current_id(&self) -> &str {
        &self.current
    }

    pub fn metas(&self) -> Vec<SessionMeta> {
        let mut sessions = self.sessions.clone();
        sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        sessions
            .into_iter()
            .map(|session| SessionMeta {
                id: session.id,
                title: session.title,
                updated_at: session.updated_at,
            })
            .collect()
    }

    pub fn current_items(&self) -> Vec<UiItem> {
        self.current_session().items.clone()
    }

    pub fn current_history(&self) -> Vec<ConversationMessage> {
        self.current_session().history.clone()
    }

    pub fn create(&mut self) -> Result<(), AppError> {
        let session = blank_session();
        self.current = session.id.clone();
        self.sessions.push(session);
        self.save()
    }

    pub fn open(&mut self, id: &str) -> Result<(), AppError> {
        if !self.sessions.iter().any(|session| session.id == id) {
            return Err(AppError::Config("conversa não encontrada".into()));
        }
        self.current = id.to_string();
        self.save()
    }

    pub fn delete(&mut self, id: &str) -> Result<(), AppError> {
        self.sessions.retain(|session| session.id != id);
        if self.sessions.is_empty() {
            let session = blank_session();
            self.current = session.id.clone();
            self.sessions.push(session);
        } else if !self
            .sessions
            .iter()
            .any(|session| session.id == self.current)
        {
            self.current = self
                .sessions
                .iter()
                .max_by_key(|session| session.updated_at)
                .map(|session| session.id.clone())
                .unwrap_or_else(|| self.sessions[0].id.clone());
        }
        self.save()
    }

    pub fn push_user(&mut self, display: &str) -> Result<(), AppError> {
        {
            let session = self.current_session_mut();
            if session.title == "Nova conversa" {
                session.title = title_from(display);
            }
            session.updated_at = now();
            session.items.push(UiItem::User {
                content: display.to_string(),
            });
        }
        self.save()
    }

    pub fn push_item(&mut self, item: UiItem) -> Result<(), AppError> {
        {
            let session = self.current_session_mut();
            session.updated_at = now();
            session.items.push(item);
        }
        self.save()
    }

    pub fn finish_tool(&mut self, id: &str, output: &str, timed_out: bool) -> Result<(), AppError> {
        {
            let session = self.current_session_mut();
            if let Some(UiItem::Tool {
                output: slot,
                timed_out: flag,
                pending,
                ..
            }) = session
                .items
                .iter_mut()
                .rev()
                .find(|item| matches!(item, UiItem::Tool { id: tool_id, .. } if tool_id == id))
            {
                *slot = output.to_string();
                *flag = timed_out;
                *pending = false;
            }
            session.updated_at = now();
        }
        self.save()
    }

    pub fn set_history(&mut self, history: Vec<ConversationMessage>) -> Result<(), AppError> {
        {
            let session = self.current_session_mut();
            session.history = history;
            session.updated_at = now();
        }
        self.save()
    }

    fn current_session(&self) -> &Session {
        self.sessions
            .iter()
            .find(|session| session.id == self.current)
            .expect("conversa atual")
    }

    fn current_session_mut(&mut self) -> &mut Session {
        let current = self.current.clone();
        self.sessions
            .iter_mut()
            .find(|session| session.id == current)
            .expect("conversa atual")
    }

    fn save(&self) -> Result<(), AppError> {
        let file = StoreFile {
            current: self.current.clone(),
            sessions: self.sessions.clone(),
        };
        let raw = serde_json::to_string_pretty(&file)
            .map_err(|err| AppError::Config(format!("não serializei as conversas: {err}")))?;
        fs::write(&self.path, raw)
            .map_err(|err| AppError::Config(format!("não salvei as conversas: {err}")))
    }
}

fn blank_session() -> Session {
    Session {
        id: next_id(),
        title: "Nova conversa".into(),
        updated_at: now(),
        items: Vec::new(),
        history: vec![ConversationMessage::System(system_prompt().to_string())],
    }
}

fn title_from(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    let mut title: String = line.chars().take(48).collect();
    if line.chars().count() > 48 {
        title.push('…');
    }
    if title.is_empty() {
        "Nova conversa".into()
    } else {
        title
    }
}

fn next_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("chat-{}-{}", now(), NEXT.fetch_add(1, Ordering::Relaxed))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
