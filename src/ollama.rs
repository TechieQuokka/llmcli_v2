use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

use crate::model_cap::ModelCaps;

const BASE_URL: &str = "http://localhost:11434";

// ── Request / Response types ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>, // base64 strings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audios: Option<Vec<String>>, // base64 strings
}

impl Message {
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            images: None,
            audios: None,
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    think: bool,
    stream: bool,
    keep_alive: i32,
}

#[derive(Serialize)]
struct UnloadRequest<'a> {
    model: &'a str,
    messages: &'a [Message; 0],
    keep_alive: i32,
}

#[derive(Serialize)]
struct ShowRequest<'a> {
    model: &'a str,
}

#[derive(Deserialize)]
struct ShowResp {
    capabilities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    message: Option<ChunkMessage>,
    done: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ChunkMessage {
    content: Option<String>,
    thinking: Option<String>,
}

// ── Public API surface ────────────────────────────────────────────────────────

pub struct OllamaClient {
    client: Client,
}

impl OllamaClient {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("Failed to build HTTP client"),
        }
    }

    /// Stream a chat completion.
    /// Calls `on_think` for each thinking token, `on_content` for each answer token.
    pub async fn chat_stream<FT, FC>(
        &self,
        model: &str,
        messages: &[Message],
        think: bool,
        interrupted: Arc<AtomicBool>,
        mut on_think: FT,
        mut on_content: FC,
    ) -> Result<String>
    where
        FT: FnMut(&str),
        FC: FnMut(&str),
    {
        let body = ChatRequest { model, messages, think, stream: true, keep_alive: 600 };

        let body_json = serde_json::to_string(&body)
            .map_err(|e| anyhow::anyhow!("serialize error: {e}"))?;

        let resp = self.client
            .post(format!("{BASE_URL}/api/chat"))
            .header("Content-Type", "application/json")
            .body(body_json)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Ollama API error {status}: {text}");
        }

        let mut stream = resp.bytes_stream();
        let mut full_content = String::new();
        let mut in_thinking = false;
        let mut thinking_started = false;
        let mut buf = Vec::new();

        let interrupt_fut = async {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
                if interrupted.load(Ordering::SeqCst) { return; }
            }
        };
        tokio::pin!(interrupt_fut);

        loop {
            let chunk = tokio::select! {
                c = stream.next() => match c {
                    Some(v) => v?,
                    None => break,
                },
                _ = &mut interrupt_fut => break,
            };
            buf.extend_from_slice(&chunk);

            // Process line-delimited JSON chunks
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = line.trim_ascii();
                if line.is_empty() { continue; }

                let parsed: StreamChunk = match serde_json::from_slice(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(msg) = parsed.message {
                    if let Some(ref t) = msg.thinking {
                        if !t.is_empty() {
                            if !thinking_started {
                                thinking_started = true;
                                in_thinking = true;
                                on_think("[thinking...]");
                            }
                        }
                    }

                    if let Some(ref c) = msg.content {
                        if !c.is_empty() {
                            if in_thinking {
                                in_thinking = false;
                                on_think("\x1b[2K\r");
                            }
                            on_content(c);
                            full_content.push_str(c);
                        }
                    }
                }

                if parsed.done.unwrap_or(false) {
                    break;
                }
            }
        }

        Ok(full_content)
    }

    /// Instantly unload a model by sending keep_alive=0
    pub async fn unload_model(&self, model: &str) {
        let body = UnloadRequest { model, messages: &[], keep_alive: 0 };
        let _ = self.client
            .post(format!("{BASE_URL}/api/chat"))
            .json(&body)
            .send()
            .await;
    }

    /// Fetch capabilities for a single model via /api/show.
    pub async fn model_caps(&self, model: &str) -> ModelCaps {
        self.fetch_show(model).await.unwrap_or_default()
    }

    /// Fetch list of chat-capable models with their capabilities.
    /// Embedding-only models are excluded.
    /// /api/show is called for each model in parallel.
    pub async fn list_models(&self) -> Result<Vec<(String, u64, ModelCaps)>> {
        #[derive(Deserialize)]
        struct ListResp { models: Vec<ModelEntry> }
        #[derive(Deserialize)]
        struct ModelEntry { name: String, size: u64 }

        let resp: ListResp = self.client
            .get(format!("{BASE_URL}/api/tags"))
            .send()
            .await?
            .json()
            .await?;

        let names_sizes: Vec<(String, u64)> =
            resp.models.into_iter().map(|e| (e.name, e.size)).collect();

        let cap_futs = names_sizes.iter().map(|(name, _)| self.fetch_show(name));
        let show_results = futures_util::future::join_all(cap_futs).await;

        let models = names_sizes.into_iter()
            .zip(show_results)
            .filter_map(|((name, size), caps)| caps.map(|c| (name, size, c)))
            .collect();

        Ok(models)
    }

    /// Call /api/show and parse capabilities.
    /// Returns None if the model is not chat-capable (e.g. embedding-only).
    async fn fetch_show(&self, model: &str) -> Option<ModelCaps> {
        let resp = self.client
            .post(format!("{BASE_URL}/api/show"))
            .json(&ShowRequest { model })
            .send()
            .await
            .ok()?;

        let show: ShowResp = resp.json().await.ok()?;
        let caps_list = show.capabilities.unwrap_or_default();

        // Exclude models that explicitly declare no completion capability
        if !caps_list.is_empty() && !caps_list.iter().any(|c| c == "completion") {
            return None;
        }

        Some(ModelCaps::from_ollama(&caps_list))
    }
}
