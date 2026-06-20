//! Rust types for the Gemini Interactions API.
//!
//! Generated at build time from `schemas/interactions.openapi.json`.
//! The generated code is in `OUT_DIR/interactions_types.rs`.

#![allow(non_camel_case_types)]

// Include the generated types
include!(concat!(env!("OUT_DIR"), "/interactions_types.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_create_model_interaction_params_basic() {
        let json = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "input": "Hello, how are you?"
        });
        let params: CreateModelInteractionParams =
            serde_json::from_value(json).expect("deserialize basic params");
        assert_eq!(params.model, "gemini-3.1-flash-lite");
    }

    #[test]
    fn deserialize_interaction_response() {
        let json = serde_json::json!({
            "id": "abc123",
            "status": "completed",
            "created": "2026-01-01T00:00:00Z",
            "updated": "2026-01-01T00:00:01Z",
            "steps": [],
            "model": "gemini-3.1-flash-lite"
        });
        let interaction: Interaction =
            serde_json::from_value(json).expect("deserialize interaction");
        assert_eq!(interaction.id, "abc123");
        assert_eq!(interaction.status, "completed");
        assert_eq!(interaction.model.as_deref(), Some("gemini-3.1-flash-lite"));
    }

    #[test]
    fn deserialize_generation_config() {
        let json = serde_json::json!({
            "temperature": 0.7,
            "top_p": 0.9,
            "max_output_tokens": 4096
        });
        let config: GenerationConfig =
            serde_json::from_value(json).expect("deserialize generation config");
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.top_p, Some(0.9));
        assert_eq!(config.max_output_tokens, Some(4096));
    }

    #[test]
    fn deserialize_create_model_interaction_params_with_generation_config() {
        let json = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "input": "Hello",
            "generation_config": {
                "temperature": 0.5,
                "max_output_tokens": 2048
            },
            "system_instruction": "You are a helpful assistant.",
            "stream": true
        });
        let params: CreateModelInteractionParams =
            serde_json::from_value(json).expect("deserialize full params");
        assert_eq!(params.model, "gemini-3.1-flash-lite");
        assert_eq!(params.stream, Some(true));
        assert_eq!(
            params.system_instruction.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert!(params.generation_config.is_some());
    }

    #[test]
    fn deserialize_usage() {
        let json = serde_json::json!({
            "total_tokens": 150,
            "total_input_tokens": 50,
            "total_output_tokens": 100
        });
        let usage: Usage = serde_json::from_value(json).expect("deserialize usage");
        assert_eq!(usage.total_tokens, Some(150));
        assert_eq!(usage.total_input_tokens, Some(50));
        assert_eq!(usage.total_output_tokens, Some(100));
    }
}
