mod input;
mod media;
mod model_cap;
mod ollama;
mod session;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::{bail, Result};

const HELP: &str = "\
Usage:
  llmcli [OPTIONS] [MODEL]

Arguments:
  MODEL           Model name (e.g. gemma4:e4b, qwen3:14b)
                  Omit to list available models and choose interactively.

Options:
  --think         Start with think mode enabled
  -h, --help      Show this help and exit

Slash commands (inside the chat):
  /help           List all commands
  /info           Show model capabilities
  /model <n>      Switch model
  /think          Enable think mode
  /nothink        Disable think mode (default)
  /image <path>   Attach image (multimodal models)
  /audio <path>   Attach audio (audio-capable models)
  /file  <path>   Attach text file (all models)
  /clear          Clear conversation history
  /exit           Unload model and quit
";

#[derive(Default)]
struct Args {
    model: Option<String>,
    think: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1).peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", HELP);
                std::process::exit(0);
            }
            "--think" => {
                args.think = true;
            }
            a if a.starts_with('-') => {
                bail!("Unknown option: {a}\nRun with --help for usage.");
            }
            _ => {
                if args.model.is_none() {
                    args.model = Some(arg);
                }
            }
        }
    }

    Ok(args)
}

fn fmt_size(bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    const MB: u64 = 1_000_000;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.0}MB", bytes as f64 / MB as f64)
    }
}

/// Interactive model selector: list available models, ask user to pick.
async fn pick_model(client: &ollama::OllamaClient) -> Result<String> {
    let models = client.list_models().await?;
    if models.is_empty() {
        bail!("No models found. Pull a model first: ollama pull <model>");
    }

    println!("\nAvailable models:");
    for (i, (name, size)) in models.iter().enumerate() {
        let caps = model_cap::resolve(name);
        println!("  {:>2})  {:<40}  {:>7}  [{}]", i + 1, name, fmt_size(*size), caps.describe());
    }
    println!();

    loop {
        let result = input::read_line("Select model (number or name): ");
        match result {
            input::LineResult::Line(s) => {
                let s = s.trim().to_owned();
                if s.is_empty() {
                    continue;
                }
                if let Ok(n) = s.parse::<usize>() {
                    if n >= 1 && n <= models.len() {
                        return Ok(models[n - 1].0.clone());
                    }
                    println!("  Out of range.");
                    continue;
                }
                let names: Vec<&str> = models.iter().map(|(n, _)| n.as_str()).collect();
                if names.contains(&s.as_str()) {
                    return Ok(s);
                }
                let matches: Vec<_> = names.iter().filter(|m| m.contains(&s)).collect();
                match matches.len() {
                    0 => println!("  No model matching '{s}'."),
                    1 => return Ok(matches[0].to_string()),
                    _ => {
                        println!("  Ambiguous — matches:");
                        for m in &matches {
                            println!("    {m}");
                        }
                    }
                }
            }
            _ => bail!("Aborted."),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;

    let client = Arc::new(ollama::OllamaClient::new());

    let model = match args.model {
        Some(m) => m,
        None => pick_model(&client).await?,
    };

    // Atomic flag for Ctrl-C: lets the REPL loop handle it cleanly
    // instead of aborting a streaming response mid-output.
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        })
        .expect("Failed to install Ctrl-C handler");
    }

    let mut sess = session::Session::new(client, model, args.think, interrupted);
    sess.run().await;

    Ok(())
}
