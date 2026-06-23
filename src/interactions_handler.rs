//! Gemini Interactions API handler.
//!
//! Handles Anthropic→Interactions and OpenAI→Interactions translation,
//! session state management, control messages, proxy_limit splitting,
//! and response translation back to the client's protocol.

#![allow(clippy::too_many_arguments)]

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::Client as HttpClient;

use crate::auth::{is_auth_header, should_forward_request_header};
use crate::config::{Config, Protocol, RouteTarget};
use crate::control::{scan_control_messages, ControlAction};
use crate::diagnostics::Diagnostics;
use crate::error::AppError;
use crate::interactions as interactions_lib;
use crate::interactions_types::{
    CreateModelInteractionParams, Interaction, InteractionSseEvent, InteractionsInput, Step,
    StepDeltaData,
};
use crate::session::SessionStore;
use crate::sse;
use anyllm_translate::anthropic::{ContentBlock, MessageResponse, Role, StopReason, Usage};
use anyllm_translate::openai::{
    ChatCompletionResponse, ChatContent, ChatMessage, ChatRole, ChatUsage, Choice, FinishReason,
};

pub const API_REVISION: &str = "2026-05-20";

#[derive(Clone)]
pub struct InteractionsHandler {
    http: HttpClient,
    diagnostics: Diagnostics,
    session_store: Arc<SessionStore>,
    error_translation: Arc<[crate::config::ErrorTranslationRule]>,
}

impl InteractionsHandler {
    pub fn new(
        config: &Config,
        diagnostics: Diagnostics,
        session_store: Arc<SessionStore>,
    ) -> Result<Self, AppError> {
        let mut client_builder = HttpClient::builder().timeout(config.upstream_timeout);
        // Apply per-section proxy if configured (use first interactions section's proxy)
        for section in config.sections.values() {
            if section.endpoint_interactions.is_some() {
                if let Some(ref proxy_url) = section.proxy {
                    if let Ok(proxy) = reqwest::Proxy::all(proxy_url) {
                        client_builder = client_builder.proxy(proxy);
                    }
                }
                break;
            }
        }
        Ok(Self {
            http: client_builder
                .build()
                .map_err(|err| AppError::Internal(err.to_string()))?,
            diagnostics,
            session_store,
            error_translation: config.error_translation.clone().into(),
        })
    }

    /// Anthropic ingress → Interactions upstream.
    pub async fn handle_from_anthropic(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        endpoint: &str,
    ) -> Result<Response, AppError> {
        let body_val: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
        let model = body_val
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        let guard =
            crate::diagnostics::RequestDiagnostics::new(&self.diagnostics, &route.section, &model);
        guard.ingress_dump(body, request_headers);

        // Extract session ID
        let session_id = self.resolve_session_id(request_headers, &body_val);

        // Process control messages
        let messages = body_val
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        let mut processed_hashes = HashSet::new();
        let control_result = scan_control_messages(
            &messages,
            route.control_clean_all.as_deref(),
            route.control_extend_lifetime.as_deref(),
            &mut processed_hashes,
        );

        // Execute control action if any
        if let Some(action) = &control_result.action {
            return self
                .handle_control_action(action, &session_id, route, Protocol::Anthropic, guard)
                .await;
        }

        // Get session state for delta computation (Anthropic ingress)
        let session = self.session_store.get_or_create(&session_id).await;
        let delivered = session.message_count;
        let incoming_count = messages.len() - control_result.stripped_count;
        let (start_index, new_count) = crate::session::compute_delta(delivered, incoming_count);

        // Use cleaned messages (control messages removed) for the upstream request
        let cleaned_messages = if control_result.stripped_count > 0 {
            control_result.cleaned_messages
        } else {
            messages
        };

        // Extract typed scalars from ingress body
        let stream = body_val
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        let temperature = body_val.get("temperature").and_then(|v| v.as_f64());
        let ingress_max_tokens = body_val
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let system = interactions_lib::extract_anthropic_system(&body_val);
        let (tools, tool_choice) = interactions_lib::extract_anthropic_tools(&body_val);

        let prev_id = if session.interaction_id.is_empty() {
            None
        } else {
            Some(session.interaction_id.as_str())
        };

        // Build the request
        let params = interactions_lib::build_interactions_request_anthropic(
            &cleaned_messages,
            start_index,
            route,
            prev_id,
            &model,
            stream,
            temperature,
            ingress_max_tokens,
            system,
            tools,
            tool_choice,
        );

        // Send to upstream
        let backend_url = endpoint.to_string();
        let request_body =
            serde_json::to_vec(&params).map_err(|e| AppError::Internal(e.to_string()))?;

        // Apply proxy_limit splitting if needed
        if let Some(limit) = route.proxy_limit {
            let contents = match &params.input {
                InteractionsInput::ContentList(list) => list.clone(),
                _ => vec![],
            };
            let size = serde_json::to_vec(&params).map(|v| v.len()).unwrap_or(0);
            if size > limit {
                if let Err(msg) = interactions_lib::can_split_under_limit(&params, limit) {
                    guard.finish_with_error(
                        400,
                        0,
                        body.len(),
                        None,
                        "anthropic->interactions",
                        "anthropic->interactions",
                        stream,
                        msg.clone(),
                    );
                    return Err(AppError::BadRequest(format!(
                        "Request cannot be split under proxy limit (see diagnostics for details)"
                    )));
                }
                return self
                    .handle_split_send(
                        &params,
                        &contents,
                        limit,
                        &backend_url,
                        route,
                        &session_id,
                        new_count,
                        stream,
                        &model,
                        endpoint,
                        body,
                        "anthropic->interactions",
                        request_headers,
                        Protocol::Anthropic,
                        guard,
                    )
                    .await;
            }
        }

        self.send_and_translate(
            &backend_url,
            &request_body,
            route,
            &session_id,
            new_count,
            stream,
            &model,
            endpoint,
            body,
            "anthropic->interactions",
            Protocol::Anthropic,
            request_headers,
            guard,
        )
        .await
    }

    /// OpenAI ingress → Interactions upstream.
    pub async fn handle_from_openai(
        &self,
        body: &[u8],
        request_headers: &HeaderMap,
        route: &RouteTarget,
        endpoint: &str,
    ) -> Result<Response, AppError> {
        let body_val: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
        let model = body_val
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        let guard =
            crate::diagnostics::RequestDiagnostics::new(&self.diagnostics, &route.section, &model);
        guard.ingress_dump(body, request_headers);

        let session_id = self.resolve_session_id(request_headers, &body_val);

        // Process control messages
        let messages = body_val
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        let mut processed_hashes = HashSet::new();
        let control_result = scan_control_messages(
            &messages,
            route.control_clean_all.as_deref(),
            route.control_extend_lifetime.as_deref(),
            &mut processed_hashes,
        );

        if let Some(action) = &control_result.action {
            return self
                .handle_control_action(action, &session_id, route, Protocol::OpenAi, guard)
                .await;
        }

        let session = self.session_store.get_or_create(&session_id).await;
        let delivered = session.message_count;
        let incoming_count = messages.len() - control_result.stripped_count;
        let (start_index, new_count) = crate::session::compute_delta(delivered, incoming_count);

        // Use cleaned messages (control messages removed) for the upstream request
        let cleaned_messages = if control_result.stripped_count > 0 {
            control_result.cleaned_messages
        } else {
            messages
        };

        // Extract typed scalars from ingress body
        let stream = body_val
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        let temperature = body_val.get("temperature").and_then(|v| v.as_f64());
        let ingress_max_tokens = body_val
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let (tools, tool_choice) = interactions_lib::extract_openai_tools(&body_val);

        let prev_id = if session.interaction_id.is_empty() {
            None
        } else {
            Some(session.interaction_id.as_str())
        };

        let params = interactions_lib::build_interactions_request_openai(
            &cleaned_messages,
            start_index,
            route,
            prev_id,
            &model,
            stream,
            temperature,
            ingress_max_tokens,
            tools,
            tool_choice,
        );

        let backend_url = endpoint.to_string();
        let request_body =
            serde_json::to_vec(&params).map_err(|e| AppError::Internal(e.to_string()))?;

        if let Some(limit) = route.proxy_limit {
            let contents = match &params.input {
                InteractionsInput::ContentList(list) => list.clone(),
                _ => vec![],
            };
            let size = serde_json::to_vec(&params).map(|v| v.len()).unwrap_or(0);
            if size > limit {
                if let Err(msg) = interactions_lib::can_split_under_limit(&params, limit) {
                    guard.finish_with_error(
                        400,
                        0,
                        body.len(),
                        None,
                        "openai->interactions",
                        "openai->interactions",
                        stream,
                        msg.clone(),
                    );
                    return Err(AppError::BadRequest(format!(
                        "Request cannot be split under proxy limit (see diagnostics for details)"
                    )));
                }
                return self
                    .handle_split_send(
                        &params,
                        &contents,
                        limit,
                        &backend_url,
                        route,
                        &session_id,
                        new_count,
                        stream,
                        &model,
                        endpoint,
                        body,
                        "openai->interactions",
                        request_headers,
                        Protocol::OpenAi,
                        guard,
                    )
                    .await;
            }
        }

        self.send_and_translate(
            &backend_url,
            &request_body,
            route,
            &session_id,
            new_count,
            stream,
            &model,
            endpoint,
            body,
            "openai->interactions",
            Protocol::OpenAi,
            request_headers,
            guard,
        )
        .await
    }

