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
    let is_first = previous_interaction_id.is_none();

    // tools and system_instruction are set only on the first interaction.
    // Follow-ups reuse the interaction's existing configuration.
    let mut params = CreateModelInteractionParams {
        model: model.to_string(),
        input: InteractionsInput::ContentList(contents.to_vec()),
        stream: Some(stream),
        tools: if is_first {
            tools.filter(|t| !t.is_empty())
        } else {
            None
        },
        ..Default::default()
    };

    // generation_config is set only on the first interaction.
    // Follow-ups reuse the interaction's existing configuration.
    if is_first {
        // Route max_tokens is a cap, not an override: min(client, route) wins.
        let max_tokens = match (route.max_tokens, ingress_max_tokens) {
            (Some(route_cap), Some(client_val)) => Some(route_cap.min(client_val)),
            (Some(route_cap), None) => Some(route_cap),
            (None, client_val) => client_val,
        };
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
    }

    if is_first {
        if let Some(sys) = system_instruction {
            params.system_instruction = Some(sys);
        }
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

/// Pack content into chunks greedily, measuring full serialized chunk body size.
///
/// `first_envelope` is the template for the first chunk (with tools, generation_config,
/// system_instruction). `subsequent_envelope` is the template for all following chunks
/// (with `previous_interaction_id`, without first-only fields).
///
/// Each content item is added only if `serialize(envelope + current_items + item) ≤ limit`.
/// Returns `Err` if any single item exceeds the limit in an otherwise-empty chunk.
pub fn pack_content_into_chunks(
    first_envelope: &CreateModelInteractionParams,
    subsequent_envelope: &CreateModelInteractionParams,
    contents: &[Content],
    limit: usize,
) -> Result<Vec<Vec<Content>>, String> {
    let mut chunks: Vec<Vec<Content>> = Vec::new();
    let mut current: Vec<Content> = Vec::new();

    for content in contents.iter().cloned() {
        let is_first_chunk = chunks.is_empty();
        let envelope = if is_first_chunk {
            first_envelope
        } else {
            subsequent_envelope
        };

        let test_input = {
            let mut test = current.clone();
            test.push(content.clone());
            test
        };
        let test_body = build_pack_body(envelope, &test_input);
        let test_size = serde_json::to_vec(&test_body).map(|v| v.len()).unwrap_or(0);

        if current.is_empty() && test_size > limit {
            return Err(format!(
                "content item too large for proxy_limit: {test_size} > {limit}"
            ));
        }

        if test_size <= limit {
            current.push(content);
        } else {
            chunks.push(std::mem::take(&mut current));
            // Re-test with appropriate envelope for the new chunk
            let new_envelope = if chunks.is_empty() {
                first_envelope
            } else {
                subsequent_envelope
            };
            let single_body = build_pack_body(new_envelope, std::slice::from_ref(&content));
            let single_size = serde_json::to_vec(&single_body)
                .map(|v| v.len())
                .unwrap_or(0);
            if single_size > limit {
                return Err(format!(
                    "content item too large for proxy_limit: {single_size} > {limit}"
                ));
            }
            current.push(content);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    Ok(chunks)
}

/// Build a `CreateModelInteractionParams` body for size measurement during packing.
pub(crate) fn build_pack_body(
    envelope: &CreateModelInteractionParams,
    input: &[Content],
) -> CreateModelInteractionParams {
    CreateModelInteractionParams {
        input: InteractionsInput::ContentList(input.to_vec()),
        ..envelope.clone()
    }
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
            .map(tool_size_breakdown)
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
    let mut entries: Vec<(usize, String)> = Vec::with_capacity(tools.len());
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
                entries.push((
                    total,
                    format!(
                        "  {name}: {} (description: {}, parameters: {})",
                        format_bytes(total),
                        format_bytes(desc_bytes),
                        format_bytes(params_bytes),
                    ),
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
                entries.push((total, format!("  ({type_name}): {}", format_bytes(total))));
            }
        }
    }
    entries.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut lines: Vec<String> = vec!["Per-tool size breakdown (sorted by size):".to_string()];
    for (_size, line) in entries {
        lines.push(line);
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

/// Clamp i64 token count to u32 range, logging a warning on overflow.
pub fn clamp_i64_to_u32(n: i64, field: &str) -> u32 {
    if n < 0 {
        tracing::warn!(value = n, field, "negative token count, clamping to 0");
        0
    } else if n > u32::MAX as i64 {
        tracing::warn!(value = n, field, "token count exceeds u32::MAX, clamping");
        u32::MAX
    } else {
        n as u32
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

    let input_u32 = clamp_i64_to_u32(input_tokens, "total_input_tokens");
    let output_u32 = clamp_i64_to_u32(output_tokens, "total_output_tokens");

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
                    prompt_tokens: input_u32,
                    completion_tokens: output_u32,
                    total_tokens: input_u32.saturating_add(output_u32),
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
                    input_tokens: input_u32,
                    output_tokens: output_u32,
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
            .filter_map(|b| {
                let block_type = b.get("type").and_then(|t| t.as_str());
                match block_type {
                    Some("text") => b.get("text").and_then(|t| t.as_str()).map(String::from),
                    Some("tool_result") => {
                        let c = b.get("content")?;
                        if let Some(s) = c.as_str() {
                            Some(s.to_string())
                        } else if let Some(arr) = c.as_array() {
                            let joined: String = arr
                                .iter()
                                .filter_map(|tb| tb.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<&str>>()
                                .join("\n");
                            if joined.is_empty() {
                                None
                            } else {
                                Some(joined)
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            })
            .collect::<Vec<String>>()
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
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
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

/// Filter harness-originated messages from the full message list.
///
/// Stateless protocols (Anthropic Messages, OpenAI Chat Completions) resend
/// both harness-originated and LLM-originated history. Only harness-originated
/// messages drive upstream deltas.
///
/// | Protocol | Kept | Discarded |
/// |----------|------|-----------|
/// | Anthropic Messages | `user` role, including `tool_result` blocks | `assistant` |
/// | OpenAI Chat Completions | `system`, `developer`, `user`, `tool` | `assistant` |
pub fn filter_harness_messages(
    messages: &[serde_json::Value],
    protocol: Protocol,
) -> Vec<serde_json::Value> {
    match protocol {
        Protocol::Anthropic => messages
            .iter()
            .filter(|m| {
                m.get("role")
                    .and_then(|r| r.as_str())
                    .map(|r| r == "user")
                    .unwrap_or(false)
            })
            .cloned()
            .collect(),
        Protocol::OpenAi => messages
            .iter()
            .filter(|m| {
                m.get("role")
                    .and_then(|r| r.as_str())
                    .map(|r| matches!(r, "system" | "developer" | "user" | "tool"))
                    .unwrap_or(false)
            })
            .cloned()
            .collect(),
    }
}

/// Compute the xxh3-64 hash of a harness message.
///
/// Hash input is `serde_json::to_vec(message)` from the parsed
/// `serde_json::Value` — the **full** message `Value` (all fields
/// including `role`, `content`, nested `tool_result` blocks) is
/// serialized and hashed, not extracted text.
pub fn hash_harness_message(message: &serde_json::Value) -> u64 {
    let bytes = serde_json::to_vec(message).unwrap_or_default();
    xxhash_rust::xxh3::xxh3_64(&bytes)
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

    // --- pack_content_into_chunks tests (RED) ---

    fn pack_envelope(model: &str) -> CreateModelInteractionParams {
        CreateModelInteractionParams {
            model: model.to_string(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            ..Default::default()
        }
    }

    fn text_content(text: &str) -> Content {
        Content::TextContent(TextContent {
            text: text.to_string(),
            ..Default::default()
        })
    }

    fn content_of_approx_size(target_bytes: usize) -> Content {
        let payload_len = target_bytes.saturating_sub(50);
        let text = "x".repeat(payload_len.max(1));
        Content::TextContent(TextContent {
            text,
            ..Default::default()
        })
    }

    #[test]
    fn pack_greedy_splits_at_limit() {
        let envelope = pack_envelope("test-model");
        let envelope_size = serde_json::to_vec(&envelope).unwrap().len();
        let limit = envelope_size + 200;

        let item1 = content_of_approx_size(50);
        let item2 = content_of_approx_size(50);
        let item3 = content_of_approx_size(50);

        let items = vec![item1.clone(), item2.clone(), item3.clone()];
        let result = pack_content_into_chunks(&envelope, &envelope, &items, limit).unwrap();

        for chunk in &result {
            let body = build_pack_body(&envelope, chunk);
            let size = serde_json::to_vec(&body).unwrap().len();
            assert!(size <= limit, "chunk size {size} exceeds limit {limit}");
        }

        let total: usize = result.iter().map(|c| c.len()).sum();
        assert_eq!(total, 3, "all items must be packed");
    }

    #[test]
    fn pack_single_item_too_large_rejected() {
        let envelope = pack_envelope("test-model");
        let envelope_size = serde_json::to_vec(&envelope).unwrap().len();
        let limit = envelope_size + 5; // tiny limit — only ~5 bytes for content

        // Create an item that's definitely > 5 bytes when serialized
        let item = content_of_approx_size(100);
        let result = pack_content_into_chunks(&envelope, &envelope, &[item], limit);
        assert!(result.is_err(), "single item > limit must error");
    }

    #[test]
    fn pack_all_items_fit_in_one_chunk() {
        let envelope = pack_envelope("test-model");
        let limit = 10 * 1024 * 1024;

        let items = vec![
            text_content("one"),
            text_content("two"),
            text_content("three"),
        ];
        let result = pack_content_into_chunks(&envelope, &envelope, &items, limit).unwrap();
        assert_eq!(result.len(), 1, "all items should fit in one chunk");
        assert_eq!(result[0].len(), 3);
    }

    #[test]
    fn pack_greedy_each_chunk_maximally_full() {
        let envelope = pack_envelope("test-model");
        let envelope_size = serde_json::to_vec(&envelope).unwrap().len();
        let limit = envelope_size + 300;

        let items = vec![
            content_of_approx_size(100),
            content_of_approx_size(100),
            content_of_approx_size(100),
            content_of_approx_size(150),
            content_of_approx_size(100),
        ];
        let result = pack_content_into_chunks(&envelope, &envelope, &items, limit).unwrap();

        for chunk in &result {
            let body = build_pack_body(&envelope, chunk);
            let size = serde_json::to_vec(&body).unwrap().len();
            assert!(size <= limit, "chunk size {size} exceeds limit {limit}");
        }

        // Greedy: adding first item of chunk 1 to chunk 0 would exceed limit
        if result.len() > 1 {
            let mut test_chunk1 = result[0].clone();
            test_chunk1.push(result[1][0].clone());
            let body = build_pack_body(&envelope, &test_chunk1);
            let size = serde_json::to_vec(&body).unwrap().len();
            assert!(
                size > limit,
                "greedy invariant broken: could have added more to chunk 0"
            );
        }

        let total: usize = result.iter().map(|c| c.len()).sum();
        assert_eq!(total, 5, "all items must be packed");
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

    #[test]
    fn extract_openai_system_in_non_first_position() {
        // System message can appear at any position in OpenAI message arrays.
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "system", "content": "Be concise"}),
            serde_json::json!({"role": "assistant", "content": "OK"}),
        ];
        let sys = extract_openai_system(&msgs);
        assert_eq!(sys, Some("Be concise".to_string()));
    }

    #[test]
    fn extract_openai_system_no_system_message_returns_none() {
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "OK"}),
        ];
        let sys = extract_openai_system(&msgs);
        assert_eq!(sys, None);
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

    #[test]
    fn build_anthropic_followup_omits_first_only_fields() {
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
            Some("prev-123"),
            "gemini-3.1-flash-lite",
            false,
            Some(0.7),
            Some(200),
            anthropic_system(),
            Some(tools),
            Some(ToolChoice::Simple("auto".into())),
        );
        // On follow-up: no tools, no system_instruction, no generation_config
        assert!(req.tools.is_none(), "tools must be absent on follow-up");
        assert!(
            req.system_instruction.is_none(),
            "system_instruction must be absent on follow-up"
        );
        assert!(
            req.generation_config.is_none(),
            "generation_config must be absent on follow-up"
        );
        assert_eq!(
            req.previous_interaction_id.as_deref(),
            Some("prev-123"),
            "previous_interaction_id must be set"
        );
    }

    #[test]
    fn build_anthropic_first_includes_all_fields() {
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
            Some(0.7),
            Some(200),
            anthropic_system(),
            Some(tools),
            Some(ToolChoice::Simple("auto".into())),
        );
        // First interaction: everything must be present
        assert!(
            req.tools.is_some(),
            "tools must be present on first interaction"
        );
        assert!(
            req.system_instruction.is_some(),
            "system_instruction must be present on first interaction"
        );
        assert!(
            req.generation_config.is_some(),
            "generation_config must be present on first interaction"
        );
        let gc = req.generation_config.unwrap();
        assert!(gc.temperature.is_some());
        assert!(gc.max_output_tokens.is_some());
        assert!(gc.tool_choice.is_some());
        assert!(req.previous_interaction_id.is_none());
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
    fn max_tokens_client_lower_than_route_client_wins() {
        // Client sends max_tokens=100, route has max_tokens=1000.
        // Lower (more restrictive) client value must win.
        let mut route = test_route();
        route.max_tokens = Some(1000);
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &route,
            None,
            "test-model",
            false,
            None,
            Some(100),
            None,
            None,
            None,
        );
        let gc = req.generation_config.unwrap();
        assert_eq!(gc.max_output_tokens, Some(100));
    }

    #[test]
    fn max_tokens_route_lower_than_client_route_caps() {
        // Client sends max_tokens=1000, route has max_tokens=100.
        // Route must cap the client.
        let mut route = test_route();
        route.max_tokens = Some(100);
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &route,
            None,
            "test-model",
            false,
            None,
            Some(1000),
            None,
            None,
            None,
        );
        let gc = req.generation_config.unwrap();
        assert_eq!(gc.max_output_tokens, Some(100));
    }

    #[test]
    fn max_tokens_no_route_limit_client_used() {
        // Client sends max_tokens=500, no route limit. Client value preserved.
        let route = test_route();
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &route,
            None,
            "test-model",
            false,
            None,
            Some(500),
            None,
            None,
            None,
        );
        let gc = req.generation_config.unwrap();
        assert_eq!(gc.max_output_tokens, Some(500));
    }

    #[test]
    fn max_tokens_no_client_limit_route_used() {
        // No client limit, route has max_tokens=100. Route value used.
        let mut route = test_route();
        route.max_tokens = Some(100);
        let req = build_interactions_request_anthropic(
            &anthropic_msgs(),
            0,
            &route,
            None,
            "test-model",
            false,
            None,
            None,
            None,
            None,
            None,
        );
        let gc = req.generation_config.unwrap();
        assert_eq!(gc.max_output_tokens, Some(100));
    }

    #[test]
    fn token_count_saturating_conversion_above_u32_max() {
        let interaction: Interaction = serde_json::from_value(serde_json::json!({
            "id": "big",
            "status": "completed",
            "model": "test",
            "usage": {
                "total_input_tokens": 5000000000_i64,
                "total_output_tokens": 5000000000_i64,
            }
        }))
        .expect("deserialize");
        let resp = build_response_from_interaction(&interaction, "test", Protocol::Anthropic)
            .expect("should build response");
        let msg: MessageResponse = serde_json::from_value(resp).expect("should deserialize");
        assert_eq!(msg.usage.input_tokens, u32::MAX);
        assert_eq!(msg.usage.output_tokens, u32::MAX);
    }

    #[test]
    fn token_count_within_range_passes_through() {
        let interaction: Interaction = serde_json::from_value(serde_json::json!({
            "id": "small",
            "status": "completed",
            "model": "test",
            "usage": {
                "total_input_tokens": 15000,
                "total_output_tokens": 8000,
            }
        }))
        .expect("deserialize");
        let resp = build_response_from_interaction(&interaction, "test", Protocol::Anthropic)
            .expect("should build response");
        let msg: MessageResponse = serde_json::from_value(resp).expect("should deserialize");
        assert_eq!(msg.usage.input_tokens, 15000);
        assert_eq!(msg.usage.output_tokens, 8000);
    }

    // --- tool_result extraction tests ---

    #[test]
    fn extract_anthropic_content_tool_result_string() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny"}]
        });
        let result = extract_anthropic_content(&msg);
        let content = result.expect("should extract tool_result with string content");
        match content {
            Content::TextContent(tc) => assert_eq!(tc.text, "sunny"),
            other => panic!("expected TextContent, got {:?}", other),
        }
    }

    #[test]
    fn extract_anthropic_content_tool_result_array() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": [{"type": "text", "text": "result: 42"}]}]
        });
        let result = extract_anthropic_content(&msg);
        let content = result.expect("should extract tool_result with array content");
        match content {
            Content::TextContent(tc) => assert_eq!(tc.text, "result: 42"),
            other => panic!("expected TextContent, got {:?}", other),
        }
    }

    #[test]
    fn extract_anthropic_content_mixed_text_and_tool_result() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "a"},
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "b"}
            ]
        });
        let result = extract_anthropic_content(&msg);
        let content = result.expect("should extract mixed text and tool_result");
        match content {
            Content::TextContent(tc) => assert_eq!(tc.text, "a\nb"),
            other => panic!("expected TextContent, got {:?}", other),
        }
    }

    // --- Phase 1: Harness filtering and hashing ---

    #[test]
    fn filter_harness_messages_anthropic_keeps_user_only() {
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi there"}),
            serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "result"}]}),
        ];
        let filtered = filter_harness_messages(&msgs, Protocol::Anthropic);
        assert_eq!(filtered.len(), 2, "must keep two user messages");
        assert_eq!(filtered[0]["role"], "user");
        assert_eq!(filtered[1]["role"], "user");
    }

    #[test]
    fn filter_harness_messages_openai_keeps_harness_roles() {
        let msgs = vec![
            serde_json::json!({"role": "system", "content": "Be helpful"}),
            serde_json::json!({"role": "developer", "content": "Use tools"}),
            serde_json::json!({"role": "user", "content": "Hello"}),
            serde_json::json!({"role": "assistant", "content": "Hi"}),
            serde_json::json!({"role": "tool", "content": "result"}),
        ];
        let filtered = filter_harness_messages(&msgs, Protocol::OpenAi);
        assert_eq!(filtered.len(), 4, "must keep system, developer, user, tool");
        let roles: Vec<&str> = filtered
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["system", "developer", "user", "tool"]);
        // assistant must not be present
        assert!(!filtered.iter().any(|m| m["role"] == "assistant"));
    }

    #[test]
    fn hash_harness_message_is_deterministic() {
        let msg = serde_json::json!({"role": "user", "content": "hello"});
        let h1 = hash_harness_message(&msg);
        let h2 = hash_harness_message(&msg);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_harness_message_different_content_different_hash() {
        let a = serde_json::json!({"role": "user", "content": "hello"});
        let b = serde_json::json!({"role": "user", "content": "world"});
        assert_ne!(hash_harness_message(&a), hash_harness_message(&b));
    }

    #[test]
    fn hash_harness_message_includes_all_fields() {
        // Two messages with same text but different role must differ
        let a = serde_json::json!({"role": "user", "content": "hello"});
        let b = serde_json::json!({"role": "system", "content": "hello"});
        assert_ne!(hash_harness_message(&a), hash_harness_message(&b));
    }

    #[test]
    fn control_stripped_before_hashing() {
        // Given: one control-like message and one user message
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "control message"}),
            serde_json::json!({"role": "user", "content": "real message"}),
        ];
        // Simulate control stripping: only the second message remains
        let filtered: Vec<_> = msgs.iter().skip(1).cloned().collect();
        let hashes: Vec<u64> = filtered.iter().map(|m| hash_harness_message(m)).collect();
        assert_eq!(hashes.len(), 1);
        // The stripped message must NOT participate in hashing
        assert_eq!(
            hashes[0],
            hash_harness_message(&serde_json::json!({"role": "user", "content": "real message"}))
        );
    }

    // ── Phase 6.1: proxy_limit packing measures full body ─────────

    #[test]
    fn pack_chunk_body_includes_previous_interaction_id_overhead() {
        // Each chunk body is the serialized full CreateModelInteractionParams,
        // including previous_interaction_id. Verify size measurement accounts for it.
        let envelope = pack_envelope("test-model");
        let env_size = serde_json::to_vec(&envelope).unwrap().len();
        let mut sub_env = envelope.clone();
        sub_env.previous_interaction_id = Some("x".repeat(36));

        let sub_size = serde_json::to_vec(&sub_env).unwrap().len();
        assert!(
            sub_size > env_size,
            "subsequent envelope with previous_interaction_id must be larger than first envelope"
        );

        // Verify each emitted chunk body is <= limit
        let limit = sub_size + 100;
        let items = vec![
            content_of_approx_size(40),
            content_of_approx_size(40),
            content_of_approx_size(40),
        ];
        let result = pack_content_into_chunks(&envelope, &sub_env, &items, limit).unwrap();
        for chunk in &result {
            let env = if result
                .first()
                .map(|c| c.as_ptr() == chunk.as_ptr())
                .unwrap_or(false)
            {
                &envelope
            } else {
                &sub_env
            };
            let body = build_pack_body(env, chunk);
            let size = serde_json::to_vec(&body).unwrap().len();
            assert!(size <= limit, "chunk body {size} > limit {limit}");
        }
    }

    #[test]
    fn pack_chunk_size_accounts_for_all_fields() {
        // Tool and generation_config fields in first envelope increase chunk size.
        let envelope = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            tools: Some(vec![Tool::Function(crate::interactions_types::Function {
                name: Some("my_tool".into()),
                description: Some("A test tool".into()),
                parameters: Some(serde_json::json!({"type": "object"})),
                ..Default::default()
            })]),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.7),
                max_output_tokens: Some(1000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let sub_env = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            previous_interaction_id: Some("x".repeat(36)),
            ..Default::default()
        };

        let env_size = serde_json::to_vec(&envelope).unwrap().len();
        let limit = env_size + 150;
        let items = vec![content_of_approx_size(50)];
        let result = pack_content_into_chunks(&envelope, &sub_env, &items, limit).unwrap();
        assert_eq!(result.len(), 1);
        let body = build_pack_body(&envelope, &result[0]);
        let size = serde_json::to_vec(&body).unwrap().len();
        assert!(
            size <= limit,
            "chunk with tools+gen_config: {size} > {limit}"
        );
    }

    // ── Phase 6.2: system instruction split precedes content ──────

    #[test]
    fn system_instruction_first_chunk_carries_tools_and_gen_config() {
        // Verify the structure expected by system instruction splitting:
        // first chunk envelope (with tools, generation_config) is larger
        // than subsequent chunks, and system_instruction is the first thing split.
        let first_env = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            system_instruction: Some("short sys".into()),
            tools: Some(vec![Tool::Function(crate::interactions_types::Function {
                name: Some("t".into()),
                description: None,
                parameters: Some(serde_json::json!({"type": "object"})),
                ..Default::default()
            })]),
            generation_config: Some(GenerationConfig {
                temperature: Some(0.7),
                ..Default::default()
            }),
            ..Default::default()
        };
        let sub_env = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            previous_interaction_id: Some("x".repeat(36)),
            ..Default::default()
        };
        // First envelope with tools+gen_config+sys must be larger
        let first_size = serde_json::to_vec(&first_env).unwrap().len();
        let sub_size = serde_json::to_vec(&sub_env).unwrap().len();
        assert!(first_size > sub_size,
            "first envelope ({first_size}) with tools+gen_config must be larger than subsequent ({sub_size})");
    }

    #[test]
    fn system_instruction_split_parts_fit_under_limit() {
        // When system_instruction needs splitting, each part (when wrapped in
        // an envelope without tools) must fit under the limit.
        let envelope = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            system_instruction: None,
            previous_interaction_id: Some("x".repeat(36)),
            ..Default::default()
        };
        let env_size = serde_json::to_vec(&envelope).unwrap().len();
        let limit = env_size + 100;
        let sys_part = "x".repeat(50);
        let mut env_with_sys = envelope.clone();
        env_with_sys.system_instruction = Some(sys_part);
        let size = serde_json::to_vec(&env_with_sys).unwrap().len();
        assert!(
            size <= limit,
            "system part + envelope ({size}) <= limit ({limit})"
        );
    }
}
