//! Rust types for the Gemini Interactions API.
//!
//! Generated at build time from `schemas/interactions.openapi.json`.
//! The generated code is in `OUT_DIR/interactions_types.rs`.

#![allow(non_camel_case_types)]

// Include the generated types
include!(concat!(env!("OUT_DIR"), "/interactions_types.rs"));

// Manual Default impl for InteractionsInput — cannot derive because
// it's a data-carrying untagged enum (String, StepList, ContentList, Content).
impl Default for InteractionsInput {
    fn default() -> Self {
        InteractionsInput::String(String::new())
    }
}

/// Typed tool_choice covering the oneOf: simple string or full config.
/// The generated `GenerationConfig.tool_choice` is `Option<serde_json::Value>`
/// because the build.rs codegen cannot handle inline oneOf schemas.
/// This enum bridges that gap with a serde-untagged representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Simple(String),
    Config(ToolChoiceConfig),
}

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

    #[test]
    fn content_serialization_has_correct_type_field() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        let v = serde_json::to_value(&c).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("type").and_then(|v| v.as_str()),
            Some("text"),
            "Content serialization must produce \"type\": \"text\", not null"
        );
    }

    #[test]
    fn deserialize_interaction_created_incomplete() {
        // Gemini API sends interaction.created SSE events where the initial
        // interaction object is incomplete: only id, status, object, model —
        // no created/updated/steps. These must deserialize successfully.
        let json = serde_json::json!({
            "event_type": "interaction.created",
            "interaction": {
                "id": "v1_ChdUdFUzYXQzNktPS2xtdGtQdE92eGtBcxIXVHRVM2F0MzZLT0tsbXRrUHRPdnhrQXM",
                "status": "in_progress",
                "object": "interaction",
                "model": "gemini-3.1-flash-lite"
            }
        });
        let event: InteractionSseEvent = serde_json::from_value(json)
            .expect("deserialize interaction.created with incomplete interaction");
        match event {
            InteractionSseEvent::InteractionCreatedEvent(ev) => {
                assert_eq!(
                    ev.interaction.id,
                    "v1_ChdUdFUzYXQzNktPS2xtdGtQdE92eGtBcxIXVHRVM2F0MzZLT0tsbXRrUHRPdnhrQXM"
                );
                assert_eq!(ev.interaction.status, "in_progress");
                assert_eq!(
                    ev.interaction.model.as_deref(),
                    Some("gemini-3.1-flash-lite")
                );
            }
            other => panic!(
                "expected InteractionCreatedEvent, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn sse_event_serialization_has_correct_event_type_field() {
        let ev = InteractionSseEvent::InteractionStatusUpdate(InteractionStatusUpdate {
            interaction_id: "int1".into(),
            status: "running".into(),
            ..Default::default()
        });
        let v = serde_json::to_value(&ev).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("event_type").and_then(|v| v.as_str()),
            Some("interaction.status_update"),
            "InteractionSseEvent serialization must preserve event_type tag, not null"
        );
    }
}