    /// Send a single interaction request and translate response.
    #[allow(clippy::too_many_arguments)]
    async fn send_and_translate(
        &self,
        url: &str,
        egress_body: &[u8],
        route: &RouteTarget,
        session_id: &str,
        new_count: usize,
        stream: bool,
        model: &str,
        upstream_label: &str,
        ingress_body: &[u8],
        direction: &str,
        ingress: Protocol,
        request_headers: &HeaderMap,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        let egress_headers =
            build_interactions_headers_map(route.api_key.as_deref(), request_headers);
        guard.egress_dump(egress_body, &egress_headers);

        let builder = build_interactions_headers(
            self.http
                .post(url)
                .header(header::CONTENT_TYPE, "application/json"),
            route.api_key.as_deref(),
            request_headers,
        );

        let start = std::time::Instant::now();
        let upstream = builder.body(egress_body.to_vec()).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream.text().await.unwrap_or_default();
            guard.response_dump(
                crate::diagnostics::dump_body_from_bytes(error_body.as_bytes()),
                status.as_u16(),
                true,
                vec![],
            );
            guard.finish_with_error(
                status.as_u16(),
                duration_ms,
                ingress_body.len(),
                Some(error_body.len()),
                upstream_label,
                direction,
                stream,
                error_body.clone(),
            );
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::translate_interactions_error_to_protocol(&error_body, ingress);
            let body = crate::apply_error_translation(sc, body, &self.error_translation);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .header(Self::session_header_name(ingress), session_id)
                .body(Body::from(body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }

        if stream {
            return self
                .handle_stream_response(
                    upstream,
                    session_id,
                    new_count,
                    model,
                    upstream_label,
                    direction,
                    ingress,
                    guard,
                )
                .await;
        }

        let response_body_bytes = upstream.bytes().await?;
        let validated = crate::validate_upstream_body(response_body_bytes, guard.request_id())?;
        guard.response_dump(validated.dump, 200, false, vec![]);
        let response_body = validated.text;
        let interaction: Interaction = serde_json::from_str(&response_body).map_err(|e| {
            AppError::Upstream(format!("failed to parse interaction response: {e}"))
        })?;

        // Update session
        let interaction_id = interaction.id.clone();
        let _ = self
            .session_store
            .update(session_id, interaction_id, new_count, false)
            .await;

        // Translate response back to ingress protocol
        let resp = interactions_lib::build_response_from_interaction(&interaction, model, ingress)
            .map_err(AppError::Internal)?;

        guard.finish(
            200,
            duration_ms,
            ingress_body.len(),
            Some(response_body.len()),
            upstream_label,
            direction,
            stream,
        );

        Ok(Self::ok_with_session_header(ingress, session_id, resp))
    }

    /// Stream interactions response events translated to the client's protocol.
    #[allow(clippy::too_many_arguments)]
    async fn handle_stream_response(
        &self,
        upstream: reqwest::Response,
        session_id: &str,
        new_count: usize,
        model: &str,
        upstream_label: &str,
        direction: &str,
        ingress: Protocol,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        let request_size = guard.ingress_size();
        let request_id = guard.request_id().to_string();
        let mut byte_stream = upstream.bytes_stream();
        let mut buffer = String::new();
        let mut interaction_id = String::new();
        let mut total_bytes: usize = 0;
        let mut dump_buffer: Vec<u8> = Vec::new();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);

        let model_owned = model.to_string();
        let session_store = self.session_store.clone();
        let sid = session_id.to_string();
        let dir = direction.to_string();
        let label = upstream_label.to_string();
        let start = std::time::Instant::now();
        let is_openai = matches!(ingress, Protocol::OpenAi);
        let dump_enabled = self.diagnostics.dump_enabled();
        let session_hdr_name = Self::session_header_name(ingress);
        let session_hdr_value = session_id.to_string();

        // Eagerly commit message_count to prevent racing follow-up requests
        // from re-sending messages the in-flight stream is about to deliver.
        // interaction_id is set to empty (pending) — updated after stream completes.
        let _ = session_store
            .update(&sid, String::new(), new_count, false)
            .await;

        tokio::spawn(async move {
            let mut translator: Option<
                anyllm_translate::mapping::reverse_streaming_map::ReverseStreamingTranslator,
            > = None;
            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        total_bytes += chunk.len();
                        if dump_enabled
                            && dump_buffer.len() < crate::relay::MAX_STREAMING_DUMP_BYTES
                        {
                            let remaining =
                                crate::relay::MAX_STREAMING_DUMP_BYTES - dump_buffer.len();
                            let to_take = std::cmp::min(chunk.len(), remaining);
                            dump_buffer.extend_from_slice(&chunk[..to_take]);
                        }
                        // Reject non-UTF-8 chunks — from_utf8_lossy would silently
                        // produce garbage that confuses the downstream agent.
                        if std::str::from_utf8(&chunk).is_err() {
                            let err_payload = bytes::Bytes::from(sse::format_sse_event_str(
                                &anyllm_translate::anthropic::StreamEvent::Error {
                                    error: anyllm_translate::anthropic::streaming::StreamError {
                                        error_type: "upstream_error".into(),
                                        message: "non-utf8 response from upstream".into(),
                                    },
                                },
                            ));
                            let _ = tx.send(Ok(err_payload)).await;
                            let duration_ms = start.elapsed().as_millis() as u64;
                            guard.finish_with_error(
                                502,
                                duration_ms,
                                request_size,
                                Some(total_bytes),
                                &label,
                                &dir,
                                true,
                                "non-utf8 streaming response from upstream".into(),
                            );
                            return;
                        }
                        buffer.push_str(std::str::from_utf8(&chunk).unwrap());

                        // Parse complete SSE events separated by \n\n
                        while let Some(pos) = buffer.find("\n\n") {
                            let event_str = buffer[..pos].to_string();
                            buffer = buffer[pos + 2..].to_string();

                            for line in event_str.lines() {
                                let data = line
                                    .strip_prefix("data: ")
                                    .or_else(|| line.strip_prefix("data:"));
                                let data = match data {
                                    Some(d) => d.trim(),
                                    None => continue,
                                };
                                if data.is_empty() || data == "[DONE]" {
                                    continue;
                                }
                                if let Some(events) =
                                    translate_stream_event(data, &model_owned, &model_owned)
                                {
                                    for event in &events {
                                        if is_openai {
                                            // Pipe StreamEvent through ReverseStreamingTranslator
                                            // to produce OpenAI ChatCompletionChunk SSE.
                                            if matches!(
                                                event,
                                                anyllm_translate::anthropic::StreamEvent::MessageStart { .. }
                                            ) {
                                                if let anyllm_translate::anthropic::StreamEvent::MessageStart { message } = event {
                                                    translator = Some(
                                                        anyllm_translate::new_reverse_stream_translator(
                                                            message.id.clone(),
                                                            model_owned.clone(),
                                                        ),
                                                    );
                                                }
                                                continue;
                                            }
                                            if let Some(ref mut active) = translator {
                                                if let Some(chunk) =
                                                    active.process_event(event).into_iter().next()
                                                {
                                                    let payload =
                                                        sse::format_openai_sse_chunk(&chunk);
                                                    let payload = bytes::Bytes::from(payload);
                                                    if tx.send(Ok(payload)).await.is_err() {
                                                        let duration_ms =
                                                            start.elapsed().as_millis() as u64;
                                                        guard.finish(
                                                            499,
                                                            duration_ms,
                                                            request_size,
                                                            Some(total_bytes),
                                                            &label,
                                                            &dir,
                                                            true,
                                                        );
                                                        return;
                                                    }
                                                }
                                            }
                                        } else {
                                            let payload = sse::format_sse_event(event);
                                            if tx.send(Ok(payload)).await.is_err() {
                                                let duration_ms =
                                                    start.elapsed().as_millis() as u64;
                                                guard.finish(
                                                    499,
                                                    duration_ms,
                                                    request_size,
                                                    Some(total_bytes),
                                                    &label,
                                                    &dir,
                                                    true,
                                                );
                                                return;
                                            }
                                        }
                                    }
                                    // Track interaction_id from interaction.created events
                                    if let Ok(InteractionSseEvent::InteractionCreatedEvent(ev)) =
                                        serde_json::from_str::<InteractionSseEvent>(data)
                                    {
                                        interaction_id = ev.interaction.id;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(std::io::Error::other(format!("stream error: {e}"))))
                            .await;
                        let duration_ms = start.elapsed().as_millis() as u64;
                        guard.finish_with_error(
                            502,
                            duration_ms,
                            request_size,
                            Some(total_bytes),
                            &label,
                            &dir,
                            true,
                            format!("stream error: {e}"),
                        );
                        return;
                    }
                }
            }

            // Flush remaining OpenAI chunks on stream end
            if translator.is_some() {
                let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
            }

            // Update session after stream completes
            if !interaction_id.is_empty() {
                let _ = session_store
                    .update(&sid, interaction_id, new_count, false)
                    .await;
            }

            // Response dump for streaming
            let dump_body = crate::diagnostics::dump_body_from_bytes(&dump_buffer);
            if dump_body.is_base64() {
                tracing::warn!(
                    request_id = %request_id,
                    direction = "response",
                    body_len = dump_buffer.len(),
                    "non-utf8 streaming interactions upstream response"
                );
            }
            guard.response_dump_streaming(dump_body, 200);

            let duration_ms = start.elapsed().as_millis() as u64;
            guard.finish(
                200,
                duration_ms,
                request_size,
                Some(total_bytes),
                &label,
                &dir,
                true,
            );
        });

        let sse_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        sse::sse_response_with_extra_header(
            &HeaderMap::new(),
            sse_stream,
            session_hdr_name,
            &session_hdr_value,
        )
    }

