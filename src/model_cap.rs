#[derive(Debug, Clone, Default)]
pub struct ModelCaps {
    pub think: bool,
    pub image: bool,
    pub audio: bool,
}

impl ModelCaps {
    pub fn from_ollama(caps: &[String]) -> Self {
        Self {
            think: caps.iter().any(|c| c == "thinking"),
            image: caps.iter().any(|c| c == "vision"),
            audio: caps.iter().any(|c| c == "audio"),
        }
    }

    pub fn describe(&self) -> String {
        let mut parts = vec!["text"];
        if self.image { parts.push("image"); }
        if self.audio { parts.push("audio"); }
        if self.think { parts.push("think"); }
        parts.join(", ")
    }
}
