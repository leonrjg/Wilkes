use crate::generate::GenerationRequest;

/// Everything about a model's text protocol that sits outside the visible
/// answer. Tasks never branch on this: they provide plain instructions and
/// constrain only the answer text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModelFamily {
    Qwen3,
    Gemma3,
}

impl ModelFamily {
    pub(super) fn frame_prompt(self, request: &GenerationRequest) -> String {
        match self {
            Self::Qwen3 => frame_qwen3(request),
            Self::Gemma3 => frame_gemma3(request),
        }
    }

    pub(super) fn eos_tokens(self) -> &'static [&'static str] {
        match self {
            Self::Qwen3 => &["<|im_end|>", "<|endoftext|>"],
            Self::Gemma3 => &["<end_of_turn>", "<eos>"],
        }
    }

    pub(super) fn visible_text<'a>(self, raw: &'a str) -> &'a str {
        match self {
            Self::Qwen3 => strip_qwen3_think_preamble(raw),
            Self::Gemma3 => raw.trim(),
        }
    }
}

/// Qwen3's non-thinking template pre-fills the empty reasoning block in the
/// assistant turn. Leaving the model to emit it would put protocol tokens in
/// front of the task answer, where an answer grammar must reject them.
fn frame_qwen3(request: &GenerationRequest) -> String {
    let mut framed = String::new();
    if let Some(system) = request.system.as_deref() {
        framed.push_str("<|im_start|>system\n");
        framed.push_str(system);
        framed.push_str("<|im_end|>\n");
    }
    framed.push_str("<|im_start|>user\n");
    framed.push_str(&request.prompt);
    framed.push_str(" /no_think<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n");
    framed
}

/// Gemma 3 instruction models have only `user` and `model` roles. A system
/// instruction therefore belongs at the start of the user turn rather than in
/// a made-up role the model was not trained on.
fn frame_gemma3(request: &GenerationRequest) -> String {
    let mut framed = String::from("<start_of_turn>user\n");
    if let Some(system) = request.system.as_deref() {
        framed.push_str(system);
        framed.push_str("\n\n");
    }
    framed.push_str(&request.prompt);
    framed.push_str("<end_of_turn>\n<start_of_turn>model\n");
    framed
}

/// Defensive for unconstrained tasks and old prompt frames. With the current
/// prefill this is normally an identity operation because the empty block is
/// already part of the prompt, not the generated text.
fn strip_qwen3_think_preamble(text: &str) -> &str {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix("<think>") else {
        return text.trim();
    };
    match rest.split_once("</think>") {
        Some((_, after)) => after.trim(),
        // An unterminated block means the model never got to the answer.
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{Constraint, Sampling};

    fn request(system: Option<&str>) -> GenerationRequest {
        GenerationRequest {
            system: system.map(str::to_string),
            prompt: "label these".to_string(),
            max_tokens: 8,
            constraint: Constraint::Text { stop: Vec::new() },
            sampling: Sampling::default(),
        }
    }

    #[test]
    fn qwen_prefills_the_empty_think_block_before_generation() {
        let framed = ModelFamily::Qwen3.frame_prompt(&request(None));
        assert!(framed.contains("/no_think"), "{framed}");
        assert!(
            framed.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"),
            "{framed}"
        );
        assert!(!framed.contains("<|im_start|>system"));
    }

    #[test]
    fn qwen_supports_a_real_system_turn() {
        let framed = ModelFamily::Qwen3.frame_prompt(&request(Some("be terse")));
        assert!(
            framed.starts_with("<|im_start|>system\nbe terse<|im_end|>\n"),
            "{framed}"
        );
    }

    #[test]
    fn gemma_merges_system_text_into_the_user_turn() {
        let framed = ModelFamily::Gemma3.frame_prompt(&request(Some("be terse")));
        assert_eq!(
            framed,
            "<start_of_turn>user\nbe terse\n\nlabel these<end_of_turn>\n\
             <start_of_turn>model\n"
        );
        assert!(!framed.contains("system"));
    }

    #[test]
    fn each_family_owns_its_eos_tokens() {
        assert!(ModelFamily::Qwen3.eos_tokens().contains(&"<|im_end|>"));
        assert!(ModelFamily::Gemma3.eos_tokens().contains(&"<end_of_turn>"));
    }

    #[test]
    fn qwen_hidden_output_is_removed_at_the_protocol_boundary() {
        assert_eq!(
            ModelFamily::Qwen3.visible_text("<think>\n\n</think>\n\nCache invalidation"),
            "Cache invalidation"
        );
        assert_eq!(
            ModelFamily::Qwen3.visible_text("  Plain answer  "),
            "Plain answer"
        );
        assert_eq!(ModelFamily::Qwen3.visible_text("<think>still musing"), "");
    }
}
