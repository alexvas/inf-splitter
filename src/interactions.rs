//! Protocol translation for Gemini Interactions API.
//!
//! Uses generated types from interactions_types for type-safe
//! request construction and response parsing.

#![allow(clippy::too_many_arguments)]

use crate::config::RouteTarget;
use crate::interactions_types::{
    Content, CreateModelInteractionParams, GenerationConfig, Interaction, InteractionsInput, Step,
    TextContent,
};

/// Build an interactions request from Anthropic ingress.
///
/// `messages` are the pre-cleaned ingress messages (control messages already stripped).
/// All other parameters are typed scalars extracted at the ingress boundary.
pub fn build_interactions_request_anthropic(
    messages: &[serde_json::Value],
    start_index: usize,
    route: &RouteTarget,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream: bool,
    temperature: Option<f64>,
    ingress_max_tokens: Option<u32>,
    system: Option<String>,
) -> CreateModelInteractionParams {
    let contents: Vec<Content> = messages
        .iter()
        .skip(start_index)
        .filter_map(extract_anthropic_content)
        .collect();

    build_request_body(
        &contents,
        route,
        previous_interaction_id,
        model,
        stream,
        temperature,
        ingress_max_tokens,
        system,
    )
}

/// Build an interactions request from OpenAI ingress.
pub fn build_interactions_request_openai(
    messages: &[serde_json::Value],
    start_index: usize,
    route: &RouteTarget,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream: bool,
    temperature: Option<f64>,
    ingress_max_tokens: Option<u32>,
) -> CreateModelInteractionParams {
    let contents: Vec<Content> = messages
        .iter()
        .skip(start_index)
        .filter_map(extract_openai_content)
        .collect();

    let system = extract_openai_system(messages);

    build_request_body(
        &contents,
        route,
        previous_interaction_id,
        model,
        stream,
        temperature,
        ingress_max_tokens,
        system,
    )
}

fn build_request_body(
    contents: &[Content],
    route: &RouteTarget,
    previous_interaction_id: Option<&str>,
    model: &str,
    stream: bool,
    temperature: Option<f64>,
    ingress_max_tokens: Option<u32>,
    system_instruction: Option<String>,
) -> CreateModelInteractionParams {
    let mut params = CreateModelInteractionParams {
        model: model.to_string(),
        input: InteractionsInput::ContentList(contents.to_vec()),
        stream: Some(stream),
        ..Default::default()
    };

    let max_tokens = route.max_tokens.or(ingress_max_tokens);
    if max_tokens.is_some() || temperature.is_some() {
        params.generation_config = Some(GenerationConfig {
            temperature,
            max_output_tokens: max_tokens.map(|v| v as i64),
            ..Default::default()
        });
    }

    if let Some(sys) = system_instruction {
        params.system_instruction = Some(sys);
    }

    if let Some(prev) = previous_interaction_id {
        params.previous_interaction_id = Some(prev.to_string());
    }

    params
}

/// Build a `CreateModelInteractionParams` for a single chunk in a split-send sequence.
///
/// Chunks are always sent non-streaming. They carry the model name, chunked input,
/// and optionally a system instruction + previous interaction ID for session chaining.
pub fn build_chunk_request(
    model: &str,
    input: Vec<Content>,
    system_instruction: Option<String>,
    previous_interaction_id: Option<String>,
) -> CreateModelInteractionParams {
    CreateModelInteractionParams {
        model: model.to_string(),
        input: InteractionsInput::ContentList(input),
        stream: Some(false),
        system_instruction,
        previous_interaction_id,
        ..Default::default()
    }
}

/// Serialize content array and measure byte size.
pub fn serialized_content_size(contents: &[Content]) -> usize {
    serde_json::to_vec(contents).map(|v| v.len()).unwrap_or(0)
}

