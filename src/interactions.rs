//! Protocol translation for Gemini Interactions API.
//!
//! Uses generated types from interactions_types for type-safe
//! request construction and response parsing.

#![allow(clippy::too_many_arguments)]

use crate::config::{Protocol, RouteTarget};
use crate::interactions_types::{
    Content, CreateModelInteractionParams, GenerationConfig, Interaction, InteractionsInput, Step,
    TextContent, Tool, ToolChoice,
};
use anyllm_translate::anthropic::{ContentBlock, MessageResponse, Role, StopReason, Usage};
use anyllm_translate::openai::{
    ChatCompletionResponse, ChatContent, ChatMessage, ChatRole, ChatUsage, Choice, FinishReason,
    FunctionCall, ToolCall,
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
    tools: Option<Vec<Tool>>,
    tool_choice: Option<ToolChoice>,
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
        tools,
        tool_choice,
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
    tools: Option<Vec<Tool>>,
    tool_choice: Option<ToolChoice>,
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
        tools,
        tool_choice,
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
    tools: Option<Vec<Tool>>,
    tool_choice: Option<ToolChoice>,
) -> CreateModelInteractionParams {
    let mut params = CreateModelInteractionParams {
        model: model.to_string(),
        input: InteractionsInput::ContentList(contents.to_vec()),
        stream: Some(stream),
        tools: tools.filter(|t| !t.is_empty()),
        ..Default::default()
    };

    let max_tokens = route.max_tokens.or(ingress_max_tokens);
    let has_tool_choice = tool_choice.is_some();
    if max_tokens.is_some() || temperature.is_some() || has_tool_choice {
        let mut gen_config = GenerationConfig {
            temperature,
            max_output_tokens: max_tokens.map(|v| v as i64),
            ..Default::default()
        };
        if has_tool_choice {
            gen_config.tool_choice = tool_choice.and_then(|tc| serde_json::to_value(tc).ok());
        }
        params.generation_config = Some(gen_config);
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

/// Check whether a request that exceeds `proxy_limit` can be split at all.
///
/// Verifies three things:
/// 1. The non-splittable envelope (model, generation_config, tools, etc.,
///    but NOT input or system_instruction) fits within the limit.
/// 2. Every single input Content element, when wrapped in the full envelope,
///    fits within the limit.
/// 3. If system_instruction also needs splitting, no space-delimited word
///    is so large that it cannot fit when wrapped in the full envelope.
pub fn can_split_under_limit(
    params: &CreateModelInteractionParams,
    limit: usize,
) -> Result<(), String> {
    let contents = match &params.input {
        InteractionsInput::ContentList(list) => list.clone(),
        _ => vec![],
    };

    let tool_info = || {
        params
            .tools
            .as_deref()
            .map(|tools| tool_size_breakdown(tools))
            .unwrap_or_default()
    };

    // 1. Minimal envelope (no input, no system_instruction)
    let envelope = CreateModelInteractionParams {
        model: params.model.clone(),
        input: InteractionsInput::ContentList(vec![]),
        system_instruction: None,
        stream: params.stream,
        generation_config: params.generation_config.clone(),
        tools: params.tools.clone(),
        previous_interaction_id: params.previous_interaction_id.clone(),
        ..Default::default()
    };
    let envelope_size = serde_json::to_vec(&envelope).map(|v| v.len()).unwrap_or(0);
    if envelope_size >= limit {
        let mut parts: Vec<String> = Vec::new();

        let mut measure = |label: &str, val: &serde_json::Value| {
            let size = serde_json::to_vec(val).map(|v| v.len()).unwrap_or(0);
            parts.push(format!("  {label}: {} ({})", size, format_bytes(size)));
        };

        measure("model", &serde_json::json!(params.model));
        measure("stream", &serde_json::json!(params.stream));
        if let Some(ref gc) = params.generation_config {
            measure("generation_config", &serde_json::json!(gc));
        }
        if let Some(ref tools) = params.tools {
            measure("tools", &serde_json::json!(tools));
        }
        if let Some(ref prev_id) = params.previous_interaction_id {
            measure("previous_interaction_id", &serde_json::json!(prev_id));
        }

        let ti = tool_info();
        return Err(format!(
            "Non-splittable request fields ({envelope_size} bytes / {}) exceed proxy limit ({}):\n{}\n{ti}",
            format_bytes(envelope_size),
            format_bytes(limit),
            parts.join("\n"),
        ));
    }

    // 2. Each single content element, wrapped in the full envelope, must fit
    for c in &contents {
        let single = CreateModelInteractionParams {
            input: InteractionsInput::ContentList(vec![c.clone()]),
            ..envelope.clone()
        };
        if serde_json::to_vec(&single).map(|v| v.len()).unwrap_or(0) > limit {
            let ti = tool_info();
            return Err(format!(
                "Single content element too large for proxy limit ({}):\n{ti}",
                format_bytes(limit),
            ));
        }
    }

    // 3. System instruction: if it needs splitting, check worst-case splittability
    if let Some(ref sys) = params.system_instruction {
        let sys_body = CreateModelInteractionParams {
            system_instruction: Some(sys.clone()),
            ..envelope.clone()
        };
        if serde_json::to_vec(&sys_body).map(|v| v.len()).unwrap_or(0) > limit {
            // split_text_for_limit splits on hierarchical delimiters, final
            // fallback is space. If any space-delimited word + envelope > limit,
            // splitting will fail.
            for word in sys.split_whitespace() {
                let word_body = CreateModelInteractionParams {
                    system_instruction: Some(word.to_string()),
                    ..envelope.clone()
                };
                if serde_json::to_vec(&word_body).map(|v| v.len()).unwrap_or(0) > limit {
                    let ti = tool_info();
                    return Err(format!(
                        "System instruction contains unsplittable word exceeding proxy limit ({}):\n{ti}",
                        format_bytes(limit),
                    ));
                }
            }
        }
    }

    Ok(())
}

fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn tool_size_breakdown(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec!["Per-tool size breakdown:".to_string()];
    for tool in tools {
        match tool {
            Tool::Function(f) => {
                let name = f.name.as_deref().unwrap_or("(unnamed)");
                let total = serde_json::to_vec(tool).map(|v| v.len()).unwrap_or(0);
                let desc_bytes = f.description.as_ref().map(|d| d.len()).unwrap_or(0);
                let params_bytes = f
                    .parameters
                    .as_ref()
                    .and_then(|p| serde_json::to_vec(p).ok())
                    .map(|v| v.len())
                    .unwrap_or(0);
                lines.push(format!(
                    "  {name}: {} (description: {}, parameters: {})",
                    format_bytes(total),
                    format_bytes(desc_bytes),
                    format_bytes(params_bytes),
                ));
            }
            other => {
                let type_name = match other {
                    Tool::CodeExecution(_) => "code_execution",
                    Tool::UrlContext(_) => "url_context",
                    Tool::ComputerUse(_) => "computer_use",
                    Tool::McpServer(_) => "mcp_server",
                    Tool::GoogleSearch(_) => "google_search",
                    Tool::FileSearch(_) => "file_search",
                    Tool::GoogleMaps(_) => "google_maps",
                    Tool::Retrieval(_) => "retrieval",
                    Tool::Function(_) => unreachable!(),
                };
                let total = serde_json::to_vec(other).map(|v| v.len()).unwrap_or(0);
                lines.push(format!("  ({type_name}): {}", format_bytes(total)));
            }
        }
    }
    lines.join("\n")
}

/// Extract function tool calls from an Interaction response.
///
/// Returns `None` when the interaction has no function_call steps (i.e., the
/// model did not request any tool invocation) or the status is not
/// `"requires_action"`. Returns `Some(Vec<(id, name, arguments)>)` for each
/// `FunctionCallStep` found.
pub fn extract_interaction_tool_calls(
    interaction: &Interaction,
) -> Option<Vec<(String, String, serde_json::Value)>> {
    if interaction.status != "requires_action" {
        return None;
    }
    let calls: Vec<_> = interaction
        .steps
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|step| match step {
            Step::FunctionCallStep(fcs) => Some((
                fcs.id.clone(),
                fcs.name.clone(),
                fcs.arguments.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

/// Build a typed protocol response from an Interaction.
///
/// Extracts text and tool calls from the interaction and constructs the
/// appropriate response type (Anthropic `MessageResponse` or OpenAI
/// `ChatCompletionResponse`).  Uses `stop_reason: "tool_use"` /
/// `finish_reason: "tool_calls"` when the status is `"requires_action"`,
/// `"end_turn"` / `"stop"` otherwise.
pub fn build_response_from_interaction(
    interaction: &Interaction,
    model: &str,
    ingress: Protocol,
) -> Result<serde_json::Value, String> {
    let text = extract_interaction_text(interaction);
    let tool_calls = extract_interaction_tool_calls(interaction);
    let input_tokens = interaction
        .usage
        .as_ref()
        .and_then(|u| u.total_input_tokens)
        .unwrap_or(0);
    let output_tokens = interaction
        .usage
        .as_ref()
        .and_then(|u| u.total_output_tokens)
        .unwrap_or(0);

    match ingress {
        Protocol::OpenAi => {
            let (content, tool_calls_field, finish_reason) = if let Some(ref calls) = tool_calls {
                let tc: Vec<ToolCall> = calls
                    .iter()
                    .map(|(id, name, args)| ToolCall {
                        id: id.clone(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: serde_json::to_string(args).unwrap_or_default(),
                        },
                    })
                    .collect();
                (None, Some(tc), FinishReason::ToolCalls)
            } else {
                (Some(ChatContent::Text(text)), None, FinishReason::Stop)
            };
            let typed = ChatCompletionResponse {
                id: interaction.id.clone(),
                object: "chat.completion".to_string(),
                model: model.to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatMessage {
                        role: ChatRole::Assistant,
                        content,
                        name: None,
                        tool_calls: tool_calls_field,
                        tool_call_id: None,
                        refusal: None,
                        reasoning_content: None,
                    },
                    finish_reason: Some(finish_reason),
                    logprobs: None,
                }],
                usage: Some(ChatUsage {
                    prompt_tokens: input_tokens as u32,
                    completion_tokens: output_tokens as u32,
                    total_tokens: (input_tokens + output_tokens) as u32,
                    completion_tokens_details: None,
                    prompt_tokens_details: None,
                }),
                created: None,
                system_fingerprint: None,
                service_tier: None,
            };
            serde_json::to_value(typed).map_err(|e| e.to_string())
        }
        Protocol::Anthropic => {
            let content: Vec<ContentBlock> = if let Some(ref calls) = tool_calls {
                calls
                    .iter()
                    .map(|(id, name, args)| ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: args.clone(),
                    })
                    .collect()
            } else {
                vec![ContentBlock::Text { text: text.clone() }]
            };
            let stop_reason = if tool_calls.is_some() {
                Some(StopReason::ToolUse)
            } else {
                Some(StopReason::EndTurn)
            };
            let typed = MessageResponse {
                id: interaction.id.clone(),
                response_type: "message".to_string(),
                role: Role::Assistant,
                model: model.to_string(),
                content,
                stop_reason,
                stop_sequence: None,
                usage: Usage {
                    input_tokens: input_tokens as u32,
                    output_tokens: output_tokens as u32,
                    ..Default::default()
                },
                created: None,
            };
            serde_json::to_value(typed).map_err(|e| e.to_string())
        }
    }
}

/// Extract response text from Interaction using generated types.
pub fn extract_interaction_text(interaction: &Interaction) -> String {
    interaction
        .steps
        .as_deref()
        .unwrap_or_default()
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

/// Extract tools and tool_choice from an Anthropic ingress body, converting to
/// Interactions API Tool format.
///
/// Anthropic tool: `{"name": "...", "description": "...", "input_schema": {...}}`
/// → `Tool::Function(Function { name, description, parameters: input_schema, .. })`
///
/// Returns `(tools, tool_choice)`.  `tools` is `None` when the ingress has no tools array.
pub fn extract_anthropic_tools(
    body: &serde_json::Value,
) -> (Option<Vec<Tool>>, Option<ToolChoice>) {
    let tools = body.get("tools").and_then(|arr| arr.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|t| {
                let mut tool = t.clone();
                let obj = tool.as_object_mut()?;
                // Inject type tag if missing
                if !obj.contains_key("type") {
                    obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("function".to_string()),
                    );
                }
                // Rename input_schema → parameters
                if let Some(schema) = obj.remove("input_schema") {
                    obj.insert("parameters".to_string(), schema);
                }
                serde_json::from_value::<Tool>(tool).ok()
            })
            .collect()
    });

    let tool_choice = body.get("tool_choice").and_then(|v| {
        // Anthropic uses {"type": "auto"} — extract type as a simple string
        if v.is_object() {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| ToolChoice::Simple(s.to_string()))
        } else {
            serde_json::from_value::<ToolChoice>(v.clone()).ok()
        }
    });

    (tools, tool_choice)
}

