//! Protocol translation for Gemini Interactions API.
//!
//! Uses generated types from interactions_types for type-safe
//! request construction and response parsing.

use crate::config::RouteTarget;
use crate::interactions_types::{Content, GenerationConfig, Interaction, Step, TextContent};

/// Build an interactions request from Anthropic ingress body.
pub fn build_interactions_request_anthropic(
    body: &serde_json::Value,
    start_index: usize,
    route: &RouteTarget,
    previous_interaction_id: Option<&str>,
) -> serde_json::Value {
    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    let contents: Vec<Content> = messages
        .iter()
        .skip(start_index)
        .filter_map(extract_anthropic_content)
        .collect();

    build_request_body(&contents, route, body, previous_interaction_id, || {
        extract_anthropic_system(body)
    })
}

/// Build an interactions request from OpenAI ingress body.
pub fn build_interactions_request_openai(
    body: &serde_json::Value,
    start_index: usize,
    route: &RouteTarget,
    previous_interaction_id: Option<&str>,
) -> serde_json::Value {
    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    let contents: Vec<Content> = messages
        .iter()
        .skip(start_index)
        .filter_map(extract_openai_content)
        .collect();

    build_request_body(&contents, route, body, previous_interaction_id, || {
        extract_openai_system(body)
    })
}

fn build_request_body(
    contents: &[Content],
    route: &RouteTarget,
    body: &serde_json::Value,
    previous_interaction_id: Option<&str>,
    system_fn: impl FnOnce() -> Option<String>,
) -> serde_json::Value {
    let mut req = serde_json::json!({
        "input": contents,
        "stream": body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false),
    });

    // Generation config
    let max_tokens = route.max_tokens.or_else(|| {
        body.get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
    });
    if max_tokens.is_some() {
        let gc = GenerationConfig {
            temperature: body.get("temperature").and_then(|v| v.as_f64()),
            top_p: None,
            max_output_tokens: max_tokens.map(|v| v as i64),
            seed: None,
            stop_sequences: None,
            thinking_level: None,
            thinking_summaries: None,
            speech_config: None,
            image_config: None,
            presence_penalty: None,
            frequency_penalty: None,
            tool_choice: None,
        };
        req["generation_config"] = serde_json::to_value(&gc).unwrap_or_default();
    }

    if let Some(sys) = system_fn() {
        req["system_instruction"] = serde_json::json!(sys);
    }

    if let Some(prev) = previous_interaction_id {
        req["previous_interaction_id"] = serde_json::json!(prev);
    }

    req
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
        annotations: None,
        r#type: serde_json::Value::Null,
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
        annotations: None,
        r#type: serde_json::Value::Null,
    }))
}

fn extract_anthropic_system(body: &serde_json::Value) -> Option<String> {
    // Top-level system field (Anthropic format)
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

fn extract_openai_system(body: &serde_json::Value) -> Option<String> {
    body.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.first())
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

    #[test]
    fn build_anthropic_request_basic() {
        let body = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        let req = build_interactions_request_anthropic(&body, 0, &test_route(), None);
        assert_eq!(req["input"].as_array().unwrap().len(), 1);
        assert_eq!(req["stream"], false);
    }

    #[test]
    fn build_anthropic_with_previous_id() {
        let body = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let req = build_interactions_request_anthropic(&body, 0, &test_route(), Some("prev-123"));
        assert_eq!(req["previous_interaction_id"].as_str().unwrap(), "prev-123");
    }

    #[test]
    fn build_openai_request_basic() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let req = build_interactions_request_openai(&body, 0, &test_route(), None);
        assert_eq!(req["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn build_openai_with_system_message() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hi"}
            ]
        });
        let req = build_interactions_request_openai(&body, 0, &test_route(), None);
        assert_eq!(
            req["system_instruction"].as_str().unwrap(),
            "You are helpful."
        );
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
                    annotations: None,
                    r#type: serde_json::Value::Null,
                })]),
                r#type: serde_json::Value::Null,
            })],
            model: Some("gemini-3.1-flash-lite".into()),
            agent: None,
            agent_config: None,
            cached_content: None,
            environment: None,
            environment_id: None,
            generation_config: None,
            input: None,
            previous_interaction_id: None,
            response_format: None,
            response_mime_type: None,
            response_modalities: None,
            role: None,
            safety_settings: None,
            service_tier: None,
            system_instruction: None,
            tools: None,
            usage: None,
            webhook_config: None,
            labels: None,
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
            annotations: None,
            r#type: serde_json::Value::Null,
        });
        let chunks = split_content_for_limit(&[c], 1024 * 1024);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn serialized_content_size_positive() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            annotations: None,
            r#type: serde_json::Value::Null,
        });
        assert!(serialized_content_size(&[c]) > 0);
    }

    #[test]
    fn extract_openai_system_handles_array_content() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": [{"type": "text", "text": "You are helpful."}]},
                {"role": "user", "content": "Hi"}
            ]
        });
        let sys = extract_openai_system(&body);
        assert_eq!(sys, Some("You are helpful.".to_string()));
    }
}