/// Split content array into chunks under `limit` bytes. Sequential greedy packing.
pub fn split_content_for_limit(contents: &[Content], limit: usize) -> Vec<Vec<Content>> {
    let mut chunks: Vec<Vec<Content>> = Vec::new();
    let mut current: Vec<Content> = Vec::new();

    for content in contents.iter().cloned() {
        let mut test_chunk = current.clone();
        test_chunk.push(content.clone());
        if serde_json::to_vec(&test_chunk)
            .map(|v| v.len())
            .unwrap_or(0)
            <= limit
            || current.is_empty()
        {
            current.push(content);
        } else {
            chunks.push(std::mem::take(&mut current));
            current.push(content);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Check if any single Content element exceeds the limit (unsplittable).
pub fn single_element_too_large(contents: &[Content], limit: usize) -> bool {
    contents
        .iter()
        .any(|c| serde_json::to_vec(c).map(|v| v.len()).unwrap_or(0) > limit)
}

/// Extract response text from Interaction using generated types.
pub fn extract_interaction_text(interaction: &Interaction) -> String {
    interaction
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::ModelOutputStep(mos) => mos.content.as_ref().map(|content| {
                content
                    .iter()
                    .filter_map(|c| match c {
                        Content::TextContent(tc) => Some(tc.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<&str>>()
                    .join("")
            }),
            _ => None,
        })
        .collect::<Vec<String>>()
        .join("")
}

/// Extract system prompt from an Anthropic ingress body.
pub fn extract_anthropic_system(body: &serde_json::Value) -> Option<String> {
    if let Some(sys) = body.get("system") {
        if let Some(s) = sys.as_str() {
            return Some(s.to_string());
        }
        if let Some(blocks) = sys.as_array() {
            let text: String = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<&str>>()
                .join("\n");
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Get interaction ID from response.
pub fn extract_interaction_id(interaction: &Interaction) -> Option<String> {
    Some(interaction.id.clone())
}

// --- Internal helpers ---

fn extract_anthropic_content(msg: &serde_json::Value) -> Option<Content> {
    let content = msg.get("content")?;
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else if let Some(blocks) = content.as_array() {
        blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<&str>>()
            .join("\n")
    } else {
        return None;
    };

    if text.is_empty() {
        return None;
    }

    Some(Content::TextContent(TextContent {
        text,
        ..Default::default()
    }))
}

fn extract_openai_content(msg: &serde_json::Value) -> Option<Content> {
    let content = msg.get("content")?;
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else if let Some(arr) = content.as_array() {
        arr.iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| part.get("value").and_then(|v| v.as_str()))
            })
            .collect::<Vec<&str>>()
            .join("\n")
    } else {
        return None;
    };

    if text.is_empty() {
        return None;
    }

    Some(Content::TextContent(TextContent {
        text,
        ..Default::default()
    }))
}

fn extract_openai_system(messages: &[serde_json::Value]) -> Option<String> {
    messages
        .first()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .and_then(|m| {
            m.get("content").and_then(|c| {
                if let Some(s) = c.as_str() {
                    return Some(s.to_string());
                }
                if let Some(blocks) = c.as_array() {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
                None
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_route() -> RouteTarget {
        RouteTarget {
            section: "test".into(),
            endpoint_openai: None,
            endpoint_anthropic: None,
            endpoint_interactions: None,
            api_key: None,
            max_tokens: None,
            max_output_tokens: None,
            max_completion_tokens: None,
            model_names: std::collections::HashSet::new(),
            drop_fields: crate::config::DropFields::default(),
            proxy: None,
            proxy_limit: None,
            control_clean_all: None,
            control_extend_lifetime: None,
        }
    }

    fn anthropic_msgs() -> Vec<serde_json::Value> {
        vec![serde_json::json!({"role": "user", "content": "Hello"})]
    }

    fn anthropic_system() -> Option<String> {
        Some("You are helpful.".to_string())
    }

    #[test]
    fn build_anthropic_request_basic() {
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &test_route(),
            None,
            "gemini-3.1-flash-lite",
            false,
            None,
            Some(100),
            anthropic_system(),
        );
        match &req.input {
            InteractionsInput::ContentList(list) => assert_eq!(list.len(), 1),
            _ => panic!("expected ContentList"),
        }
        assert_eq!(req.stream, Some(false));
    }

    #[test]
    fn build_anthropic_with_previous_id() {
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &test_route(),
            Some("prev-123"),
            "gemini-3.1-flash-lite",
            false,
            None,
            Some(100),
            None,
        );
        assert_eq!(req.previous_interaction_id.as_deref(), Some("prev-123"));
    }

    #[test]
    fn build_openai_request_basic() {
        let msgs = vec![serde_json::json!({"role": "user", "content": "Hello"})];
        let req = build_interactions_request_openai(
            &msgs,
            0,
            &test_route(),
            None,
            "gpt-4",
            false,
            None,
            None,
        );
        match &req.input {
            InteractionsInput::ContentList(list) => assert_eq!(list.len(), 1),
            _ => panic!("expected ContentList"),
        }
    }

    #[test]
    fn build_openai_with_system_message() {
        let msgs = vec![
            serde_json::json!({"role": "system", "content": "You are helpful."}),
            serde_json::json!({"role": "user", "content": "Hi"}),
        ];
        let req = build_interactions_request_openai(
            &msgs,
            0,
            &test_route(),
            None,
            "gpt-4",
            false,
            None,
            None,
        );
        assert_eq!(req.system_instruction.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn extract_text_from_interaction() {
        use crate::interactions_types::{Interaction, ModelOutputStep};
        let interaction = Interaction {
            id: "abc".into(),
            status: "completed".into(),
            created: "2026-01-01T00:00:00Z".into(),
            updated: "2026-01-01T00:00:00Z".into(),
            steps: vec![Step::ModelOutputStep(ModelOutputStep {
                content: Some(vec![Content::TextContent(TextContent {
                    text: "Hello!".into(),
                    ..Default::default()
                })]),
                ..Default::default()
            })],
            model: Some("gemini-3.1-flash-lite".into()),
            ..Default::default()
        };
        assert_eq!(extract_interaction_text(&interaction), "Hello!");
    }

    #[test]
    fn split_content_empty() {
        assert!(split_content_for_limit(&[], 100).is_empty());
    }

    #[test]
    fn split_content_single_chunk() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        let chunks = split_content_for_limit(&[c], 1024 * 1024);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn serialized_content_size_positive() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        assert!(serialized_content_size(&[c]) > 0);
    }

    #[test]
    fn extract_openai_system_handles_array_content() {
        let msgs = vec![
            serde_json::json!({"role": "system", "content": [{"type": "text", "text": "You are helpful."}]}),
            serde_json::json!({"role": "user", "content": "Hi"}),
        ];
        let sys = extract_openai_system(&msgs);
        assert_eq!(sys, Some("You are helpful.".to_string()));
    }
}