    /// Handle split sending: break content into chunks under proxy_limit.
    async fn handle_split_send(
        &self,
        params: &CreateModelInteractionParams,
        contents: &[crate::interactions_types::Content],
        limit: usize,
        url: &str,
        route: &RouteTarget,
        session_id: &str,
        total_message_count: usize,
        _stream: bool,
        model: &str,
        upstream_label: &str,
        ingress_body: &[u8],
        direction: &str,
        request_headers: &HeaderMap,
        ingress: Protocol,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        let start = std::time::Instant::now();
        let mut total_response_bytes: usize = 0;

        let egress_headers =
            build_interactions_headers_map(route.api_key.as_deref(), request_headers);

        let chunks = interactions_lib::split_content_for_limit(contents, limit);
        let mut last_id: Option<String> = None;
        let mut last_interaction: Option<Interaction> = None;

        // Check if there's a system_instruction that needs splitting
        let system_instruction = params.system_instruction.clone();

        // If system_instruction + empty content exceeds limit, split system_instruction first
        if let Some(ref sys) = system_instruction {
            let empty_body = CreateModelInteractionParams {
                model: model.to_string(),
                input: InteractionsInput::ContentList(vec![]),
                stream: Some(false),
                system_instruction: Some(sys.clone()),
                ..Default::default()
            };
            let empty_size = serde_json::to_vec(&empty_body)
                .map(|v| v.len())
                .unwrap_or(0);
            if empty_size > limit {
                return self
                    .send_split_system_instruction(
                        sys,
                        url,
                        route,
                        session_id,
                        &chunks,
                        total_message_count,
                        limit,
                        model,
                        upstream_label,
                        ingress_body,
                        direction,
                        request_headers,
                        ingress,
                        guard,
                    )
                    .await;
            }
        }

        // Send each chunk sequentially
        let mut current_prev = params.previous_interaction_id.clone();

        for chunk in &chunks {
            let chunk_req = interactions_lib::build_chunk_request(
                model,
                chunk.clone(),
                system_instruction.clone(),
                current_prev.clone(),
            );
            let chunk_body =
                serde_json::to_vec(&chunk_req).map_err(|e| AppError::Internal(e.to_string()))?;

            guard.egress_dump(&chunk_body, &egress_headers);

            let builder = build_interactions_headers(
                self.http
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
                request_headers,
            );
            let upstream = builder.body(chunk_body).send().await?;
            if !upstream.status().is_success() {
                let status = upstream.status();
                let error_body = upstream.text().await.unwrap_or_default();
                guard.response_dump(
                    crate::diagnostics::dump_body_from_bytes(error_body.as_bytes()),
                    status.as_u16(),
                    true,
                    vec![],
                );
                guard.finish_with_error(
                    status.as_u16(),
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    Some(error_body.len()),
                    upstream_label,
                    direction,
                    false,
                    error_body.clone(),
                );
                let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let body = crate::translate_interactions_error_to_protocol(&error_body, ingress);
                let body = crate::apply_error_translation(sc, body, &self.error_translation);
                return Response::builder()
                    .status(sc)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()));
            }
            let response_bytes = upstream.bytes().await?;
            let validated = crate::validate_upstream_body(response_bytes, guard.request_id())?;
            guard.response_dump(validated.dump, 200, false, vec![]);
            let response_text = validated.text;
            let interaction: Interaction = serde_json::from_str(&response_text).map_err(|e| {
                AppError::Upstream(format!("failed to parse split interaction: {e}"))
            })?;
            total_response_bytes += response_text.len();
            current_prev = Some(interaction.id.clone());
            last_id = Some(interaction.id.clone());
            last_interaction = Some(interaction);
        }

        if let Some(ref final_id) = last_id {
            let _ = self
                .session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await;
        }

        let resp = if let Some(ref inter) = last_interaction {
            interactions_lib::build_response_from_interaction(inter, model, ingress)
                .map_err(AppError::Internal)?
        } else {
            build_fallback_response(last_interaction.as_ref(), last_id.clone(), model, ingress)?
        };
        guard.finish(
            200,
            start.elapsed().as_millis() as u64,
            ingress_body.len(),
            Some(total_response_bytes),
            upstream_label,
            direction,
            false,
        );
        Ok(Self::ok_with_session_header(ingress, session_id, resp))
    }

    /// Split system_instruction across multiple interactions, then send chunks.
    async fn send_split_system_instruction(
        &self,
        sys: &str,
        url: &str,
        route: &RouteTarget,
        session_id: &str,
        chunks: &[Vec<crate::interactions_types::Content>],
        total_message_count: usize,
        limit: usize,
        model: &str,
        upstream_label: &str,
        ingress_body: &[u8],
        direction: &str,
        request_headers: &HeaderMap,
        ingress: Protocol,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        let start = std::time::Instant::now();
        let mut total_response_bytes: usize = 0;
        let egress_headers =
            build_interactions_headers_map(route.api_key.as_deref(), request_headers);
        // Split system_instruction on natural boundaries
        let sys_parts = split_text_for_limit(sys, limit).map_err(AppError::BadRequest)?;
        let mut last_id: Option<String> = None;
        let mut last_interaction: Option<Interaction> = None;
        let mut current_prev: Option<String> = None;

        // Send empty interactions with system_instruction chunks
        for (i, part) in sys_parts.iter().enumerate() {
            let is_last_sys = i == sys_parts.len() - 1;
            let input_for_chunk = if is_last_sys && !chunks.is_empty() {
                chunks[0].clone()
            } else {
                vec![]
            };
            let chunk_req = interactions_lib::build_chunk_request(
                model,
                input_for_chunk,
                Some(part.clone()),
                current_prev.clone(),
            );
            let chunk_body =
                serde_json::to_vec(&chunk_req).map_err(|e| AppError::Internal(e.to_string()))?;

            guard.egress_dump(&chunk_body, &egress_headers);

            let builder = build_interactions_headers(
                self.http
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
                request_headers,
            );
            let upstream = builder.body(chunk_body).send().await?;
            let upstream_status = upstream.status();
            if !upstream_status.is_success() {
                let error_body = upstream.text().await.unwrap_or_default();
                guard.response_dump(
                    crate::diagnostics::dump_body_from_bytes(error_body.as_bytes()),
                    upstream_status.as_u16(),
                    true,
                    vec![],
                );
                guard.finish_with_error(
                    upstream_status.as_u16(),
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    Some(error_body.len()),
                    upstream_label,
                    direction,
                    false,
                    error_body.clone(),
                );
                let sc = StatusCode::from_u16(upstream_status.as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let body = crate::translate_interactions_error_to_protocol(&error_body, ingress);
                let body = crate::apply_error_translation(sc, body, &self.error_translation);
                return Response::builder()
                    .status(sc)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()));
            }
            let response_bytes = upstream.bytes().await?;
            total_response_bytes += response_bytes.len();
            let validated = crate::validate_upstream_body(response_bytes, guard.request_id())?;
            guard.response_dump(validated.dump, 200, false, vec![]);
            let response_text = validated.text;
            if let Ok(interaction) = serde_json::from_str::<Interaction>(&response_text) {
                current_prev = Some(interaction.id.clone());
                last_id = Some(interaction.id);
            }
        }

        // Send remaining chunks if more than one
        if chunks.len() > 1 {
            for chunk in chunks.iter().skip(1) {
                let chunk_req = interactions_lib::build_chunk_request(
                    model,
                    chunk.clone(),
                    None,
                    current_prev.clone(),
                );
                let chunk_body = serde_json::to_vec(&chunk_req)
                    .map_err(|e| AppError::Internal(e.to_string()))?;

                guard.egress_dump(&chunk_body, &egress_headers);

                let builder = build_interactions_headers(
                    self.http
                        .post(url)
                        .header(header::CONTENT_TYPE, "application/json"),
                    route.api_key.as_deref(),
                    request_headers,
                );
                let upstream = builder.body(chunk_body).send().await?;
                let upstream_status = upstream.status();
                if !upstream_status.is_success() {
                    let error_body = upstream.text().await.unwrap_or_default();
                    guard.response_dump(
                        crate::diagnostics::dump_body_from_bytes(error_body.as_bytes()),
                        upstream_status.as_u16(),
                        true,
                        vec![],
                    );
                    guard.finish_with_error(
                        upstream_status.as_u16(),
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        Some(error_body.len()),
                        upstream_label,
                        direction,
                        false,
                        error_body.clone(),
                    );
                    let sc = StatusCode::from_u16(upstream_status.as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let body =
                        crate::translate_interactions_error_to_protocol(&error_body, ingress);
                    let body = crate::apply_error_translation(sc, body, &self.error_translation);
                    return Response::builder()
                        .status(sc)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .map_err(|err| AppError::Internal(err.to_string()));
                }
                let response_bytes = upstream.bytes().await?;
                total_response_bytes += response_bytes.len();
                let validated = crate::validate_upstream_body(response_bytes, guard.request_id())?;
                guard.response_dump(validated.dump, 200, false, vec![]);
                let response_text = validated.text;
                if let Ok(interaction) = serde_json::from_str::<Interaction>(&response_text) {
                    current_prev = Some(interaction.id.clone());
                    last_id = Some(interaction.id.clone());
                    last_interaction = Some(interaction);
                }
            }
        }

        if let Some(ref final_id) = last_id {
            let _ = self
                .session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await;
        }

        // Translate last response to ingress protocol
        let resp = if let Some(ref inter) = last_interaction {
            interactions_lib::build_response_from_interaction(inter, model, ingress)
                .map_err(AppError::Internal)?
        } else {
            build_fallback_response(last_interaction.as_ref(), last_id.clone(), model, ingress)?
        };
        guard.finish(
            200,
            start.elapsed().as_millis() as u64,
            ingress_body.len(),
            Some(total_response_bytes),
            upstream_label,
            direction,
            false,
        );
        Ok(Self::ok_with_session_header(ingress, session_id, resp))
    }

    /// Cancel an interaction upstream (ignores 404).
    async fn cancel_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<(), AppError> {
        let url = build_interaction_url(route, &format!("/{interaction_id}/cancel"));
        let builder = build_interactions_headers(
            self.http.post(&url),
            route.api_key.as_deref(),
            &HeaderMap::new(),
        );
        match builder.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "cancel interaction failed");
                Err(AppError::Internal(format!(
                    "cancel interaction {interaction_id}: {e}"
                )))
            }
        }
    }

    async fn delete_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<(), AppError> {
        let url = build_interaction_url(route, &format!("/{interaction_id}"));
        let builder = build_interactions_headers(
            self.http.delete(&url),
            route.api_key.as_deref(),
            &HeaderMap::new(),
        );
        match builder.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "delete interaction failed");
                Err(AppError::Internal(format!(
                    "delete interaction {interaction_id}: {e}"
                )))
            }
        }
    }

    /// Execute a control action and return the response.
    async fn handle_control_action(
        &self,
        action: &ControlAction,
        session_id: &str,
        route: &RouteTarget,
        ingress: Protocol,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        match action {
            ControlAction::CleanAll => {
                let all =
                    self.session_store.remove_all().await.map_err(|e| {
                        AppError::Internal(format!("session clean-all failed: {e}"))
                    })?;
                let mut cancelled = 0usize;
                let mut deleted = 0usize;
                let mut errors: Vec<String> = Vec::new();
                for (_sid, state) in &all {
                    if !state.interaction_id.is_empty() {
                        if let Err(e) = self.cancel_interaction(&state.interaction_id, route).await
                        {
                            errors.push(format!("cancel {}: {}", state.interaction_id, e));
                        } else {
                            cancelled += 1;
                        }
                        if let Err(e) = self.delete_interaction(&state.interaction_id, route).await
                        {
                            errors.push(format!("delete {}: {}", state.interaction_id, e));
                        } else {
                            deleted += 1;
                        }
                    }
                }
                let msg = if errors.is_empty() {
                    format!(
                        "Cleaned all {} sessions ({} cancelled, {} deleted)",
                        all.len(),
                        cancelled,
                        deleted
                    )
                } else {
                    format!(
                        "Cleaned all {} sessions ({} cancelled, {} deleted). Errors: {}",
                        all.len(),
                        cancelled,
                        deleted,
                        errors.join("; ")
                    )
                };
                guard.finish(
                    200,
                    0,
                    0,
                    None,
                    "control-action",
                    "clean-all",
                    false,
                );
                Ok(Self::ok_with_session_header(
                    ingress,
                    session_id,
                    serde_json::json!({"status": "ok", "message": msg}),
                ))
            }
            ControlAction::ExtendLifetime(until) => {
                self.session_store
                    .extend_lifetime(session_id, *until)
                    .await
                    .map_err(|e| AppError::Internal(format!("session extend failed: {e}")))?;
                let msg = format!("Session {} lifetime extended to UTC {}", session_id, until);
                guard.finish(
                    200,
                    0,
                    0,
                    None,
                    "control-action",
                    "extend-lifetime",
                    false,
                );
                Ok(Self::ok_with_session_header(
                    ingress,
                    session_id,
                    serde_json::json!({"status": "ok", "message": msg}),
                ))
            }
        }
    }

    /// GET interaction state (for startup pending verification).
    pub async fn get_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<bool, String> {
        let url = build_interaction_url(route, &format!("/{interaction_id}"));
        let builder = build_interactions_headers(
            self.http.get(&url),
            route.api_key.as_deref(),
            &HeaderMap::new(),
        );
        match builder.send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "get interaction failed");
                Err(e.to_string())
            }
        }
    }

    fn resolve_session_id(&self, headers: &HeaderMap, body: &serde_json::Value) -> String {
        // Priority: x-request-id header → x-claude-code-session-id header → request_id body field → random
        if let Some(hdr) = headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
        {
            return hdr.to_string();
        }
        if let Some(hdr) = headers
            .get("x-claude-code-session-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
        {
            return hdr.to_string();
        }
        if let Some(id) = body.get("request_id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return id.to_string();
            }
        }
        uuid::Uuid::now_v7().to_string()
    }

    /// Maps ingress protocol to the response header name carrying the session id.
    fn session_header_name(ingress: Protocol) -> &'static str {
        match ingress {
            Protocol::Anthropic => "x-claude-code-session-id",
            Protocol::OpenAi => "x-request-id",
        }
    }

    /// Build a `200 OK` JSON response with the session identifier header.
    fn ok_with_session_header(
        ingress: Protocol,
        session_id: &str,
        json: serde_json::Value,
    ) -> Response {
        let hdr_name = Self::session_header_name(ingress);
        let hdr_value = HeaderValue::from_str(session_id).unwrap_or(HeaderValue::from_static(""));
        let mut response = (StatusCode::OK, axum::Json(json)).into_response();
        response
            .headers_mut()
            .insert(HeaderName::from_static(hdr_name), hdr_value);
        response
    }
}

