# llmcli

A minimal, fast terminal frontend for [Ollama](https://ollama.com) written in Rust.

## Features

- **Streaming responses** — tokens printed live as they arrive
- **Multimodal support** — attach images, audio, and text files to messages
- **Think mode** — shows `[thinking...]` indicator for reasoning models
- **Model switching** — list and switch models by index without leaving the chat
- **Quoted path support** — handles file paths with spaces using single or double quotes
- **Inline media + message** — attach a file and send a message in one command
- **Zero TUI dependencies** — custom raw-mode line editor via `libc` termios

## Requirements

- [Ollama](https://ollama.com) running locally (`http://localhost:11434`)
- Rust 1.85+ (edition 2024)

## Build

```bash
cargo build --release
# binary: ./target/release/llmcli
```

## Usage

```bash
# Interactive model selection
llmcli

# Start with a specific model
llmcli gemma4:e4b

# Start with think mode enabled
llmcli --think qwen3:14b
```

## Slash Commands

| Command | Description |
|---------|-------------|
| `/help` | Show all commands |
| `/info` | Show current model and capabilities |
| `/model` | List available models with size |
| `/model <n>` | Switch to model by index |
| `/think` | Enable think mode |
| `/nothink` | Disable think mode |
| `/image <path>` | Attach image to next message |
| `/image '<path>' [msg]` | Attach image and send message immediately |
| `/audio <path>` | Attach audio to next message |
| `/audio '<path>' [msg]` | Attach audio and send message immediately |
| `/file <path>` | Attach text file to next message |
| `/file '<path>' [msg]` | Attach file and send message immediately |
| `/clear` | Clear conversation history and screen |
| `/exit` | Unload model and quit |

## Supported File Formats

**Images:** `jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `tiff`, `avif`, `heic`, `ico`, `svg`  
**Audio:** `mp3`, `wav`, `ogg`, `flac`, `m4a`, `aac`, `opus`, `aiff`, `wma`, `amr`, `midi`  
**Text:** any file extension

Files with unknown or missing extensions are identified by magic bytes.

## Model Capabilities

Model capabilities are resolved from the model name prefix:

| Model | Capabilities |
|-------|-------------|
| `gemma4`, `gemma3n` | text, image, audio, think |
| `gemma3` | text, image, think |
| `qwen3`, `deepseek-r1`, `phi4` | text, think |
| others | text |

## Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime |
| `reqwest` | HTTP streaming to Ollama API |
| `serde` / `serde_json` | JSON serialization |
| `base64` | Image / audio encoding |
| `futures-util` | Stream processing |
| `anyhow` | Error handling |
| `ctrlc` | Ctrl-C signal handling |
| `libc` | Raw terminal mode |
