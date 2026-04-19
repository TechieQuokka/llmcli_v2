use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::{
    input::{self, read_line, LineResult},
    media,
    model_cap::{self, ModelCaps},
    ollama::{Message, OllamaClient},
};

// ── ANSI colour helpers ───────────────────────────────────────────────────────
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";

fn fmt_size(bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    const MB: u64 = 1_000_000;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0}MB", bytes as f64 / MB as f64)
    }
}

fn print_info(msg: &str) {
    println!("{CYAN}{msg}{RESET}");
}
fn print_warn(msg: &str) {
    println!("{YELLOW}{msg}{RESET}");
}
fn print_err(msg: &str) {
    println!("{RED}{msg}{RESET}");
}

// ── Pending media for next message ────────────────────────────────────────────
#[derive(Default)]
struct PendingMedia {
    images: Vec<String>,  // base64
    text_chunks: Vec<String>, // file contents prepended to message
}

impl PendingMedia {
    fn clear(&mut self) {
        self.images.clear();
        self.text_chunks.clear();
    }
}

// ── Session ───────────────────────────────────────────────────────────────────
pub struct Session {
    client: Arc<OllamaClient>,
    model: String,
    caps: ModelCaps,
    think: bool,
    history: Vec<Message>,
    pending: PendingMedia,
    interrupted: Arc<AtomicBool>,
}

impl Session {
    pub fn new(
        client: Arc<OllamaClient>,
        model: String,
        think_init: bool,
        interrupted: Arc<AtomicBool>,
    ) -> Self {
        let caps = model_cap::resolve(&model);
        Self {
            client,
            model,
            caps,
            think: think_init,
            history: Vec::new(),
            pending: PendingMedia::default(),
            interrupted,
        }
    }

    /// Main REPL loop.
    pub async fn run(&mut self) {
        self.print_welcome();

        loop {
            if self.interrupted.load(Ordering::SeqCst) {
                self.interrupted.store(false, Ordering::SeqCst);
                println!();
                print_warn("^C — type /exit to quit.");
                continue;
            }

            let think_indicator = if self.think {
                format!("{MAGENTA}[think]{RESET} ")
            } else {
                String::new()
            };
            let prompt = format!(
                "{BOLD}{GREEN}{}{RESET} {think_indicator}{BOLD}>{RESET} ",
                self.model
            );

            match read_line(&prompt) {
                LineResult::Interrupted => {
                    print_warn("^C — type /exit to quit.");
                    continue;
                }
                LineResult::Eof => {
                    self.shutdown().await;
                    return;
                }
                LineResult::Line(line) => {
                    let line = line.trim().to_owned();
                    if line.is_empty() {
                        continue;
                    }
                    if line.starts_with('/') {
                        if self.handle_command(&line).await {
                            // /exit returned true
                            self.shutdown().await;
                            return;
                        }
                    } else {
                        self.handle_chat(line).await;
                    }
                }
            }
        }
    }

    // ── Command dispatcher ────────────────────────────────────────────────────