/// Build a protocol-appropriate response body from the last interaction
/// when `build_response_from_interaction` is not available.
fn build_fallback_response(
    last_interaction: Option<&Interaction>,
    last_id: Option<String>,
    model: &str,
    ingress: Protocol,
) -> Result<serde_json::Value, AppError> {
    let text = last_interaction
        .map(interactions_lib::extract_interaction_text)
        .unwrap_or_default();
    let input_tokens = last_interaction
        .and_then(|i| i.usage.as_ref())
        .and_then(|u| u.total_input_tokens)
        .unwrap_or(0);
    let output_tokens = last_interaction
        .and_then(|i| i.usage.as_ref())
        .and_then(|u| u.total_output_tokens)
        .unwrap_or(0);
    match ingress {
        Protocol::OpenAi => {
            let typed = ChatCompletionResponse {
                id: last_id.unwrap_or_default(),
                object: "chat.completion".to_string(),
                model: model.to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: ChatMessage {
                        role: ChatRole::Assistant,
                        content: Some(ChatContent::Text(text)),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        refusal: None,
                        reasoning_content: None,
                    },
                    finish_reason: Some(FinishReason::Stop),
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
            serde_json::to_value(typed).map_err(|e| AppError::Internal(e.to_string()))
        }
        Protocol::Anthropic => {
            let typed = MessageResponse {
                id: last_id.unwrap_or_default(),
                response_type: "message".to_string(),
                role: Role::Assistant,
                model: model.to_string(),
                content: vec![ContentBlock::Text { text: text.clone() }],
                stop_reason: Some(StopReason::EndTurn),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: input_tokens as u32,
                    output_tokens: output_tokens as u32,
                    ..Default::default()
                },
                created: None,
            };
            serde_json::to_value(typed).map_err(|e| AppError::Internal(e.to_string()))
        }
    }
}

