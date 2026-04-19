/// Model capability flags
#[derive(Debug, Clone)]
pub struct ModelCaps {
    pub think: bool,
    pub image: bool,
    pub audio: bool,
}

impl ModelCaps {
    pub const fn new(think: bool, image: bool, audio: bool) -> Self {
        Self { think, image, audio }
    }
}

/// Resolve capabilities for a given model name.
/// Matching is prefix/substring based so tag variants (e.g. :e4b, :latest) are covered.
pub fn resolve(model: &str) -> ModelCaps {
    let m = model.to_lowercase();

    // ── Audio + Image + Think ────────────────────────────────────────────────
    if m.contains("gemma4") || m.contains("gemma3n") {
        return ModelCaps::new(true, true, true);
    }

    // ── Image + Think ────────────────────────────────────────────────────────
    if m.contains("gemma3") {
        return ModelCaps::new(true, true, false);
    }
    if m.contains("glm-ocr") || m.contains("deepseek-ocr") {
        return ModelCaps::new(false, true, false);
    }

    // ── Think only ───────────────────────────────────────────────────────────
    if m.contains("qwen3") || m.contains("qwen3.5") {
        return ModelCaps::new(true, false, false);
    }
    if m.contains("deepseek-r1") {
        return ModelCaps::new(true, false, false);
    }
    if m.contains("phi4-reasoning") {
        return ModelCaps::new(true, false, false);
    }
    if m.contains("phi4") {
        return ModelCaps::new(true, false, false);
    }
    if m.contains("huihui") || m.contains("abliterated") {
        // abliterated gemma4 variants — keep multimodal
        return ModelCaps::new(true, true, true);
    }
    if m.contains("fredrezones") {
        return ModelCaps::new(true, true, true);
    }

    // ── Text only (no special caps) ──────────────────────────────────────────
    // qwen2.5-coder, exaone, medgemma, functiongemma, etc.
    ModelCaps::new(false, false, false)
}

impl ModelCaps {
    pub fn describe(&self) -> String {
        let mut caps = vec!["text"];
        if self.image { caps.push("image"); }
        if self.audio { caps.push("audio"); }
        if self.think { caps.push("think"); }
        caps.join(", ")
    }
}