/// Extract tools and tool_choice from an OpenAI ingress body, converting to
/// Interactions API Tool format.
///
/// OpenAI tool: `{"type": "function", "function": {"name": "...", ...}}`
/// → `Tool::Function(Function { name, description, parameters, .. })`
///
/// Returns `(tools, tool_choice)`.  `tools` is `None` when the ingress has no tools array.
pub fn extract_openai_tools(body: &serde_json::Value) -> (Option<Vec<Tool>>, Option<ToolChoice>) {
    let tools = body.get("tools").and_then(|arr| arr.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|t| {
                let func = t.get("function")?;
                let mut tool = func.clone();
                let obj = tool.as_object_mut()?;
                if !obj.contains_key("type") {
                    obj.insert(
                        "type".to_string(),
                        serde_json::Value::String("function".to_string()),
                    );
                }
                serde_json::from_value::<Tool>(tool).ok()
            })
            .collect()
    });

    let tool_choice = body
        .get("tool_choice")
        .and_then(|v| serde_json::from_value::<ToolChoice>(v.clone()).ok());

    (tools, tool_choice)
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
    use crate::interactions_types::FunctionCallStep;

    fn test_route() -> RouteTarget {
        RouteTarget {
            section: "test".into(),
            ..Default::default()
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
            None,
            None,
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
            None,
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
            None,
            None,
        );
        assert_eq!(req.system_instruction.as_deref(), Some("You are helpful."));
    }

    #[test]
    fn extract_text_from_interaction() {
        use crate::interactions_types::{Interaction, ModelOutputStep};
        let step = ModelOutputStep {
            content: Some(vec![Content::TextContent(TextContent {
                text: "Hello!".into(),
                ..Default::default()
            })]),
            ..Default::default()
        };
        let interaction = Interaction {
            id: "abc".into(),
            status: "completed".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![Step::ModelOutputStep(step)]),
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

    // --- Tool extraction tests ---

    #[test]
    fn extract_anthropic_tools_basic() {
        let body = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "messages": [{"role": "user", "content": "Weather?"}],
            "tools": [{"name": "get_weather", "description": "Get weather", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"}
        });
        let (tools, tool_choice) = extract_anthropic_tools(&body);
        let tools = tools.expect("tools should be extracted");
        assert_eq!(tools.len(), 1);
        match &tools[0] {
            Tool::Function(f) => {
                assert_eq!(f.name.as_deref(), Some("get_weather"));
                assert_eq!(f.description.as_deref(), Some("Get weather"));
                assert!(f.parameters.is_some());
            }
            _ => panic!("expected Function tool"),
        }
        assert!(tool_choice.is_some());
        match tool_choice.unwrap() {
            ToolChoice::Simple(s) => assert_eq!(s, "auto"),
            other => panic!("expected Simple tool_choice, got {:?}", other),
        }
    }

    #[test]
    fn extract_anthropic_tools_none_when_no_tools() {
        let body = serde_json::json!({
            "model": "gemini-3.1-flash-lite",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (tools, tool_choice) = extract_anthropic_tools(&body);
        assert!(tools.is_none());
        assert!(tool_choice.is_none());
    }

    #[test]
    fn extract_openai_tools_basic() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Weather?"}],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get weather", "parameters": {"type": "object"}}}],
            "tool_choice": "auto"
        });
        let (tools, tool_choice) = extract_openai_tools(&body);
        let tools = tools.expect("tools should be extracted");
        assert_eq!(tools.len(), 1);
        match &tools[0] {
            Tool::Function(f) => {
                assert_eq!(f.name.as_deref(), Some("get_weather"));
                assert_eq!(f.description.as_deref(), Some("Get weather"));
                assert!(f.parameters.is_some());
            }
            _ => panic!("expected Function tool"),
        }
        assert!(tool_choice.is_some());
        match tool_choice.unwrap() {
            ToolChoice::Simple(s) => assert_eq!(s, "auto"),
            other => panic!("expected Simple tool_choice, got {:?}", other),
        }
    }

    #[test]
    fn extract_openai_tools_none_when_no_tools() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (tools, tool_choice) = extract_openai_tools(&body);
        assert!(tools.is_none());
        assert!(tool_choice.is_none());
    }

    #[test]
    fn build_anthropic_request_with_tools() {
        let tools = vec![Tool::Function(crate::interactions_types::Function {
            name: Some("get_weather".into()),
            description: Some("Get weather".into()),
            parameters: Some(serde_json::json!({"type": "object"})),
            ..Default::default()
        })];
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &test_route(),
            None,
            "gemini-3.1-flash-lite",
            false,
            None,
            Some(100),
            None,
            Some(tools),
            Some(ToolChoice::Simple("auto".into())),
        );
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
        assert!(req.generation_config.is_some());
        assert!(req.generation_config.unwrap().tool_choice.is_some());
    }

    #[test]
    fn build_openai_request_with_tools() {
        let tools = vec![Tool::Function(crate::interactions_types::Function {
            name: Some("get_weather".into()),
            description: Some("Get weather".into()),
            parameters: Some(serde_json::json!({"type": "object"})),
            ..Default::default()
        })];
        let req = build_interactions_request_openai(
            &[serde_json::json!({"role": "user", "content": "Weather?"})],
            0,
            &test_route(),
            None,
            "gemini-3.1-flash-lite",
            false,
            None,
            None,
            Some(tools),
            Some(ToolChoice::Simple("auto".into())),
        );
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
        assert!(req.generation_config.is_some());
        assert!(req.generation_config.unwrap().tool_choice.is_some());
    }

    #[test]
    fn build_request_no_tools_field_when_none() {
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &test_route(),
            None,
            "gemini-3.1-flash-lite",
            false,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(
            req.tools.is_none(),
            "tools should be absent when not provided"
        );
    }

    // --- Tool call extraction tests (RED — not yet implemented) ---

    #[test]
    fn extract_tool_calls_from_interaction() {
        let interaction = Interaction {
            id: "abc".into(),
            status: "requires_action".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![Step::FunctionCallStep(FunctionCallStep {
                id: "call-1".into(),
                name: "get_weather".into(),
                arguments: Some(serde_json::json!({"location": "Boston"})),
                ..Default::default()
            })]),
            ..Default::default()
        };
        let result = extract_interaction_tool_calls(&interaction);
        assert!(
            result.is_some(),
            "should extract tool calls from requires_action interaction"
        );
        let tool_calls = result.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].0, "call-1"); // (id, name, arguments)
        assert_eq!(tool_calls[0].1, "get_weather");
        assert_eq!(tool_calls[0].2, serde_json::json!({"location": "Boston"}));
    }

    #[test]
    fn extract_tool_calls_empty_for_completed() {
        let interaction = Interaction {
            id: "abc".into(),
            status: "completed".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![]),
            ..Default::default()
        };
        let result = extract_interaction_tool_calls(&interaction);
        assert!(
            result.is_none(),
            "should return None for completed interaction"
        );
    }

    #[test]
    fn build_response_with_function_call_anthropic() {
        let interaction = Interaction {
            id: "abc".into(),
            status: "requires_action".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![Step::FunctionCallStep(FunctionCallStep {
                id: "call-1".into(),
                name: "get_weather".into(),
                arguments: Some(serde_json::json!({"location": "Boston"})),
                ..Default::default()
            })]),
            model: Some("gemini".into()),
            ..Default::default()
        };
        let resp = build_response_from_interaction(&interaction, "gemini", Protocol::Anthropic)
            .expect("should build response");
        let msg: MessageResponse =
            serde_json::from_value(resp).expect("should deserialize as MessageResponse");
        assert_eq!(msg.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "get_weather");
                assert_eq!(input, &serde_json::json!({"location": "Boston"}));
            }
            other => panic!("expected ToolUse, got: {other:?}"),
        }
    }

    #[test]
    fn build_response_without_tool_calls_is_end_turn() {
        let interaction = Interaction {
            id: "abc".into(),
            status: "completed".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![]),
            ..Default::default()
        };
        let resp =
            build_response_from_interaction(&interaction, "gemini", Protocol::Anthropic).unwrap();
        let msg: MessageResponse =
            serde_json::from_value(resp).expect("should deserialize as MessageResponse");
        assert_eq!(msg.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn build_response_function_call_openai() {
        let interaction = Interaction {
            id: "abc".into(),
            status: "requires_action".into(),
            created: Some("2026-01-01T00:00:00Z".into()),
            updated: Some("2026-01-01T00:00:00Z".into()),
            steps: Some(vec![Step::FunctionCallStep(FunctionCallStep {
                id: "call-1".into(),
                name: "get_weather".into(),
                arguments: Some(serde_json::json!({"location": "Boston"})),
                ..Default::default()
            })]),
            model: Some("gemini".into()),
            ..Default::default()
        };
        let resp = build_response_from_interaction(&interaction, "gemini", Protocol::OpenAi)
            .expect("should build response");
        let msg: ChatCompletionResponse =
            serde_json::from_value(resp).expect("should deserialize as ChatCompletionResponse");
        assert_eq!(msg.choices[0].finish_reason, Some(FinishReason::ToolCalls));
        let tool_calls = msg.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("should have tool_calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments, r#"{"location":"Boston"}"#);
    }

    /// Compile-time check: verify all required tool-use types exist.
    #[test]
    fn check_tool_variants_exist() {
        // Anthropic
        let _ = anyllm_translate::anthropic::StopReason::ToolUse;
        let _ = anyllm_translate::anthropic::ContentBlock::ToolUse {
            id: String::new(),
            name: String::new(),
            input: serde_json::Value::Null,
        };
        // OpenAI
        let _ = anyllm_translate::openai::FinishReason::ToolCalls;
        let tc = anyllm_translate::openai::ToolCall {
            call_type: "function".into(),
            id: String::new(),
            function: anyllm_translate::openai::FunctionCall {
                name: String::new(),
                arguments: String::new(),
            },
        };
        let _ = tc;
    }
}