/// Build a `HeaderMap` with the same logic as `build_interactions_headers`
/// but returns headers directly instead of applying them to a `RequestBuilder`.
fn build_interactions_headers_map(api_key: Option<&str>, request_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        HeaderName::from_bytes(b"Api-Revision").unwrap(),
        HeaderValue::from_static(API_REVISION),
    );

    for (name, value) in request_headers.iter() {
        let name_str = name.as_str();
        if !should_forward_request_header(name_str) {
            continue;
        }
        if api_key.is_some() && is_auth_header(name_str) {
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }

    if let Some(key) = api_key {
        let _ = headers.insert(
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(key).unwrap_or(HeaderValue::from_static("")),
        );
    }
    headers
}

/// Build headers for interactions upstream requests.
fn build_interactions_headers(
    builder: reqwest::RequestBuilder,
    api_key: Option<&str>,
    request_headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    let headers = build_interactions_headers_map(api_key, request_headers);
    let mut b = builder;
    for (name, value) in headers.iter() {
        if let Ok(v) = value.to_str() {
            b = b.header(name.as_str(), v);
        }
    }
    b
}

/// Build a URL for interactions lifecycle operations.
fn build_interaction_url(route: &RouteTarget, suffix: &str) -> String {
    let base = route
        .endpoint_interactions
        .as_deref()
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta/interactions");
    let base = match base.find('?') {
        Some(pos) => &base[..pos],
        None => base,
    };
    let base = base.trim_end_matches('/');
    format!("{base}{suffix}")
}

/// Split text into chunks that each fit under `limit` bytes.
/// Uses hierarchical boundaries: \\n\\n → \\n → . → ! → ? → , → ; → char.
/// Each chunk is as large as possible while staying under the limit.
fn split_text_for_limit(text: &str, limit: usize) -> Result<Vec<String>, String> {
    if text.len() <= limit {
        return Ok(vec![text.to_string()]);
    }

    let delimiters: &[&str] = &["\n\n", "\n", ". ", "! ", "? ", ", ", "; ", " "];
    let chunks = split_by_best_delimiter(text, limit, delimiters);
    if chunks.is_empty() || chunks.iter().any(|c| c.len() > limit) {
        Err(format!(
            "Unable to split system instruction under limit {} bytes",
            limit
        ))
    } else {
        Ok(chunks)
    }
}

fn split_by_best_delimiter(text: &str, limit: usize, delimiters: &[&str]) -> Vec<String> {
    if let Some((delim, rest)) = delimiters.split_first() {
        let parts: Vec<&str> = text.split(delim).collect();
        if parts.len() == 1 {
            // This delimiter didn't help, try next
            return split_by_best_delimiter(text, limit, rest);
        }

        let mut result: Vec<String> = Vec::new();
        let mut current = String::new();

        for part in &parts {
            let candidate = if current.is_empty() {
                part.to_string()
            } else {
                format!("{}{}{}", &current, delim, part)
            };

            if candidate.len() <= limit {
                current = candidate;
            } else {
                if !current.is_empty() {
                    // Current chunk is full — push it
                    if current.len() > limit {
                        // Still too large, try finer delimiter
                        let sub = split_by_best_delimiter(&current, limit, rest);
                        result.extend(sub);
                        current = String::new();
                    } else {
                        result.push(current.clone());
                        current = String::new();
                    }
                }
                // Start new chunk with this part
                if part.len() <= limit {
                    current = part.to_string();
                } else {
                    // Single part too large, try finer delimiter
                    let sub = split_by_best_delimiter(part, limit, rest);
                    if sub.is_empty() {
                        // Can't split further — preserve as-is; caller will detect oversized chunk
                        result.push(part.to_string());
                        current = String::new();
                    } else {
                        let last = sub.len() - 1;
                        for (i, s) in sub.into_iter().enumerate() {
                            if i == last {
                                current = s;
                            } else {
                                result.push(s);
                            }
                        }
                    }
                }
            }
        }

        if !current.is_empty() {
            if current.len() > limit {
                let sub = split_by_best_delimiter(&current, limit, rest);
                result.extend(sub);
            } else {
                result.push(current);
            }
        }

        result
    } else {
        // No more delimiters — can't split further
        let bytes = text.as_bytes();
        if bytes.len() <= limit {
            vec![text.to_string()]
        } else {
            // Preserve oversized text — caller will detect and return error
            vec![text.to_string()]
        }
    }
}

