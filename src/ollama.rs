use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "http://localhost:11434";

// ── Request / Response types ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>, // base64 strings
}

impl Message {
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into(), images: None }
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
        mut on_think: FT,
        mut on_content: FC,
    ) -> Result<String>
    where
        FT: FnMut(&str),
        FC: FnMut(&str),
    {
        let body = ChatRequest { model, messages, think, stream: true, keep_alive: -1 };

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

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
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
                    // thinking field present → we are in reasoning trace
                    if let Some(ref t) = msg.thinking {
                        if !t.is_empty() {
                            if !thinking_started {
                                thinking_started = true;
                                in_thinking = true;
                                on_think("[thinking...]");
                            }
                            // We intentionally do NOT forward raw thinking tokens
                            // to keep output compact (user requested summary-style)
                        }
                    }

                    // content field present → final answer
                    if let Some(ref c) = msg.content {
                        if !c.is_empty() {
                            if in_thinking {
                                // Transition: thinking → answer
                                in_thinking = false;
                                on_think("\x1b[2K\r"); // clear the [thinking...] line
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

    /// Fetch list of locally available models
    pub async fn list_models(&self) -> Result<Vec<(String, u64)>> {
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

        Ok(resp.models.into_iter().map(|e| (e.name, e.size)).collect())
    }
}