    /// Parse a media command argument that may use quoted paths.
    ///
    /// `/image '/path/with spaces/file.png' optional message here`
    ///           ^^^^^^^^^^^^^^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^^^^^^^^
    ///           path (quotes stripped)       optional inline message
    ///
    /// Without quotes the whole arg is treated as the path (original behaviour).
    fn parse_media_arg<'a>(arg: &'a str) -> (&'a str, Option<&'a str>) {
        let arg = arg.trim();
        if let Some(first) = arg.chars().next() {
            if first == '\'' || first == '"' {
                if let Some(end) = arg[1..].find(first) {
                    let path = &arg[1..end + 1];
                    let rest = arg[end + 2..].trim();
                    let msg = if rest.is_empty() { None } else { Some(rest) };
                    return (path, msg);
                }
            }
        }
        (arg, None)
    }

    /// Returns true when the session should exit.
    async fn handle_command(&mut self, line: &str) -> bool {
        let mut parts = line.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().unwrap_or("").trim();

        match cmd.as_str() {
            "/exit" | "/quit" => return true,

            "/help" => self.cmd_help(),

            "/info" => self.cmd_info(),

            "/think" => {
                if self.caps.think {
                    self.think = true;
                    print_info("[think mode ON]");
                } else {
                    print_warn(
                        "[warn] Current model does not support think mode."
                    );
                }
            }

            "/nothink" => {
                self.think = false;
                print_info("[think mode OFF]");
            }

            "/clear" => {
                // 1. Clear terminal first (erase screen + move cursor to top-left)
                print!("\x1b[2J\x1b[H");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                // 2. Clear conversation context
                self.history.clear();
                self.pending.clear();
                // 3. Reprint welcome header
                self.print_welcome();
            }

            "/model" => {
                if arg.is_empty() {
                    self.cmd_list_models().await;
                } else {
                    match arg.parse::<usize>() {
                        Ok(n) => self.cmd_switch_model_by_index(n).await,
                        Err(_) => print_err(
                            "Usage: /model          — list models\n       /model <number>  — switch by index",
                        ),
                    }
                }
            }

            "/image" => {
                if arg.is_empty() {
                    print_err("Usage: /image <path>  or  /image '<path with spaces>' [message]");
                } else if !self.caps.image {
                    print_warn("[warn] Current model does not support images.");
                } else {
                    let (path, msg) = Self::parse_media_arg(arg);
                    self.cmd_attach_image(path);
                    if let Some(m) = msg {
                        self.handle_chat(m.to_owned()).await;
                    }
                }
            }

            "/audio" => {
                if arg.is_empty() {
                    print_err("Usage: /audio <path>  or  /audio '<path with spaces>' [message]");
                } else if !self.caps.audio {
                    print_warn("[warn] Current model does not support audio.");
                } else {
                    let (path, msg) = Self::parse_media_arg(arg);
                    self.cmd_attach_audio(path);
                    if let Some(m) = msg {
                        self.handle_chat(m.to_owned()).await;
                    }
                }
            }

            "/file" => {
                if arg.is_empty() {
                    print_err("Usage: /file <path>  or  /file '<path with spaces>' [message]");
                } else {
                    let (path, msg) = Self::parse_media_arg(arg);
                    self.cmd_attach_file(path);
                    if let Some(m) = msg {
                        self.handle_chat(m.to_owned()).await;
                    }
                }
            }

            other => {
                print_err(&format!("Unknown command: {other}  (type /help)"));
            }
        }

        false
    }

    fn cmd_help(&self) {
        println!(
            "\n{BOLD}Commands:{RESET}
  {CYAN}/help{RESET}            Show this help
  {CYAN}/info{RESET}            Show current model capabilities
  {CYAN}/model{RESET}           List available models
  {CYAN}/model <number>{RESET}  Switch to model by index
  {CYAN}/think{RESET}           Enable think mode
  {CYAN}/nothink{RESET}         Disable think mode {DIM}(default){RESET}
  {CYAN}/image <path>{RESET}            Attach image to next message
  {CYAN}/image '<path>' [msg]{RESET}  Attach image and send message immediately
  {CYAN}/audio <path>{RESET}            Attach audio to next message
  {CYAN}/audio '<path>' [msg]{RESET}  Attach audio and send message immediately
  {CYAN}/file  <path>{RESET}            Attach text file to next message
  {CYAN}/file  '<path>' [msg]{RESET}  Attach file and send message immediately
  {CYAN}/clear{RESET}           Clear conversation history
  {CYAN}/exit{RESET}            Unload model and quit\n"
        );
    }

    fn cmd_info(&self) {
        println!(
            "\n{BOLD}Model :{RESET} {GREEN}{}{RESET}",
            self.model
        );
        println!(
            "{BOLD}Caps  :{RESET} {}",
            self.caps.describe()
        );
        println!(
            "{BOLD}Think :{RESET} {}",
            if self.think { "ON" } else { "OFF" }
        );
        println!(
            "{BOLD}History:{RESET} {} messages\n",
            self.history.len()
        );
    }

    async fn cmd_list_models(&self) {
        match self.client.list_models().await {
            Ok(models) => {
                if models.is_empty() {
                    print_warn("[no models available]");
                    return;
                }
                println!("\n{BOLD}Available models:{RESET}");
                for (i, (name, size)) in models.iter().enumerate() {
                    let caps = model_cap::resolve(name);
                    let marker = if *name == self.model {
                        format!("{GREEN}*{RESET}")
                    } else {
                        " ".to_string()
                    };
                    println!(
                        "  {} {:>2})  {:<40}  {:>7}  [{}]",
                        marker, i + 1, name, fmt_size(*size), caps.describe()
                    );
                }
                println!();
            }
            Err(e) => print_err(&format!("[error] {e}")),
        }
    }

    async fn cmd_switch_model_by_index(&mut self, n: usize) {
        let models = match self.client.list_models().await {
            Ok(m) => m,
            Err(e) => {
                print_err(&format!("[error] {e}"));
                return;
            }
        };
        if n < 1 || n > models.len() {
            print_err(&format!("[error] Index out of range (1–{})", models.len()));
            return;
        }
        let new_model = models[n - 1].0.clone();
        self.cmd_switch_model(&new_model).await;
    }

    async fn cmd_switch_model(&mut self, new_model: &str) {
        if new_model == self.model {
            print_warn("[warn] Already using that model.");
            return;
        }
        print_info(&format!("[unloading {}...]", self.model));
        self.client.unload_model(&self.model).await;

        self.model = new_model.to_owned();
        self.caps = model_cap::resolve(&self.model);
        self.think = false;
        self.history.clear();
        self.pending.clear();

        print_info(&format!(
            "[switched to {}  caps: {}]",
            self.model,
            self.caps.describe()
        ));
    }

    fn cmd_attach_image(&mut self, path: &str) {
        match media::load_image(path) {
            Ok(img) => {
                self.pending.images.push(img.base64);
                print_info(&format!("[image attached: {path}]"));
            }
            Err(e) => print_err(&format!("[error] {e}")),
        }
    }

    fn cmd_attach_audio(&mut self, path: &str) {
        match media::load_audio(path) {
            Ok(audio) => {
                self.pending.images.push(audio.base64);
                print_info(&format!("[audio attached: {path}]"));
            }
            Err(e) => print_err(&format!("[error] {e}")),
        }
    }

    fn cmd_attach_file(&mut self, path: &str) {
        match media::load_text_file(path) {
            Ok(contents) => {
                // Wrap in a fenced block so the model understands the boundary
                let ext = std::path::Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("txt");
                let block = format!("```{ext}\n# file: {path}\n{contents}\n```");
                self.pending.text_chunks.push(block);
                print_info(&format!("[file attached: {path}]"));
            }
            Err(e) => print_err(&format!("[error] {e}")),
        }
    }

    // ── Chat ──────────────────────────────────────────────────────────────────

    async fn handle_chat(&mut self, user_input: String) {
        // Build content: prepend any text file chunks, then user message
        let content = if self.pending.text_chunks.is_empty() {
            user_input
        } else {
            let mut parts = self.pending.text_chunks.join("\n\n");
            parts.push_str("\n\n");
            parts.push_str(&user_input);
            parts
        };

        // Build user message; attach images if any
        let user_msg = Message {
            role: "user".into(),
            content,
            images: if self.pending.images.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.pending.images))
            },
        };
        self.pending.clear();

        self.history.push(user_msg);

        // ── Streaming response ───────────────────────────────────────────────
        let think = self.think;

        // Watch stdin for ESC during streaming; dropped (joined) after streaming ends.
        #[cfg(unix)]
        let _esc_monitor = input::EscMonitor::start(self.interrupted.clone());

        // think_shown: print [thinking...] only once
        let mut think_shown = false;

        let result = self
            .client
            .chat_stream(
                &self.model,
                &self.history,
                think,
                self.interrupted.clone(),
                |token| {
                    // on_think callback — each closure gets its own stdout handle
                    let mut out = io::stdout();
                    if token == "\x1b[2K\r" {
                        let _ = out.write_all(b"\r\x1b[2K");
                        let _ = out.flush();
                        think_shown = false;
                    } else if !think_shown {
                        let _ = write!(out, "{DIM}{MAGENTA}{token}{RESET}");
                        let _ = out.flush();
                        think_shown = true;
                    }
                },
                |token| {
                    // on_content callback — stream tokens live
                    let mut out = io::stdout();
                    let _ = out.write_all(token.as_bytes());
                    let _ = out.flush();
                },
            )
            .await;

        println!(); // newline after streamed response

        // If ESC interrupted, clear the flag and skip saving to history
        if self.interrupted.load(Ordering::SeqCst) {
            self.interrupted.store(false, Ordering::SeqCst);
            print_warn("[interrupted]");
            self.history.pop(); // remove the user message that got no full reply
            return;
        }

        match result {
            Ok(content) => {
                self.history.push(Message::assistant(content));
            }
            Err(e) => {
                print_err(&format!("[error] {e}"));
                // Pop the failed user message so history stays consistent
                self.history.pop();
            }
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    async fn shutdown(&self) {
        print_info(&format!("\n[unloading {}...]", self.model));
        self.client.unload_model(&self.model).await;
        print_info("[bye]");
    }

    fn print_welcome(&self) {
        println!(
            "\n{BOLD}llmcli{RESET} v{}  — Ollama frontend\n\
             model : {GREEN}{}{RESET}\n\
             caps  : {}\n\
             type  {CYAN}/help{RESET} for commands\n",
            env!("CARGO_PKG_VERSION"),
            self.model,
            self.caps.describe()
        );
    }
}