/// Translate a single Interactions stream event (JSON data line) into
/// one or more Anthropic `StreamEvent` objects.
///
/// Deserializes into the generated `InteractionSseEvent` enum for type-safe
/// dispatch.  Construction of `StreamEvent` variants with complex inner types
/// (MessageStart, ContentBlockStart, ContentBlockDelta, MessageDelta, Error)
/// uses `serde_json::from_value(serde_json::json!({...}))` because the inner
/// types are not publicly exported by `anyllm_translate`.
///
/// Returns `None` for events that are intentionally skipped (status updates)
/// or malformed (logged via `tracing::info!`).
fn translate_stream_event(
    data: &str,
    _message_id: &str,
    model: &str,
) -> Option<Vec<anyllm_translate::anthropic::StreamEvent>> {
    use anyllm_translate::anthropic::StreamEvent;

    let event: InteractionSseEvent = match serde_json::from_str(data) {
        Ok(ev) => ev,
        Err(e) => {
            let preview: String = data.chars().take(200).collect();
            tracing::info!(%e, data_preview = %preview, "unrecognized interactions SSE event, dropping");
            return None;
        }
    };

    match event {
        InteractionSseEvent::InteractionCreatedEvent(ev) => {
            let msg_start: StreamEvent = serde_json::from_value(serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": ev.interaction.id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }))
            .ok()?;
            let block_start: StreamEvent = serde_json::from_value(serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }))
            .ok()?;
            Some(vec![msg_start, block_start])
        }
        InteractionSseEvent::StepStart(ev) => match &ev.step {
            Step::FunctionCallStep(fcs) => {
                let block_start: StreamEvent = serde_json::from_value(serde_json::json!({
                    "type": "content_block_start",
                    "index": ev.index,
                    "content_block": {
                        "type": "tool_use",
                        "id": fcs.id,
                        "name": fcs.name,
                        "input": {}
                    }
                }))
                .ok()?;
                Some(vec![block_start])
            }
            _ => {
                let block_start: StreamEvent = serde_json::from_value(serde_json::json!({
                    "type": "content_block_start",
                    "index": ev.index,
                    "content_block": {"type": "text", "text": ""}
                }))
                .ok()?;
                Some(vec![block_start])
            }
        },
        InteractionSseEvent::StepDelta(ev) => match ev.delta {
            StepDeltaData::TextDelta(td) => {
                let delta: StreamEvent = serde_json::from_value(serde_json::json!({
                    "type": "content_block_delta",
                    "index": ev.index,
                    "delta": {"type": "text_delta", "text": td.text}
                }))
                .ok()?;
                Some(vec![delta])
            }
            StepDeltaData::ThoughtSignatureDelta(tsd) => {
                let signature = tsd.signature.unwrap_or_default();
                let delta: StreamEvent = serde_json::from_value(serde_json::json!({
                    "type": "content_block_delta",
                    "index": ev.index,
                    "delta": {"type": "signature_delta", "signature": signature}
                }))
                .ok()?;
                Some(vec![delta])
            }
            StepDeltaData::ArgumentsDelta(ad) => {
                let partial_json = ad.arguments.unwrap_or_default();
                let delta: StreamEvent = serde_json::from_value(serde_json::json!({
                    "type": "content_block_delta",
                    "index": ev.index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": partial_json
                    }
                }))
                .ok()?;
                Some(vec![delta])
            }
            other => {
                tracing::warn!(
                    delta_type = ?serde_json::to_string(&other).unwrap_or_default(),
                    "unhandled step.delta type, dropping"
                );
                None
            }
        },
        InteractionSseEvent::StepStop(ev) => Some(vec![StreamEvent::ContentBlockStop {
            index: ev.index as u32,
        }]),
        // Status updates have no client-visible effect; safe to skip silently.
        InteractionSseEvent::InteractionStatusUpdate(_) => None,
        InteractionSseEvent::InteractionCompletedEvent(ev) => {
            let input_tokens = ev
                .interaction
                .usage
                .as_ref()
                .and_then(|u| u.total_input_tokens)
                .unwrap_or(0);
            let output_tokens = ev
                .interaction
                .usage
                .as_ref()
                .and_then(|u| u.total_output_tokens)
                .unwrap_or(0);
            let msg_delta: StreamEvent = serde_json::from_value(serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
            }))
            .ok()?;
            Some(vec![
                StreamEvent::ContentBlockStop { index: 0 },
                msg_delta,
                StreamEvent::MessageStop {},
            ])
        }
        InteractionSseEvent::ErrorEvent(ev) => {
            let msg = ev
                .error
                .as_ref()
                .and_then(|e| e.message.as_deref())
                .unwrap_or("unknown error");
            let code = ev
                .error
                .as_ref()
                .and_then(|e| e.code.as_deref())
                .unwrap_or("api_error");
            let err: StreamEvent = serde_json::from_value(serde_json::json!({
                "type": "error",
                "error": {"type": code, "message": msg}
            }))
            .ok()?;
            Some(vec![err])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactions::{single_element_too_large, split_content_for_limit};
    use crate::interactions_types::{Content, TextContent};

    // --- split_text_for_limit tests ---

    #[test]
    fn split_text_under_limit_single_chunk() {
        let result = split_text_for_limit("Hello world", 1024).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "Hello world");
    }

    #[test]
    fn split_text_exact_limit_single_chunk() {
        let text = "Hello!";
        let limit = text.len();
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], text);
    }

    #[test]
    fn split_text_empty() {
        let result = split_text_for_limit("", 100).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "");
    }

    #[test]
    fn split_text_at_double_newline() {
        let text = "First paragraph\n\nSecond paragraph";
        // "First paragraph" is 15 bytes, "Second paragraph" is 17 bytes
        // Total is 15 + 2 + 17 = 34 bytes
        let limit = 20;
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "First paragraph");
        assert_eq!(result[1], "Second paragraph");
    }

    #[test]
    fn split_text_at_single_newline() {
        let text = "Line one\nLine two\nLine three";
        let limit = 14;
        let result = split_text_for_limit(text, limit).unwrap();
        // "Line one" = 8, "Line one\nLine two" = 8+1+8=17 > 14 → split
        // "Line two" = 8, "Line two\nLine three" = 8+1+10=19 > 14 → split
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "Line one");
        assert_eq!(result[1], "Line two");
        assert_eq!(result[2], "Line three");
    }

    #[test]
    fn split_text_at_dot_space() {
        let text = "First sentence. Second sentence. Third one here";
        let limit = 25;
        let result = split_text_for_limit(text, limit).unwrap();
        // "First sentence" = 14, "First sentence. Second sentence" = 14+2+16=32 > 25
        assert!(!result.is_empty());
        assert_eq!(result[0], "First sentence");
        assert_eq!(result[1], "Second sentence");
    }

    #[test]
    fn split_text_greedy_fill() {
        // Each part fits individually, but two together exceed the limit.
        // Chunks should be as large as possible under the limit.
        let text = "AA\nBB\nCC";
        let limit = 5; // "AA\nBB" = 5 bytes exactly, "AA\nBB\nCC" = 8 > 5
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "AA\nBB");
        assert_eq!(result[1], "CC");
    }

    #[test]
    fn all_chunks_under_limit() {
        let text = "Chunk one\nChunk two\nChunk three\nChunk four";
        let limit = 15;
        let result = split_text_for_limit(text, limit).unwrap();
        for chunk in &result {
            assert!(
                chunk.len() <= limit,
                "chunk '{}' len {} exceeds limit {}",
                chunk,
                chunk.len(),
                limit
            );
        }
    }

    #[test]
    fn split_text_unsplittable_error() {
        // A single word with no delimiters, longer than the limit
        let text = "SuperCaliFragilisticExpialidociousNoSpacesOrPunctuation";
        let limit = 10;
        let result = split_text_for_limit(text, limit);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Unable to split system instruction"));
    }

    #[test]
    fn split_text_multi_level_delimiters() {
        // Text has no \n\n but has \n — should fall back to single newline
        let text = "AAA\nBBB\nCCC";
        let limit = 6; // "AAA\nBBB" = 7 > 6 → split, "BBB\nCCC" = 7 > 6 → split
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "AAA");
        assert_eq!(result[1], "BBB");
        assert_eq!(result[2], "CCC");
    }

    #[test]
    fn split_text_comma_delimiter() {
        let text = "item one, item two, item three";
        let limit = 15;
        let result = split_text_for_limit(text, limit).unwrap();
        // "item one" = 8, "item one, item two" = 8+2+9=19 > 15 → split
        assert_eq!(result[0], "item one");
    }

    #[test]
    fn split_text_semicolon_delimiter() {
        let text = "alpha; beta; gamma";
        let limit = 10;
        let result = split_text_for_limit(text, limit).unwrap();
        // "alpha" = 5, "alpha; beta" = 5+2+4=11 > 10 → split
        assert_eq!(result[0], "alpha");
    }

    #[test]
    fn split_text_exclamation_delimiter() {
        let text = "Wow! Great! Amazing";
        let limit = 10;
        let result = split_text_for_limit(text, limit).unwrap();
        // "Wow" = 3, "Wow! Great" = 3+2+5=10 ≤ 10 → fits
        // "Wow! Great! Amazing" = 10+2+7=19 > 10 → split
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "Wow! Great");
        assert_eq!(result[1], "Amazing");
    }

    #[test]
    fn split_text_question_delimiter() {
        let text = "What? When? Where?";
        let limit = 12;
        let result = split_text_for_limit(text, limit).unwrap();
        // "What" = 4, "What? When" = 4+2+4=10 ≤ 12 → fits
        // "What? When? Where" = 10+2+5=17 > 12 → split
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "What? When");
    }

    #[test]
    fn split_text_multiple_paragraphs_large_limit() {
        // Realistic system instruction with multiple paragraphs
        let text = "\
You are a helpful assistant.

You should always be polite and concise.

If you don't know the answer, say so honestly.";
        let limit = 80;
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 2);
        for chunk in &result {
            assert!(chunk.len() <= limit);
        }
    }

    // --- split_content_for_limit tests ---

    #[test]
    fn split_content_greedy_packing() {
        let make = |text: &str, size: usize| {
            // Pad text to approximate target serialized size
            let padding = "x".repeat(size.saturating_sub(text.len()));
            Content::TextContent(TextContent {
                text: format!("{}{}", text, padding),
                ..Default::default()
            })
        };
        // Each content serializes to roughly 100 bytes
        let c1 = make("msg1", 80);
        let c2 = make("msg2", 80);
        let c3 = make("msg3", 80);
        let contents = vec![c1, c2, c3];
        let chunks = split_content_for_limit(&contents, 200);
        // With limit 200, first chunk fits ~2 elements
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );
        // First chunk: should have at least 1 element
        assert!(!chunks[0].is_empty());
    }

    #[test]
    fn split_content_preserves_order() {
        let make = |i: usize| {
            Content::TextContent(TextContent {
                text: format!("message_{}", i),
                ..Default::default()
            })
        };
        let contents: Vec<Content> = (0..5).map(make).collect();
        let chunks = split_content_for_limit(&contents, 1024);
        // All fit in one chunk
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 5);
        // Order preserved
        for (i, chunk) in chunks.iter().flat_map(|c| c.iter()).enumerate() {
            match chunk {
                Content::TextContent(tc) => {
                    assert!(tc.text.contains(&format!("message_{}", i)));
                }
                _ => panic!("unexpected content type"),
            }
        }
    }

    // --- single_element_too_large tests ---

    #[test]
    fn single_element_under_limit() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        let size = serde_json::to_vec(&c).unwrap().len();
        assert!(!single_element_too_large(&[c], size + 100));
    }

    #[test]
    fn single_element_above_limit() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        let size = serde_json::to_vec(&c).unwrap().len();
        assert!(single_element_too_large(&[c], size.saturating_sub(1)));
    }

    // --- can_split_under_limit tests (RED) ---

    use crate::interactions_types::{CreateModelInteractionParams, InteractionsInput};

    fn minimal_params(contents: Vec<Content>) -> CreateModelInteractionParams {
        CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(contents),
            stream: Some(false),
            ..Default::default()
        }
    }

    #[test]
    fn can_split_overhead_alone_exceeds_limit() {
        // Envelope (model + generation_config + stream, no input, no sys) alone > limit
        let params = CreateModelInteractionParams {
            model: "test".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            ..Default::default()
        };
        let envelope_size = serde_json::to_vec(&params).unwrap().len();
        // Use a limit just below the envelope size
        let result =
            crate::interactions::can_split_under_limit(&params, envelope_size.saturating_sub(1));
        assert!(result.is_err());
    }

    #[test]
    fn can_split_single_content_too_large() {
        let c = Content::TextContent(TextContent {
            text: "x".repeat(1024),
            ..Default::default()
        });
        let params = minimal_params(vec![c]);
        let result = crate::interactions::can_split_under_limit(&params, 100);
        assert!(result.is_err());
    }

    #[test]
    fn can_split_system_instruction_unsplittable_word() {
        // system_instruction with one giant word → unsplittable
        let giant_word = "A".repeat(5000);
        let params = CreateModelInteractionParams {
            system_instruction: Some(giant_word),
            ..minimal_params(vec![])
        };
        let result = crate::interactions::can_split_under_limit(&params, 200);
        assert!(result.is_err());
    }

    #[test]
    fn can_split_all_splittable_ok() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            ..Default::default()
        });
        let params = CreateModelInteractionParams {
            system_instruction: Some(
                "You are a helpful assistant. Each sentence can be split.".into(),
            ),
            ..minimal_params(vec![c])
        };
        let result = crate::interactions::can_split_under_limit(&params, 100);
        assert!(result.is_ok());
    }

    /// Verify that when tools from a real Claude Code dump exceed the limit,
    /// the error message contains a per-tool size breakdown.
    #[test]
    fn can_split_reports_per_tool_breakdown_from_dump() {
        let tools_json: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/data/tools_from_dump.json")).unwrap();

        let anthropic_body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "messages": [{"role": "user", "content": "ping"}],
            "tools": tools_json,
        });

        let (tools, _tool_choice) =
            crate::interactions::extract_anthropic_tools(&anthropic_body);
        let tools = tools.expect("tools should be extracted from dump");

        let params = CreateModelInteractionParams {
            model: "deepseek-v4-pro".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            tools: Some(tools),
            ..Default::default()
        };

        let limit = 100 * 1024; // 100 KiB
        let result = crate::interactions::can_split_under_limit(&params, limit);

        let err = result.expect_err("envelope with 105 tools must exceed 100 KiB limit");
        assert!(
            err.contains("Non-splittable request fields"),
            "error should mention non-splittable fields: {err}"
        );
        assert!(
            err.contains("Per-tool size breakdown:"),
            "error should contain per-tool breakdown: {err}"
        );
        // Spot-check a few known tools
        assert!(err.contains("Agent"), "should list Agent tool");
        assert!(err.contains("Bash"), "should list Bash tool");
        assert!(err.contains("Read"), "should list Read tool");
        // Verify human-readable sizes are present
        assert!(err.contains("KiB"), "should use KiB for large tools");
        assert!(err.contains("description:"), "should show description size");
        assert!(err.contains("parameters:"), "should show parameters size");
    }

    // --- split_text_for_limit: more edge cases ---

    #[test]
    fn split_text_single_paragraph_fits() {
        let text = "You are a helpful and concise assistant.";
        let limit = text.len();
        let result = split_text_for_limit(text, limit).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn split_text_narrow_limit_forces_word_splitting() {
        let text = "Hello world";
        let limit = 3; // Even individual words are > 3 bytes
        let result = split_text_for_limit(text, limit);
        // "Hello" is 5 bytes > 3, and "Hello" has no delimiters → unsplittable
        assert!(result.is_err());
    }

    #[test]
    fn split_text_last_delimiter_space() {
        // Verify space delimiter works as last resort
        let text = "one two three four five";
        let limit = 10;
        let result = split_text_for_limit(text, limit).unwrap();
        // Each chunk should be ≤ limit
        for chunk in &result {
            assert!(chunk.len() <= limit);
        }
        // "one two" = 7, "one two three" = 13 > 10 → split
        // "three four" = 11 > 10 → split
        // "three" = 5, "four five" = 9 → probably 3 chunks
        assert!(result.len() >= 3);
    }

    // --- Streaming SSE event translation tests ---

    /// A step.delta event with text content.
    const STREAM_STEP_DELTA_TEXT: &str =
        r#"{"event_type":"step.delta","delta":{"type":"text","text":"Hello"},"index":1}"#;

    /// A step.delta event with thought_signature content.
    const STREAM_STEP_DELTA_SIGNATURE: &str = r#"{"event_type":"step.delta","delta":{"type":"thought_signature","signature":"EjQKMgEMOdbHDmR4/UnibJL5"},"index":0}"#;

    /// An interaction.created event.
    const STREAM_INTERACTION_CREATED: &str = r#"{"event_type":"interaction.created","interaction":{"id":"int-stream-1","status":"in_progress","created":"2026-01-01T00:00:00Z","updated":"2026-01-01T00:00:00Z","steps":[]}}"#;

    /// An interaction.completed event with usage.
    const STREAM_INTERACTION_COMPLETED: &str = r#"{"event_type":"interaction.completed","interaction":{"id":"int-stream-1","status":"completed","created":"2026-01-01T00:00:00Z","updated":"2026-01-01T00:00:01Z","steps":[],"usage":{"total_input_tokens":10,"total_output_tokens":20}}}"#;

    /// A step.start event for a model_output step.
    const STREAM_STEP_START_MODEL_OUTPUT: &str =
        r#"{"event_type":"step.start","index":1,"step":{"type":"model_output"}}"#;

    /// A step.start event for a thought step.
    const STREAM_STEP_START_THOUGHT: &str =
        r#"{"event_type":"step.start","index":0,"step":{"type":"thought"}}"#;

    /// A step.stop event.
    const STREAM_STEP_STOP: &str = r#"{"event_type":"step.stop","index":1}"#;

    /// An error_event.
    const STREAM_ERROR: &str =
        r#"{"event_type":"error","error":{"code":"not_found","message":"Result not found."}}"#;

    #[test]
    fn translate_step_delta_text_produces_block_delta() {
        let events = translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "test-model").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_delta_signature_produces_block_delta() {
        let events =
            translate_stream_event(STREAM_STEP_DELTA_SIGNATURE, "msg-1", "test-model").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_interaction_created_produces_message_and_block_start() {
        let events =
            translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "test-model").unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn translate_interaction_completed_produces_stop_events() {
        let events =
            translate_stream_event(STREAM_INTERACTION_COMPLETED, "msg-1", "test-model").unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn translate_step_start_model_output_produces_text_block() {
        let events = translate_stream_event(STREAM_STEP_START_MODEL_OUTPUT, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_start_thought_produces_text_block() {
        let events = translate_stream_event(STREAM_STEP_START_THOUGHT, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_stop_produces_block_stop() {
        let events = translate_stream_event(STREAM_STEP_STOP, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_error_event_produces_error() {
        let events = translate_stream_event(STREAM_ERROR, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_returns_none_for_unknown_event_type() {
        // "interaction.status_update" is a real event type but we skip it
        let status_update = r#"{"event_type":"interaction.status_update","interaction_id":"abc","status":"in_progress"}"#;
        let events = translate_stream_event(status_update, "msg-1", "test-model");
        assert!(events.is_none());
    }

    #[test]
    fn translate_returns_none_for_malformed_json() {
        let events = translate_stream_event("not valid json", "msg-1", "test-model");
        assert!(events.is_none());
    }

    #[test]
    fn translate_multiple_deltas_accumulate() {
        let events1 = translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "m").unwrap();
        assert!(!events1.is_empty());
        let events2 = translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "m").unwrap();
        assert!(!events2.is_empty());
        let delta2 =
            r#"{"event_type":"step.delta","delta":{"type":"text","text":" World"},"index":1}"#;
        let events3 = translate_stream_event(delta2, "msg-1", "m").unwrap();
        assert!(!events3.is_empty());
        let events4 = translate_stream_event(STREAM_INTERACTION_COMPLETED, "msg-1", "m").unwrap();
        assert_eq!(events4.len(), 3);
    }

    #[test]
    fn translate_full_dump_sequence() {
        // Simulate the full event sequence from the dump
        let created =
            translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "gemini").unwrap();
        assert_eq!(created.len(), 2); // message_start + content_block_start

        let thought_start =
            translate_stream_event(STREAM_STEP_START_THOUGHT, "msg-1", "gemini").unwrap();
        assert_eq!(thought_start.len(), 1); // content_block_start (thinking)

        let sig_delta =
            translate_stream_event(STREAM_STEP_DELTA_SIGNATURE, "msg-1", "gemini").unwrap();
        assert_eq!(sig_delta.len(), 1); // content_block_delta (signature)

        let thought_stop = translate_stream_event(STREAM_STEP_STOP, "msg-1", "gemini").unwrap();
        assert_eq!(thought_stop.len(), 1); // content_block_stop

        let text_start =
            translate_stream_event(STREAM_STEP_START_MODEL_OUTPUT, "msg-1", "gemini").unwrap();
        assert_eq!(text_start.len(), 1); // content_block_start (text)

        let text_delta = translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "gemini").unwrap();
        assert_eq!(text_delta.len(), 1); // content_block_delta (text)

        let text_stop = translate_stream_event(STREAM_STEP_STOP, "msg-1", "gemini").unwrap();
        assert_eq!(text_stop.len(), 1); // content_block_stop

        let completed =
            translate_stream_event(STREAM_INTERACTION_COMPLETED, "msg-1", "gemini").unwrap();
        assert_eq!(completed.len(), 3); // content_block_stop + message_delta + message_stop

        // Total events: 2 + 1 + 1 + 1 + 1 + 1 + 1 + 3 = 11
        let total = created.len()
            + thought_start.len()
            + sig_delta.len()
            + thought_stop.len()
            + text_start.len()
            + text_delta.len()
            + text_stop.len()
            + completed.len();
        assert_eq!(total, 11);
    }

    #[test]
    fn build_interaction_url_strips_query_params() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some(
                "https://host/v1beta/interactions?model=gemini-2.0-flash&alt=sse".into(),
            ),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(result, "https://host/v1beta/interactions/cancel");
    }

    #[test]
    fn build_interaction_url_strips_trailing_slash() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions/".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(result, "https://host/v1beta/interactions/cancel");
    }

    #[test]
    fn build_interaction_url_strips_bare_qmark() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions?model".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(result, "https://host/v1beta/interactions/cancel");
    }

    #[test]
    fn split_text_rejects_oversized_contiguous_segment() {
        // Mixed content: splittable parts + one unsplittable word
        // Without fix, the unsplittable word is silently dropped and Ok is returned
        let text = "hello world SUPERCALIFRAGILISTICEXPEALIDOCIOUS foo bar";
        let result = split_text_for_limit(text, 10);
        assert!(
            result.is_err(),
            "unsplittable contiguous segment must cause an error"
        );
    }

    // ── resolve_session_id ────────────────────────────────────

    use crate::config::Config;
    use crate::diagnostics::{Diagnostics, DiagnosticsConfig};
    use axum::http::{HeaderMap, HeaderValue};

    fn test_interactions_handler() -> InteractionsHandler {
        let config = Config::load_from_str(
            r#"
            listen_host = "127.0.0.1"
            listen_port = 3000
            upstream_timeout = "30s"
            max_request_body = "1m"

            [test]
            endpoint_interactions = "https://test.example.com/v1beta/interactions"
            models = "test-model"
            "#,
        )
        .expect("test config");
        let diagnostics = Diagnostics::new(DiagnosticsConfig::default());
        let session_store = Arc::new(SessionStore::new(
            std::env::temp_dir().join("test-resolve-session-id.toml"),
        ));
        InteractionsHandler::new(&config, diagnostics, session_store).expect("build handler")
    }

    #[tokio::test]
    async fn resolve_session_id_prefers_x_request_id_over_x_claude_code_session_id() {
        let handler = test_interactions_handler();
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("req-id-123"));
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("session-456"),
        );
        let body = serde_json::json!({});
        let result = handler.resolve_session_id(&headers, &body);
        assert_eq!(result, "req-id-123");
    }

    #[tokio::test]
    async fn resolve_session_id_uses_x_claude_code_session_id_when_x_request_id_absent() {
        let handler = test_interactions_handler();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("session-456"),
        );
        let body = serde_json::json!({});
        let result = handler.resolve_session_id(&headers, &body);
        assert_eq!(result, "session-456");
    }

    #[tokio::test]
    async fn resolve_session_id_falls_back_to_body_request_id_when_no_headers() {
        let handler = test_interactions_handler();
        let headers = HeaderMap::new();
        let body = serde_json::json!({"request_id": "body-id-789"});
        let result = handler.resolve_session_id(&headers, &body);
        assert_eq!(result, "body-id-789");
    }

    #[tokio::test]
    async fn resolve_session_id_uses_random_uuid_as_last_resort() {
        let handler = test_interactions_handler();
        let headers = HeaderMap::new();
        let body = serde_json::json!({});
        let result = handler.resolve_session_id(&headers, &body);
        assert!(!result.is_empty());
        // Should be a valid UUID
        assert!(uuid::Uuid::try_parse(&result).is_ok());
    }

    // --- build_interactions_headers tests ---
    use reqwest::Client;

    fn request_headers_with_auth() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Bearer client-sk-ant-key"),
        );
        headers.insert("x-api-key", HeaderValue::from_static("client-api-key"));
        headers.insert("x-request-id", HeaderValue::from_static("trace-12345"));
        headers.insert("x-custom-trace", HeaderValue::from_static("custom-value"));
        headers
    }

    #[test]
    fn build_interactions_headers_sets_x_goog_api_key() {
        let client = Client::new();
        let builder = client.get("http://example.com");
        let result = build_interactions_headers(builder, Some("gemini-key-123"), &HeaderMap::new());
        let req = result.build().expect("build request");
        assert_eq!(
            req.headers().get("x-goog-api-key").unwrap(),
            "gemini-key-123"
        );
    }

    #[test]
    fn build_interactions_headers_strips_client_auth_when_key_set() {
        let client = Client::new();
        let builder = client.get("http://example.com");
        let result = build_interactions_headers(
            builder,
            Some("gemini-key-123"),
            &request_headers_with_auth(),
        );
        let req = result.build().expect("build request");
        assert!(
            req.headers().get("Authorization").is_none(),
            "client Authorization must be stripped when api_key is set"
        );
        assert!(
            req.headers().get("x-api-key").is_none(),
            "client x-api-key must be stripped when api_key is set"
        );
        assert_eq!(
            req.headers().get("x-goog-api-key").unwrap(),
            "gemini-key-123",
            "x-goog-api-key must be set from api_key"
        );
    }

    #[test]
    fn build_interactions_headers_forwards_client_auth_when_no_key() {
        let client = Client::new();
        let builder = client.get("http://example.com");
        let result = build_interactions_headers(builder, None, &request_headers_with_auth());
        let req = result.build().expect("build request");
        assert_eq!(
            req.headers().get("Authorization").unwrap(),
            "Bearer client-sk-ant-key",
            "client Authorization must be forwarded when api_key is None"
        );
        assert!(
            req.headers().get("x-goog-api-key").is_none(),
            "x-goog-api-key must NOT be set when api_key is None"
        );
    }

    #[test]
    fn build_interactions_headers_forwards_non_auth_headers() {
        let client = Client::new();
        let builder = client.get("http://example.com");

        // With api_key
        let result = build_interactions_headers(
            builder,
            Some("gemini-key-123"),
            &request_headers_with_auth(),
        );
        let req = result.build().expect("build request");
        assert_eq!(
            req.headers().get("x-request-id").unwrap(),
            "trace-12345",
            "non-auth headers must be forwarded when api_key is set"
        );
        assert_eq!(
            req.headers().get("x-custom-trace").unwrap(),
            "custom-value",
            "custom headers must be forwarded when api_key is set"
        );

        // Without api_key
        let builder2 = client.get("http://example.com");
        let result2 = build_interactions_headers(builder2, None, &request_headers_with_auth());
        let req2 = result2.build().expect("build request");
        assert_eq!(
            req2.headers().get("x-request-id").unwrap(),
            "trace-12345",
            "non-auth headers must be forwarded when api_key is None"
        );
    }

    // ── build_interactions_headers_map ──────────────────────

    #[test]
    fn build_interactions_headers_map_with_api_key() {
        let map =
            build_interactions_headers_map(Some("gemini-key-123"), &request_headers_with_auth());
        // API key sets x-goog-api-key
        assert_eq!(
            map.get("x-goog-api-key").unwrap().to_str().unwrap(),
            "gemini-key-123"
        );
        // Client auth stripped
        assert!(map.get("authorization").is_none());
        assert!(map.get("x-api-key").is_none());
        // Non-auth forwarded
        assert_eq!(
            map.get("x-request-id").unwrap().to_str().unwrap(),
            "trace-12345"
        );
        assert_eq!(
            map.get("x-custom-trace").unwrap().to_str().unwrap(),
            "custom-value"
        );
        // Standard headers present
        assert_eq!(
            map.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
        assert!(map.get("Api-Revision").is_some());
    }

    #[test]
    fn build_interactions_headers_map_without_api_key() {
        let map = build_interactions_headers_map(None, &request_headers_with_auth());
        // Client auth forwarded
        assert_eq!(
            map.get("authorization").unwrap().to_str().unwrap(),
            "Bearer client-sk-ant-key"
        );
        assert_eq!(
            map.get("x-api-key").unwrap().to_str().unwrap(),
            "client-api-key"
        );
        // No x-goog-api-key
        assert!(map.get("x-goog-api-key").is_none());
        // Non-auth forwarded
        assert_eq!(
            map.get("x-request-id").unwrap().to_str().unwrap(),
            "trace-12345"
        );
    }

    // --- Streaming function_call event tests (RED — not yet implemented) ---

    /// A step.start event with a function_call step (no arguments — schema patched optional).
    const STREAM_STEP_START_FUNCTION_CALL: &str = r#"{"event_type":"step.start","index":2,"step":{"type":"function_call","id":"call-1","name":"get_weather"}}"#;

    /// A step.delta event with an arguments_delta (function_call arguments).
    const STREAM_STEP_DELTA_ARGUMENTS: &str = r#"{"event_type":"step.delta","index":2,"delta":{"type":"arguments_delta","arguments":"{\"location\":\"Boston\"}"}}"#;

    #[test]
    fn translate_step_start_function_call_produces_tool_use_block() {
        let events = translate_stream_event(STREAM_STEP_START_FUNCTION_CALL, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            anyllm_translate::anthropic::StreamEvent::ContentBlockStart {
                content_block, ..
            } => match content_block {
                anyllm_translate::anthropic::ContentBlock::ToolUse { id, name, input } => {
                    assert_eq!(id, "call-1");
                    assert_eq!(name, "get_weather");
                    assert_eq!(input, &serde_json::json!({}));
                }
                other => panic!("expected ToolUse block, got {:?}", other),
            },
            other => panic!("expected ContentBlockStart, got {:?}", other),
        }
    }

    #[test]
    fn translate_arguments_delta_produces_input_json_delta() {
        let events = translate_stream_event(STREAM_STEP_DELTA_ARGUMENTS, "msg-1", "m").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            anyllm_translate::anthropic::StreamEvent::ContentBlockDelta { delta, .. } => {
                match delta {
                    anyllm_translate::anthropic::Delta::InputJsonDelta { partial_json } => {
                        assert_eq!(partial_json, r#"{"location":"Boston"}"#);
                    }
                    other => panic!("expected InputJsonDelta, got {:?}", other),
                }
            }
            other => panic!("expected ContentBlockDelta, got {:?}", other),
        }
    }
}
