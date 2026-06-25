//! Gemini Interactions API handler.
//!
//! Handles Anthropic→Interactions and OpenAI→Interactions translation,
//! session state management, control messages, proxy_limit splitting,
//! and response translation back to the client's protocol.

#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
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
/// Maximum buffered SSE data before a \\n\\n delimiter. If an upstream
/// sends a malformed stream without delimiters, this cap prevents unbounded
/// memory growth.
const MAX_SSE_BUFFER_BYTES: usize = 1_048_576; // 1 MiB

/// Clamp a u64 max_tokens value to u32 range.
/// Values above u32::MAX are clamped with a warning — the Gemini API only
/// accepts i64 but practical values fit in u32.
fn clamp_max_tokens(n: u64) -> u32 {
    match u32::try_from(n) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(max_tokens = n, "max_tokens exceeds u32::MAX, clamping");
            u32::MAX
        }
    }
}

#[derive(Clone)]
pub struct InteractionsHandler {
    clients: HashMap<String, HttpClient>,
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
        Ok(Self {
            clients: crate::build_client_map(config)?,
            diagnostics,
            session_store,
            error_translation: config.error_translation.clone().into(),
        })
    }

    fn get_client(&self, proxy: Option<&str>) -> &HttpClient {
        proxy
            .and_then(|url| self.clients.get(url))
            .unwrap_or_else(|| self.clients.get("").expect("default client must exist"))
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
            .map(clamp_max_tokens);
        let system = interactions_lib::extract_anthropic_system(&body_val);
        let (tools, tool_choice) = interactions_lib::extract_anthropic_tools(&body_val);

        let prev_id = if start_index == 0 {
            // Context reset — client started fresh conversation.
            // Clear previous_interaction_id so the upstream creates a new interaction.
            None
        } else if session.interaction_id.is_empty() {
            None
        } else {
            Some(session.interaction_id.as_str())
        };

        // Zero messages on a new session: nothing to send and no
        // interaction to replay. Sending empty input upstream is invalid.
        if incoming_count == 0 && prev_id.is_none() {
            return Err(AppError::BadRequest(
                "empty messages on new session".to_string(),
            ));
        }

        // Exact retry — all messages already delivered. Replay existing interaction.
        if start_index == incoming_count {
            if let Some(pid) = prev_id {
                return self
                    .replay_interaction(
                        pid,
                        route,
                        &session_id,
                        &model,
                        Protocol::Anthropic,
                        request_headers,
                        guard,
                    )
                    .await;
            }
            // All messages delivered but no interaction_id to replay —
            // session state was lost or never persisted. Sending an empty
            // ContentList upstream is invalid.
            return Err(AppError::Internal(
                "session has no interaction_id for replay".to_string(),
            ));
        }

        // Build the request (Anthropic ingress)
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
        let mut body_value = serde_json::to_value(&params).map_err(|e| {
            guard.abort_internal(
                0,
                body.len(),
                endpoint,
                "anthropic->interactions",
                stream,
                e,
            )
        })?;
        let df = route.drop_fields.for_model(&model);
        crate::drop_fields_from_value(&mut body_value, &df);
        let request_body = serde_json::to_vec(&body_value).map_err(|e| {
            guard.abort_internal(
                0,
                body.len(),
                endpoint,
                "anthropic->interactions",
                stream,
                e,
            )
        })?;

        // Apply proxy_limit splitting if needed
        if let Some(limit) = route.proxy_limit {
            let contents = match &params.input {
                InteractionsInput::ContentList(list) => list.clone(),
                _ => vec![],
            };
            let size = request_body.len();
            if size > limit {
                if interactions_lib::can_split_under_limit(&params, limit).is_err() {
                    return Err(guard.abort_bad_request(
                        0,
                        body.len(),
                        "anthropic->interactions",
                        "anthropic->interactions",
                        stream,
                        "request cannot be split under proxy limit",
                    ));
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
            .get("max_completion_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| body_val.get("max_tokens").and_then(|v| v.as_u64()))
            .map(clamp_max_tokens);
        let (tools, tool_choice) = interactions_lib::extract_openai_tools(&body_val);

        let prev_id = if start_index == 0 {
            // Context reset — client started fresh conversation.
            None
        } else if session.interaction_id.is_empty() {
            None
        } else {
            Some(session.interaction_id.as_str())
        };

        // Zero messages on a new session: nothing to send and no
        // interaction to replay.
        if incoming_count == 0 && prev_id.is_none() {
            return Err(AppError::BadRequest(
                "empty messages on new session".to_string(),
            ));
        }

        // Exact retry — all messages already delivered. Replay existing interaction.
        if start_index == incoming_count {
            if let Some(pid) = prev_id {
                return self
                    .replay_interaction(
                        pid,
                        route,
                        &session_id,
                        &model,
                        Protocol::OpenAi,
                        request_headers,
                        guard,
                    )
                    .await;
            }
            return Err(AppError::Internal(
                "session has no interaction_id for replay".to_string(),
            ));
        }

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

        // Apply drop_fields BEFORE proxy_limit check so removed fields
        // don't inflate the size measurement and cause unnecessary splitting.
        let mut body_value = serde_json::to_value(&params).map_err(|e| {
            guard.abort_internal(0, body.len(), endpoint, "openai->interactions", stream, e)
        })?;
        let df = route.drop_fields.for_model(&model);
        crate::drop_fields_from_value(&mut body_value, &df);
        let request_body = serde_json::to_vec(&body_value).map_err(|e| {
            guard.abort_internal(0, body.len(), endpoint, "openai->interactions", stream, e)
        })?;

        if let Some(limit) = route.proxy_limit {
            let contents = match &params.input {
                InteractionsInput::ContentList(list) => list.clone(),
                _ => vec![],
            };
            if request_body.len() > limit {
                if interactions_lib::can_split_under_limit(&params, limit).is_err() {
                    return Err(guard.abort_bad_request(
                        0,
                        body.len(),
                        "openai->interactions",
                        "openai->interactions",
                        stream,
                        "request cannot be split under proxy limit",
                    ));
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

    /// Fetch and translate an existing interaction (exact retry recovery).
    async fn replay_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
        session_id: &str,
        model: &str,
        ingress: Protocol,
        request_headers: &HeaderMap,
        guard: crate::diagnostics::RequestDiagnostics,
    ) -> Result<Response, AppError> {
        let url = build_interaction_url(route, &format!("/{interaction_id}"));
        let start = std::time::Instant::now();

        // Record egress dump for replay request
        guard.egress_dump(b"{}", &HeaderMap::new());

        let builder = build_interactions_headers(
            self.get_client(route.proxy.as_deref()).get(&url),
            route.api_key.as_deref(),
            request_headers,
        );
        let upstream = match builder.send().await {
            Ok(r) => r,
            Err(e) => {
                let dur = start.elapsed().as_millis() as u64;
                return Err(guard.abort_upstream(dur, 0, &url, "replay", false, e));
            }
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream.text().await.unwrap_or_default();
            guard.finish_with_upstream_error(
                status.as_u16(),
                duration_ms,
                0,
                &url,
                "replay",
                false,
                error_body.clone(),
                vec![],
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

        let status_code = upstream.status().as_u16();
        let body_bytes = match upstream.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Err(guard.abort_upstream(duration_ms, 0, &url, "replay", false, e));
            }
        };
        let validated = match crate::validate_upstream_body(body_bytes, guard.request_id()) {
            Ok(v) => v,
            Err((e, dump)) => {
                guard.response_dump(dump, 502, true, vec![]);
                return Err(guard.abort_upstream(duration_ms, 0, &url, "replay", false, e));
            }
        };
        let interaction: Interaction = serde_json::from_str(&validated.text)
            .map_err(|e| guard.abort_internal(duration_ms, 0, &url, "replay", false, e))?;
        let response_json =
            interactions_lib::build_response_from_interaction(&interaction, model, ingress)
                .map_err(|e| guard.abort_internal(duration_ms, 0, &url, "replay", false, e))?;
        guard.finish(
            status_code,
            duration_ms,
            0,
            Some(validated.text.len()),
            &url,
            "replay",
            false,
        );
        Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "application/json")
            .header(Self::session_header_name(ingress), session_id)
            .body(Body::from(
                serde_json::to_vec(&response_json)
                    .map_err(|e| AppError::Internal(e.to_string()))?,
            ))
            .map_err(|err| AppError::Internal(err.to_string()))
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
            self.get_client(route.proxy.as_deref())
                .post(url)
                .header(header::CONTENT_TYPE, "application/json"),
            route.api_key.as_deref(),
            request_headers,
        );

        let start = std::time::Instant::now();
        let upstream = match builder.body(egress_body.to_vec()).send().await {
            Ok(r) => r,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                return Err(guard.abort_upstream(
                    duration_ms,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                ));
            }
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let response_headers = response_headers_to_pairs(upstream.headers());
            let error_body = upstream.text().await.unwrap_or_default();
            guard.finish_with_upstream_error(
                status.as_u16(),
                duration_ms,
                ingress_body.len(),
                upstream_label,
                direction,
                stream,
                error_body.clone(),
                response_headers,
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

        let response_headers = response_headers_to_pairs(upstream.headers());
        let response_body_bytes = match upstream.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Err(guard.abort_upstream(
                    duration_ms,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                ));
            }
        };
        let validated = match crate::validate_upstream_body(response_body_bytes, guard.request_id())
        {
            Ok(v) => v,
            Err((e, dump)) => {
                guard.response_dump(dump, 502, true, response_headers.clone());
                return Err(guard.abort_upstream(
                    duration_ms,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                ));
            }
        };
        guard.response_dump(validated.dump, 200, false, response_headers.clone());
        let response_body = validated.text;
        let interaction: Interaction = match serde_json::from_str(&response_body) {
            Ok(inter) => inter,
            Err(e) => {
                return Err(guard.abort_upstream(
                    duration_ms,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    format!("failed to parse interaction response: {e}"),
                ));
            }
        };

        // Update session
        let interaction_id = interaction.id.clone();
        self.session_store
            .update(session_id, interaction_id, new_count, false)
            .await
            .map_err(|e| {
                tracing::error!(
                    session_id = %session_id,
                    error = %e,
                    "session update failed after successful upstream interaction"
                );
                AppError::Internal(format!("session update failed: {e}"))
            })?;

        // Translate response back to ingress protocol
        let resp =
            match interactions_lib::build_response_from_interaction(&interaction, model, ingress) {
                Ok(r) => r,
                Err(e) => {
                    return Err(guard.abort_internal(
                        duration_ms,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    ));
                }
            };

        let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
        guard.ingress_response_dump(crate::diagnostics::dump_body_from_bytes(&resp_bytes), 200);

        guard.finish(
            200,
            duration_ms,
            ingress_body.len(),
            Some(response_body.len()),
            upstream_label,
            direction,
            stream,
        );

        Ok(Self::ok_with_session_header_and_upstream_headers(
            ingress,
            session_id,
            resp,
            &response_headers,
        ))
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
        let response_headers = response_headers_to_pairs(upstream.headers());
        let mut byte_stream = upstream.bytes_stream();
        let mut buffer = String::new();
        let mut interaction_id = String::new();
        let mut total_bytes: usize = 0;
        let mut dump_buffer: Vec<u8> = Vec::new();
        let mut last_active_index: Option<u32> = None;

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

        // Mark session pending before starting stream — if the process crashes
        // mid-stream, startup recovery will see pending=true and verify the
        // interaction status. The message_count is advanced eagerly to prevent
        // racing follow-up requests from re-sending in-flight messages.
        let _ = session_store
            .update(&sid, String::new(), new_count, true)
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
                            let _ = guard.abort_upstream(
                                duration_ms,
                                request_size,
                                &label,
                                &dir,
                                true,
                                "non-utf8 streaming response from upstream",
                            );
                            return;
                        }
                        buffer.push_str(std::str::from_utf8(&chunk).unwrap());

                        // Guard against unbounded buffer growth from a
                        // malformed upstream stream lacking \n\n delimiters.
                        if buffer.len() > MAX_SSE_BUFFER_BYTES {
                            let err_payload = bytes::Bytes::from(sse::format_sse_event_str(
                                &anyllm_translate::anthropic::StreamEvent::Error {
                                    error: anyllm_translate::anthropic::streaming::StreamError {
                                        error_type: "upstream_error".into(),
                                        message: "sse buffer exceeded max line length".into(),
                                    },
                                },
                            ));
                            let _ = tx.send(Ok(err_payload)).await;
                            let duration_ms = start.elapsed().as_millis() as u64;
                            let _ = guard.abort_upstream(
                                duration_ms,
                                request_size,
                                &label,
                                &dir,
                                true,
                                "sse buffer exceeded max line length",
                            );
                            return;
                        }

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
                                if let Some(events) = translate_stream_event(
                                    data,
                                    &model_owned,
                                    &model_owned,
                                    &mut last_active_index,
                                ) {
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
                                                        // Client disconnected before upstream
                                                        // stream completed — no egress/response
                                                        // dump is recorded because the upstream
                                                        // response was never fully received.
                                                        if interaction_id.is_empty() {
                                                            let _ = session_store
                                                                .update(
                                                                    &sid,
                                                                    String::new(),
                                                                    new_count,
                                                                    false,
                                                                )
                                                                .await;
                                                        }
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
                                                // Client disconnected before upstream
                                                // stream completed — no egress/response
                                                // dump is recorded because the upstream
                                                // response was never fully received.
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
                                    // Eagerly persist interaction_id from interaction.created
                                    // so client-disconnect recovery has a valid ID to probe.
                                    if let Ok(InteractionSseEvent::InteractionCreatedEvent(ev)) =
                                        serde_json::from_str::<InteractionSseEvent>(data)
                                    {
                                        interaction_id = ev.interaction.id.clone();
                                        let _ = session_store
                                            .update(&sid, ev.interaction.id, new_count, true)
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Send protocol error event before closing the stream.
                        // Without this, the client receives an abrupt body close
                        // after HTTP 200, which violates SSE protocol expectations.
                        let err_payload = bytes::Bytes::from(sse::format_sse_event_str(
                            &anyllm_translate::anthropic::StreamEvent::Error {
                                error: anyllm_translate::anthropic::streaming::StreamError {
                                    error_type: "upstream_error".into(),
                                    message: format!("stream read error: {e}"),
                                },
                            },
                        ));
                        let _ = tx.send(Ok(err_payload)).await;
                        let _ = tx
                            .send(Err(std::io::Error::other(format!("stream error: {e}"))))
                            .await;
                        let duration_ms = start.elapsed().as_millis() as u64;
                        let _ = guard.abort_upstream(
                            duration_ms,
                            request_size,
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
                if let Err(e) = session_store
                    .update(&sid, interaction_id, new_count, false)
                    .await
                {
                    tracing::error!(
                        session_id = %sid,
                        error = %e,
                        "session update failed after successful upstream stream"
                    );
                }
            }

            // Response dump for streaming
            let dump_body = crate::sse::parse_sse_buffer_to_json_array(&dump_buffer);
            if dump_body.is_base64() {
                tracing::warn!(
                    request_id = %request_id,
                    direction = "response",
                    body_len = dump_buffer.len(),
                    "non-utf8 streaming interactions upstream response"
                );
            }
            guard.response_dump_streaming(dump_body, 200, response_headers);

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
        stream: bool,
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

        let mut last_id: Option<String> = None;
        let mut last_interaction: Option<Interaction> = None;

        let system_instruction = params.system_instruction.clone();

        // Build first-chunk envelope (with all first-interaction fields) for size checks
        let first_envelope = CreateModelInteractionParams {
            model: model.to_string(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            system_instruction: system_instruction.clone(),
            tools: params.tools.clone(),
            generation_config: params.generation_config.clone(),
            ..Default::default()
        };
        let mut subsequent_envelope = CreateModelInteractionParams {
            model: model.to_string(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            // Include a representative previous_interaction_id so that
            // chunk size estimation accounts for the field that
            // build_chunk_request actually serializes.  Without this the
            // measured size can be smaller than the real serialized body,
            // causing chunks to silently exceed proxy_limit.
            previous_interaction_id: Some("x".repeat(36)),
            ..Default::default()
        };

        // Use the new full-chunk packer
        let chunks = match interactions_lib::pack_content_into_chunks(
            &first_envelope,
            &subsequent_envelope,
            contents,
            limit,
        ) {
            Ok(chunks) => chunks,
            Err(msg) => {
                return Err(guard.abort_bad_request(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    msg,
                ));
            }
        };

        // If system_instruction + envelope > limit, split system_instruction first
        if let Some(ref sys) = system_instruction {
            let empty_size = serde_json::to_vec(&first_envelope)
                .map(|v| v.len())
                .unwrap_or(0);
            if empty_size > limit {
                // Compute envelope overhead (everything except system_instruction text)
                // so split_text_for_limit produces chunks that fit when wrapped.
                let envelope_without_sys = {
                    let mut env = first_envelope.clone();
                    env.system_instruction = Some(String::new());
                    serde_json::to_vec(&env).map(|v| v.len()).unwrap_or(0)
                };
                let sys_limit = limit.saturating_sub(envelope_without_sys);
                return self
                    .send_split_system_instruction(
                        sys,
                        url,
                        route,
                        session_id,
                        &chunks,
                        total_message_count,
                        sys_limit,
                        model,
                        upstream_label,
                        ingress_body,
                        direction,
                        request_headers,
                        ingress,
                        guard,
                        params.tools.clone(),
                        params.generation_config.clone(),
                        stream,
                    )
                    .await;
            }
        }

        // Send each chunk sequentially
        let mut current_prev = params.previous_interaction_id.clone();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_first_chunk = i == 0 && current_prev.is_none();
            let si = if is_first_chunk {
                system_instruction.clone()
            } else {
                None
            };
            let mut chunk_req = interactions_lib::build_chunk_request(
                model,
                chunk.clone(),
                si,
                current_prev.clone(),
            );
            if is_first_chunk {
                chunk_req.tools = params.tools.clone();
                chunk_req.generation_config = params.generation_config.clone();
            }
            let mut chunk_body_value = serde_json::to_value(&chunk_req).map_err(|e| {
                guard.abort_internal(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            let df = route.drop_fields.for_model(model);
            crate::drop_fields_from_value(&mut chunk_body_value, &df);
            let chunk_body = serde_json::to_vec(&chunk_body_value).map_err(|e| {
                guard.abort_internal(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;

            guard.egress_dump(&chunk_body, &egress_headers);

            let builder = build_interactions_headers(
                self.get_client(route.proxy.as_deref())
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
                request_headers,
            );
            let upstream = builder.body(chunk_body).send().await.map_err(|e| {
                guard.abort_upstream(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            if !upstream.status().is_success() {
                let status = upstream.status();
                let response_headers = response_headers_to_pairs(upstream.headers());
                let error_body = upstream.text().await.unwrap_or_default();
                guard.finish_with_upstream_error(
                    status.as_u16(),
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    error_body.clone(),
                    response_headers,
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
            let response_headers = response_headers_to_pairs(upstream.headers());

            // Eagerly update session progress before reading the body.
            // If body read/validation/deserialization fails after a
            // successful HTTP 200, the upstream already created the
            // interaction — retry must not re-send the same content.
            let delivered_items: usize = chunks[..=i].iter().map(|c| c.len()).sum();
            let delivered_so_far = std::cmp::min(delivered_items, total_message_count);
            let interim_id = current_prev.clone().unwrap_or_default();
            if let Err(e) = self
                .session_store
                .update(session_id, interim_id, delivered_so_far, true)
                .await
            {
                tracing::error!(
                    session_id = %session_id,
                    error = %e,
                    "session update failed after successful split-send chunk (eager)"
                );
            }

            let response_bytes = upstream.bytes().await.map_err(|e| {
                guard.abort_upstream(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            let validated = match crate::validate_upstream_body(response_bytes, guard.request_id())
            {
                Ok(v) => v,
                Err((e, dump)) => {
                    guard.response_dump(dump, 502, true, response_headers.clone());
                    return Err(guard.abort_upstream(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    ));
                }
            };
            guard.response_dump(validated.dump, 200, false, response_headers.clone());
            let response_text = validated.text;
            let interaction: Interaction = serde_json::from_str(&response_text).map_err(|e| {
                guard.abort_upstream(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            total_response_bytes += response_text.len();
            let interaction_id = interaction.id.clone();
            current_prev = Some(interaction_id.clone());
            last_id = Some(interaction_id.clone());
            last_interaction = Some(interaction);

            // After first chunk, rebuild subsequent_envelope with the real
            // previous_interaction_id so subsequent chunk size estimation
            // uses the actual ID length, not a hardcoded 36-char placeholder.
            if i == 0 {
                subsequent_envelope.previous_interaction_id = Some(interaction_id.clone());
                // Verify remaining pre-packed chunks still fit with real ID
                for (j, chunk) in chunks.iter().enumerate().skip(1) {
                    let body = interactions_lib::build_pack_body(&subsequent_envelope, chunk);
                    let size = serde_json::to_vec(&body).map(|v| v.len()).unwrap_or(0);
                    if size > limit {
                        return Err(guard.abort_bad_request(
                            start.elapsed().as_millis() as u64,
                            ingress_body.len(),
                            upstream_label,
                            direction,
                            stream,
                            format!(
                                "chunk {} exceeds proxy_limit with real previous_interaction_id ({} > {})",
                                j, size, limit
                            ),
                        ));
                    }
                }
            }

            // Update session after each successful chunk so retries don't
            // re-send already-accepted content upstream.
            // Track delivered Content items by index (upper bound) to
            // prevent underestimation from proportional rounding.
            let delivered_items: usize = chunks[..=i].iter().map(|c| c.len()).sum();
            let delivered_so_far = std::cmp::min(delivered_items, total_message_count);
            if let Err(e) = self
                .session_store
                .update(session_id, interaction_id, delivered_so_far, true)
                .await
            {
                tracing::error!(
                    session_id = %session_id,
                    error = %e,
                    "session update failed after successful split-send chunk"
                );
            }
        }

        if stream {
            if let Some(ref inter) = last_interaction {
                let resp = interactions_lib::build_response_from_interaction(inter, model, ingress)
                    .map_err(|e| {
                        guard.abort_internal(
                            start.elapsed().as_millis() as u64,
                            ingress_body.len(),
                            upstream_label,
                            direction,
                            true,
                            e,
                        )
                    })?;
                // Finalize session after successful translation so retries
                // can recover if translation fails.
                if let Some(ref final_id) = last_id {
                    self.session_store
                        .update(session_id, final_id.clone(), total_message_count, false)
                        .await
                        .map_err(|e| {
                            tracing::error!(
                                session_id = %session_id,
                                error = %e,
                                "session update failed after successful split-send stream"
                            );
                            AppError::Internal(format!("session update failed: {e}"))
                        })?;
                }
                let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
                guard.ingress_response_dump(
                    crate::diagnostics::dump_body_from_bytes(&resp_bytes),
                    200,
                );
                guard.finish(
                    200,
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    Some(total_response_bytes),
                    upstream_label,
                    direction,
                    true,
                );
                return Self::streaming_response_from_interaction(
                    ingress, session_id, model, inter,
                );
            }
        }

        let resp = if let Some(ref inter) = last_interaction {
            interactions_lib::build_response_from_interaction(inter, model, ingress).map_err(
                |e| {
                    guard.abort_internal(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        false,
                        e,
                    )
                },
            )?
        } else {
            build_fallback_response(last_interaction.as_ref(), last_id.clone(), model, ingress)
                .map_err(|e| {
                    guard.abort_internal(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        false,
                        e,
                    )
                })?
        };
        // Finalize session after successful response translation.
        if let Some(ref final_id) = last_id {
            self.session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await
                .map_err(|e| {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "session update failed after successful split-send"
                    );
                    AppError::Internal(format!("session update failed: {e}"))
                })?;
        }
        // Dump the final ingress response before finishing
        let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
        guard.ingress_response_dump(crate::diagnostics::dump_body_from_bytes(&resp_bytes), 200);
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
        tools: Option<Vec<crate::interactions_types::Tool>>,
        generation_config: Option<crate::interactions_types::GenerationConfig>,
        stream: bool,
    ) -> Result<Response, AppError> {
        let start = std::time::Instant::now();
        let mut total_response_bytes: usize = 0;
        let egress_headers =
            build_interactions_headers_map(route.api_key.as_deref(), request_headers);
        // Split system_instruction on natural boundaries
        let sys_parts = match split_text_for_limit(sys, limit) {
            Ok(parts) => parts,
            Err(msg) => {
                return Err(guard.abort_bad_request(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    msg,
                ));
            }
        };
        let mut last_id: Option<String> = None;
        let mut last_interaction: Option<Interaction> = None;
        let mut current_prev: Option<String> = None;

        // Send empty interactions with system_instruction chunks
        for (i, part) in sys_parts.iter().enumerate() {
            let is_first_chunk = i == 0;
            let is_last_sys = i == sys_parts.len() - 1;
            let input_for_chunk = if is_last_sys && !chunks.is_empty() {
                chunks[0].clone()
            } else {
                vec![]
            };
            let mut chunk_req = interactions_lib::build_chunk_request(
                model,
                input_for_chunk,
                Some(part.clone()),
                current_prev.clone(),
            );
            if is_first_chunk {
                chunk_req.tools = tools.clone();
                chunk_req.generation_config = generation_config.clone();
            }
            let mut chunk_body_value = serde_json::to_value(&chunk_req).map_err(|e| {
                guard.abort_internal(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            let df = route.drop_fields.for_model(model);
            crate::drop_fields_from_value(&mut chunk_body_value, &df);
            let chunk_body = serde_json::to_vec(&chunk_body_value).map_err(|e| {
                guard.abort_internal(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;

            guard.egress_dump(&chunk_body, &egress_headers);

            let builder = build_interactions_headers(
                self.get_client(route.proxy.as_deref())
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
                request_headers,
            );
            let upstream = builder.body(chunk_body).send().await.map_err(|e| {
                guard.abort_upstream(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            let upstream_status = upstream.status();
            let response_headers = response_headers_to_pairs(upstream.headers());
            if !upstream_status.is_success() {
                let error_body = upstream.text().await.unwrap_or_default();
                guard.finish_with_upstream_error(
                    upstream_status.as_u16(),
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    false,
                    error_body.clone(),
                    response_headers,
                );
                let sc = StatusCode::from_u16(upstream_status.as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let body = crate::translate_interactions_error_to_protocol(&error_body, ingress);
                let body = crate::apply_error_translation(sc, body, &self.error_translation);
                return Response::builder()
                    .status(sc)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(Self::session_header_name(ingress), session_id)
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()));
            }
            let response_bytes = upstream.bytes().await.map_err(|e| {
                guard.abort_upstream(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            total_response_bytes += response_bytes.len();
            let validated = match crate::validate_upstream_body(response_bytes, guard.request_id())
            {
                Ok(v) => v,
                Err((e, dump)) => {
                    guard.response_dump(dump, 502, true, response_headers.clone());
                    return Err(guard.abort_upstream(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    ));
                }
            };
            guard.response_dump(validated.dump, 200, false, response_headers.clone());
            let response_text = validated.text;
            let interaction: Interaction = serde_json::from_str(&response_text).map_err(|e| {
                guard.abort_internal(
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    upstream_label,
                    direction,
                    stream,
                    e,
                )
            })?;
            let int_id = interaction.id.clone();
            current_prev = Some(int_id.clone());
            last_id = Some(int_id.clone());
            last_interaction = Some(interaction);

            // Checkpoint session after each system-instruction chunk
            // so retries don't re-send already-created interaction chain.
            if let Err(e) = self
                .session_store
                .update(session_id, int_id, total_message_count, true)
                .await
            {
                tracing::error!(
                    session_id = %session_id,
                    error = %e,
                    "session update failed after system-instruction chunk"
                );
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
                let chunk_body = serde_json::to_vec(&chunk_req).map_err(|e| {
                    guard.abort_internal(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    )
                })?;

                guard.egress_dump(&chunk_body, &egress_headers);

                let builder = build_interactions_headers(
                    self.get_client(route.proxy.as_deref())
                        .post(url)
                        .header(header::CONTENT_TYPE, "application/json"),
                    route.api_key.as_deref(),
                    request_headers,
                );
                let upstream = builder.body(chunk_body).send().await.map_err(|e| {
                    guard.abort_upstream(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    )
                })?;
                let upstream_status = upstream.status();
                let response_headers = response_headers_to_pairs(upstream.headers());
                if !upstream_status.is_success() {
                    let error_body = upstream.text().await.unwrap_or_default();
                    guard.finish_with_upstream_error(
                        upstream_status.as_u16(),
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        false,
                        error_body.clone(),
                        response_headers,
                    );
                    let sc = StatusCode::from_u16(upstream_status.as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let body =
                        crate::translate_interactions_error_to_protocol(&error_body, ingress);
                    let body = crate::apply_error_translation(sc, body, &self.error_translation);
                    return Response::builder()
                        .status(sc)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(Self::session_header_name(ingress), session_id)
                        .body(Body::from(body))
                        .map_err(|err| AppError::Internal(err.to_string()));
                }
                let response_bytes = upstream.bytes().await.map_err(|e| {
                    guard.abort_upstream(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        stream,
                        e,
                    )
                })?;
                total_response_bytes += response_bytes.len();
                let validated =
                    match crate::validate_upstream_body(response_bytes, guard.request_id()) {
                        Ok(v) => v,
                        Err((e, dump)) => {
                            guard.response_dump(dump, 502, true, response_headers.clone());
                            return Err(guard.abort_upstream(
                                start.elapsed().as_millis() as u64,
                                ingress_body.len(),
                                upstream_label,
                                direction,
                                stream,
                                e,
                            ));
                        }
                    };
                guard.response_dump(validated.dump, 200, false, response_headers.clone());
                let response_text = validated.text;
                let interaction: Interaction =
                    serde_json::from_str(&response_text).map_err(|e| {
                        guard.abort_internal(
                            start.elapsed().as_millis() as u64,
                            ingress_body.len(),
                            upstream_label,
                            direction,
                            stream,
                            e,
                        )
                    })?;
                current_prev = Some(interaction.id.clone());
                last_id = Some(interaction.id.clone());
                last_interaction = Some(interaction);
            }
        }

        if let Some(ref final_id) = last_id {
            self.session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await
                .map_err(|e| {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "session update failed after successful split-system-instruction"
                    );
                    AppError::Internal(format!("session update failed: {e}"))
                })?;
        }

        if stream {
            if let Some(ref inter) = last_interaction {
                let resp = interactions_lib::build_response_from_interaction(inter, model, ingress)
                    .map_err(|e| {
                        guard.abort_internal(
                            start.elapsed().as_millis() as u64,
                            ingress_body.len(),
                            upstream_label,
                            direction,
                            true,
                            e,
                        )
                    })?;
                let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
                guard.ingress_response_dump(
                    crate::diagnostics::dump_body_from_bytes(&resp_bytes),
                    200,
                );
                guard.finish(
                    200,
                    start.elapsed().as_millis() as u64,
                    ingress_body.len(),
                    Some(total_response_bytes),
                    upstream_label,
                    direction,
                    true,
                );
                return Self::streaming_response_from_interaction(
                    ingress, session_id, model, inter,
                );
            }
        }

        // Translate last response to ingress protocol
        let resp = if let Some(ref inter) = last_interaction {
            interactions_lib::build_response_from_interaction(inter, model, ingress).map_err(
                |e| {
                    guard.abort_internal(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        false,
                        e,
                    )
                },
            )?
        } else {
            build_fallback_response(last_interaction.as_ref(), last_id.clone(), model, ingress)
                .map_err(|e| {
                    guard.abort_internal(
                        start.elapsed().as_millis() as u64,
                        ingress_body.len(),
                        upstream_label,
                        direction,
                        false,
                        e,
                    )
                })?
        };
        let resp_bytes = serde_json::to_vec(&resp).unwrap_or_default();
        guard.ingress_response_dump(crate::diagnostics::dump_body_from_bytes(&resp_bytes), 200);
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

    /// Cancel an interaction upstream.
    async fn cancel_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<(), AppError> {
        let url = build_interaction_url(route, &format!("/{interaction_id}/cancel"));
        let builder = build_interactions_headers(
            self.get_client(route.proxy.as_deref()).post(&url),
            route.api_key.as_deref(),
            &HeaderMap::new(),
        );
        match builder.send().await {
            Ok(resp) if resp.status().is_success() => Ok(()),
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    interaction_id = %interaction_id,
                    status = %status,
                    body = %body,
                    "cancel interaction: upstream returned non-success"
                );
                Err(AppError::Internal(format!(
                    "cancel interaction {interaction_id}: HTTP {status}"
                )))
            }
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
            self.get_client(route.proxy.as_deref()).delete(&url),
            route.api_key.as_deref(),
            &HeaderMap::new(),
        );
        match builder.send().await {
            Ok(resp) if resp.status().is_success() => Ok(()),
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    interaction_id = %interaction_id,
                    status = %status,
                    body = %body,
                    "delete interaction: upstream returned non-success"
                );
                Err(AppError::Internal(format!(
                    "delete interaction {interaction_id}: HTTP {status}"
                )))
            }
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
                let all = match self.session_store.remove_all().await {
                    Ok(all) => all,
                    Err(e) => {
                        let error = format!("session clean-all failed: {e}");
                        return Err(guard.abort_internal(
                            0,
                            0,
                            "control-action",
                            "clean-all",
                            false,
                            error,
                        ));
                    }
                };
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
                guard.finish(200, 0, 0, None, "control-action", "clean-all", false);
                Ok(Self::ok_with_session_header(
                    ingress,
                    session_id,
                    serde_json::json!({"status": "ok", "message": msg}),
                ))
            }
            ControlAction::ExtendLifetime(until) => {
                match self.session_store.extend_lifetime(session_id, *until).await {
                    Ok(()) => (),
                    Err(e) => {
                        let error = format!("session extend failed: {e}");
                        return Err(guard.abort_internal(
                            0,
                            0,
                            "control-action",
                            "extend-lifetime",
                            false,
                            error,
                        ));
                    }
                };
                let msg = format!("Session {} lifetime extended to UTC {}", session_id, until);
                guard.finish(200, 0, 0, None, "control-action", "extend-lifetime", false);
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
        // Empty interaction_id would produce GET /v1beta/interactions/
        // (the list endpoint), which returns 200 and would cause recovery
        // to treat the interaction as found — keeping an orphaned session.
        if interaction_id.is_empty() {
            return Ok(false);
        }
        let url = build_interaction_url(route, &format!("/{interaction_id}"));
        let builder = build_interactions_headers(
            self.get_client(route.proxy.as_deref()).get(&url),
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
        // Priority: X-Client-Request-Id > x-claude-code-session-id > x-request-id > request_id body field > random
        if let Some(hdr) = headers
            .get("X-Client-Request-Id")
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
        if let Some(hdr) = headers
            .get("x-request-id")
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
        Self::ok_with_session_header_and_upstream_headers(ingress, session_id, json, &[])
    }

    fn ok_with_session_header_and_upstream_headers(
        ingress: Protocol,
        session_id: &str,
        json: serde_json::Value,
        upstream_headers: &[(String, String)],
    ) -> Response {
        let hdr_name = Self::session_header_name(ingress);
        let hdr_value = HeaderValue::from_str(session_id).unwrap_or(HeaderValue::from_static(""));
        let mut response = (StatusCode::OK, axum::Json(json)).into_response();
        let headers = response.headers_mut();
        headers.insert(HeaderName::from_static(hdr_name), hdr_value);
        for (name, value) in upstream_headers {
            if is_interactions_response_header_whitelisted(name) {
                if let Ok(v) = HeaderValue::from_str(value) {
                    if let Ok(n) = HeaderName::from_bytes(name.as_bytes()) {
                        headers.insert(n, v);
                    }
                }
            }
        }
        response
    }

    /// Build a streaming SSE response from the split-send final interaction.
    fn streaming_response_from_interaction(
        ingress: Protocol,
        session_id: &str,
        model: &str,
        interaction: &Interaction,
    ) -> Result<Response, AppError> {
        let resp = interactions_lib::build_response_from_interaction(interaction, model, ingress)
            .map_err(AppError::Internal)?;

        match ingress {
            Protocol::Anthropic => {
                let events = synthesize_anthropic_events(model, &resp);
                let body = sse::format_sse_events(&events);
                let hdr_name = Self::session_header_name(ingress);
                let hdr_value =
                    HeaderValue::from_str(session_id).unwrap_or(HeaderValue::from_static(""));
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(HeaderName::from_static(hdr_name), hdr_value)
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()))
            }
            Protocol::OpenAi => {
                let chunks = synthesize_openai_chunks(model, &resp);
                let body = chunks.join("");
                let hdr_name = Self::session_header_name(ingress);
                let hdr_value =
                    HeaderValue::from_str(session_id).unwrap_or(HeaderValue::from_static(""));
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(HeaderName::from_static(hdr_name), hdr_value)
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()))
            }
        }
    }
}

/// Synthesize Anthropic SSE events from a translated interaction response.
fn synthesize_anthropic_events(
    model: &str,
    resp: &serde_json::Value,
) -> Vec<anyllm_translate::anthropic::StreamEvent> {
    use anyllm_translate::anthropic::StreamEvent;

    let msg_id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("msg_1");
    let content = resp.get("content").and_then(|v| v.as_array());
    let stop_reason = resp
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    let input_tokens = resp
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = resp
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut events: Vec<StreamEvent> = Vec::new();

    events.push(stream_event_message_start(
        msg_id,
        model,
        input_tokens,
        output_tokens,
    ));

    if let Some(blocks) = content {
        for (idx, block) in blocks.iter().enumerate() {
            let idx_u32 = idx as u32;
            let block_type = block.get("type").and_then(|v| v.as_str());

            match block_type {
                Some("text") => {
                    let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    events.push(stream_event_content_block_start_text(idx_u32));
                    events.push(stream_event_content_block_delta_text(idx_u32, text));
                }
                Some("tool_use") => {
                    let tool_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let tool_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let tool_input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                    let partial_json = serde_json::to_string(&tool_input).unwrap_or_default();

                    events.push(stream_event_content_block_start_tool_use(
                        idx_u32, tool_id, tool_name,
                    ));
                    events.push(stream_event_content_block_delta_json(
                        idx_u32,
                        &partial_json,
                    ));
                }
                _ => {}
            }

            events.push(StreamEvent::ContentBlockStop { index: idx_u32 });
        }
    }

    events.push(stream_event_message_delta(
        stop_reason,
        input_tokens,
        output_tokens,
    ));
    events.push(StreamEvent::MessageStop {});

    events
}

// ── OpenAI SSE chunk constructors ────────────────────────────────────
//
// These helpers build OpenAI-compatible ChatCompletionChunk SSE lines.
// They use serde_json::json!({...}) because ChatCompletionChunk's inner
// types are not publicly exported by anyllm_translate. The functions
// return pre-formatted "data: {...}\n\n" strings ready for the SSE stream.

fn openai_sse_chunk(
    msg_id: &str,
    model: &str,
    index: u32,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> String {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let chunk = serde_json::json!({
        "id": msg_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": index,
            "delta": delta,
            "finish_reason": finish_reason
        }]
    });
    format!(
        "data: {}\n\n",
        serde_json::to_string(&chunk).unwrap_or_default()
    )
}

fn openai_sse_role_chunk(msg_id: &str, model: &str, index: u32) -> String {
    openai_sse_chunk(
        msg_id,
        model,
        index,
        serde_json::json!({"role": "assistant"}),
        None,
    )
}

fn openai_sse_content_chunk(msg_id: &str, model: &str, index: u32, content: &str) -> String {
    openai_sse_chunk(
        msg_id,
        model,
        index,
        serde_json::json!({"content": content}),
        None,
    )
}

fn openai_sse_tool_calls_chunk(
    msg_id: &str,
    model: &str,
    index: u32,
    tool_calls: &[serde_json::Value],
) -> String {
    openai_sse_chunk(
        msg_id,
        model,
        index,
        serde_json::json!({"tool_calls": tool_calls}),
        None,
    )
}

fn openai_sse_finish_chunk(msg_id: &str, model: &str, index: u32, finish_reason: &str) -> String {
    openai_sse_chunk(
        msg_id,
        model,
        index,
        serde_json::json!({}),
        Some(finish_reason),
    )
}

/// Synthesize OpenAI SSE chunks from a translated interaction response.
fn synthesize_openai_chunks(model: &str, resp: &serde_json::Value) -> Vec<String> {
    let msg_id = resp
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("chatcmpl-1");
    let choices = resp.get("choices").and_then(|v| v.as_array());

    let mut chunks = Vec::new();

    if let Some(choices) = choices {
        for choice in choices {
            let index = choice.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let finish_reason = choice
                .get("finish_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("stop");
            let delta_content = choice
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tool_calls = choice
                .get("message")
                .and_then(|m| m.get("tool_calls"))
                .and_then(|v| v.as_array());

            chunks.push(openai_sse_role_chunk(msg_id, model, index));

            if let Some(tc_arr) = tool_calls {
                if !tc_arr.is_empty() {
                    chunks.push(openai_sse_tool_calls_chunk(msg_id, model, index, tc_arr));
                }
            } else if !delta_content.is_empty() {
                chunks.push(openai_sse_content_chunk(
                    msg_id,
                    model,
                    index,
                    delta_content,
                ));
            }

            chunks.push(openai_sse_finish_chunk(msg_id, model, index, finish_reason));
        }
    }

    chunks.push("data: [DONE]\n\n".to_string());

    chunks
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
                    prompt_tokens: interactions_lib::clamp_i64_to_u32(
                        input_tokens,
                        "total_input_tokens",
                    ),
                    completion_tokens: interactions_lib::clamp_i64_to_u32(
                        output_tokens,
                        "total_output_tokens",
                    ),
                    total_tokens: interactions_lib::clamp_i64_to_u32(
                        input_tokens + output_tokens,
                        "total_tokens",
                    ),
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
                    input_tokens: interactions_lib::clamp_i64_to_u32(
                        input_tokens,
                        "total_input_tokens",
                    ),
                    output_tokens: interactions_lib::clamp_i64_to_u32(
                        output_tokens,
                        "total_output_tokens",
                    ),
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
fn response_headers_to_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(n, _)| is_interactions_response_header_whitelisted(n.as_str()))
        .map(|(n, v)| {
            (
                n.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn build_interactions_headers_map(api_key: Option<&str>, request_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );

    for (name, value) in request_headers.iter() {
        let name_str = name.as_str();
        if !should_forward_request_header(name_str) {
            continue;
        }
        if api_key.is_some() && is_auth_header(name_str) {
            continue;
        }
        // x-request-id is generated by the upstream, not the client.
        // Do not forward it — Gemini's stateful protocol uses
        // previous_interaction_id in the request body for continuity.
        if name_str.eq_ignore_ascii_case("x-request-id") {
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }

    // Insert fixed Api-Revision AFTER client header forwarding so client
    // cannot override it. Gemini requires a specific revision string.
    headers.insert(
        HeaderName::from_bytes(b"Api-Revision").unwrap(),
        HeaderValue::from_static(API_REVISION),
    );

    if let Some(key) = api_key {
        let _ = headers.insert(
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(key).unwrap_or(HeaderValue::from_static("")),
        );
    }

    // Forward x-claude-code-session-id as X-Client-Request-Id for
    // OpenAI upstream request correlation.
    if headers.contains_key("x-claude-code-session-id")
        && !headers.contains_key("x-client-request-id")
    {
        if let Some(val) = headers.get("x-claude-code-session-id").cloned() {
            headers.insert(HeaderName::from_static("x-client-request-id"), val);
        }
    }

    headers
}

/// Whitelist of upstream response headers to forward through interactions success responses.
fn is_interactions_response_header_whitelisted(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "x-request-id"
        || name == "x-claude-code-session-id"
        || name.starts_with("x-ratelimit-")
        || name == "request-id"
}
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
    let (base, query) = match base.find('?') {
        Some(pos) => (&base[..pos], Some(&base[pos + 1..])),
        None => (base, None),
    };
    let base = base.trim_end_matches('/');
    match query {
        Some(q) if !q.is_empty() => format!("{base}{suffix}?{q}"),
        _ => format!("{base}{suffix}"),
    }
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

// ── StreamEvent constructors ─────────────────────────────────────────
//
// The inner types of these StreamEvent variants (MessageStartMessage,
// ContentBlockStartContent, Delta, MessageDeltaData, StreamError) are not
// publicly exported by anyllm_translate. Constructing them requires serde
// deserialization from a JSON value. These helpers isolate that pattern.
//
// All constructors use .expect("...") because the JSON shapes are
// statically correct — a panic here indicates a serde schema mismatch
// that must fail fast.

fn stream_event_message_start(
    id: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "message_start",
        "message": {
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
        }
    }))
    .expect("message_start serde schema mismatch")
}

fn stream_event_content_block_start_text(index: u32) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {"type": "text", "text": ""}
    }))
    .expect("content_block_start text serde schema mismatch")
}

fn stream_event_content_block_start_tool_use(
    index: u32,
    tool_id: &str,
    tool_name: &str,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {
            "type": "tool_use",
            "id": tool_id,
            "name": tool_name,
            "input": {}
        }
    }))
    .expect("content_block_start tool_use serde schema mismatch")
}

fn stream_event_content_block_delta_text(
    index: u32,
    text: &str,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "text_delta", "text": text}
    }))
    .expect("content_block_delta text_delta serde schema mismatch")
}

fn stream_event_content_block_delta_signature(
    index: u32,
    signature: &str,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "signature_delta", "signature": signature}
    }))
    .expect("content_block_delta signature_delta serde schema mismatch")
}

fn stream_event_content_block_delta_json(
    index: u32,
    partial_json: &str,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "input_json_delta", "partial_json": partial_json}
    }))
    .expect("content_block_delta input_json_delta serde schema mismatch")
}

fn stream_event_message_delta(
    stop_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "message_delta",
        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
    }))
    .expect("message_delta serde schema mismatch")
}

fn stream_event_error(code: &str, message: &str) -> anyllm_translate::anthropic::StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "error",
        "error": {"type": code, "message": message}
    }))
    .expect("error event serde schema mismatch")
}

// ── SSE event translation ────────────────────────────────────────────

/// Translate a single Interactions stream event (JSON data line) into
/// one or more Anthropic `StreamEvent` objects.
///
/// Deserializes into the generated `InteractionSseEvent` enum for type-safe
/// dispatch.
///
/// `last_active_index` tracks the most recent step.start index so that
/// `InteractionCompletedEvent` can emit the correct `ContentBlockStop`.
///
/// Returns `None` for events that are intentionally skipped (status updates)
/// or malformed (logged via `tracing::info!`).
fn translate_stream_event(
    data: &str,
    _message_id: &str,
    model: &str,
    last_active_index: &mut Option<u32>,
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
            let msg_start = stream_event_message_start(&ev.interaction.id, model, 0, 0);
            // Do not emit content_block_start here — step.start events provide
            // the actual content block structure with correct indices.
            Some(vec![msg_start])
        }
        InteractionSseEvent::StepStart(ev) => {
            *last_active_index = Some(ev.index as u32);
            match &ev.step {
                Step::FunctionCallStep(fcs) => {
                    let block_start = stream_event_content_block_start_tool_use(
                        ev.index as u32,
                        &fcs.id,
                        fcs.name.as_str(),
                    );
                    Some(vec![block_start])
                }
                _ => {
                    let block_start = stream_event_content_block_start_text(ev.index as u32);
                    Some(vec![block_start])
                }
            }
        }
        InteractionSseEvent::StepDelta(ev) => match ev.delta {
            StepDeltaData::TextDelta(td) => {
                let delta = stream_event_content_block_delta_text(ev.index as u32, &td.text);
                Some(vec![delta])
            }
            StepDeltaData::ThoughtSignatureDelta(tsd) => {
                let signature = tsd.signature.unwrap_or_default();
                let delta = stream_event_content_block_delta_signature(ev.index as u32, &signature);
                Some(vec![delta])
            }
            StepDeltaData::ArgumentsDelta(ad) => {
                let partial_json = ad.arguments.unwrap_or_default();
                let delta = stream_event_content_block_delta_json(ev.index as u32, &partial_json);
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
        InteractionSseEvent::StepStop(ev) => {
            let index = ev.index as u32;
            // Mark block as already stopped so InteractionCompletedEvent
            // doesn't emit a duplicate ContentBlockStop.
            *last_active_index = None;
            Some(vec![StreamEvent::ContentBlockStop { index }])
        }
        // Status updates have no client-visible effect; safe to skip silently.
        InteractionSseEvent::InteractionStatusUpdate(_) => None,
        InteractionSseEvent::InteractionCompletedEvent(ev) => {
            let input_tokens = ev
                .interaction
                .usage
                .as_ref()
                .and_then(|u| u.total_input_tokens)
                .unwrap_or(0) as u64;
            let output_tokens = ev
                .interaction
                .usage
                .as_ref()
                .and_then(|u| u.total_output_tokens)
                .unwrap_or(0) as u64;
            let msg_delta = stream_event_message_delta("end_turn", input_tokens, output_tokens);
            // Only emit ContentBlockStop if the last step didn't already stop it.
            // StepStop clears last_active_index, so Some means no StepStop preceded.
            let mut events = Vec::with_capacity(3);
            if let Some(index) = *last_active_index {
                events.push(StreamEvent::ContentBlockStop { index });
            }
            *last_active_index = None;
            events.push(msg_delta);
            events.push(StreamEvent::MessageStop {});
            Some(events)
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
            let err = stream_event_error(code, msg);
            Some(vec![err])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactions::{single_element_too_large, split_content_for_limit};
    use crate::interactions_types::{Content, TextContent};

    // --- clamp_max_tokens tests ---

    #[test]
    fn clamp_max_tokens_within_range() {
        assert_eq!(clamp_max_tokens(0), 0);
        assert_eq!(clamp_max_tokens(4096), 4096);
        assert_eq!(clamp_max_tokens(u32::MAX as u64), u32::MAX);
    }

    #[test]
    fn clamp_max_tokens_above_u32_max_clamps() {
        // 5_000_000_000 > u32::MAX (4_294_967_295) — should clamp, not wrap to 705_032_704
        assert_eq!(clamp_max_tokens(5_000_000_000), u32::MAX);
        assert_eq!(clamp_max_tokens(u64::MAX), u32::MAX);
    }

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

        let (tools, _tool_choice) = crate::interactions::extract_anthropic_tools(&anthropic_body);
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
            err.contains("Per-tool size breakdown (sorted by size):"),
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

    #[test]
    fn split_send_first_chunk_gets_tools_and_gen_config() {
        use crate::interactions_types::{Content, Function, GenerationConfig, TextContent, Tool};

        let tool = Tool::Function(Function {
            name: Some("get_weather".into()),
            description: Some("Get weather".into()),
            parameters: Some(serde_json::json!({"type": "object"})),
            ..Default::default()
        });
        let gen_config = GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(200),
            ..Default::default()
        };
        let params = CreateModelInteractionParams {
            model: "test-model".into(),
            input: InteractionsInput::ContentList(vec![]),
            stream: Some(false),
            tools: Some(vec![tool.clone()]),
            generation_config: Some(gen_config.clone()),
            ..Default::default()
        };

        // Simulate the chunk-loop logic from handle_split_send
        let contents = vec![
            Content::TextContent(TextContent {
                text: "msg-1".into(),
                ..Default::default()
            }),
            Content::TextContent(TextContent {
                text: "msg-2".into(),
                ..Default::default()
            }),
        ];
        let chunks = interactions_lib::split_content_for_limit(&contents, 1024);
        assert_eq!(chunks.len(), 1, "both messages fit in one chunk");

        let current_prev: Option<String> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let is_first_chunk = i == 0 && current_prev.is_none();
            let mut chunk_req = interactions_lib::build_chunk_request(
                "test-model",
                chunk.clone(),
                None,
                current_prev.clone(),
            );
            if is_first_chunk {
                chunk_req.tools = params.tools.clone();
                chunk_req.generation_config = params.generation_config.clone();
            }

            if i == 0 {
                assert!(chunk_req.tools.is_some(), "first chunk must have tools");
                assert!(
                    chunk_req.generation_config.is_some(),
                    "first chunk must have generation_config"
                );
            } else {
                assert!(
                    chunk_req.tools.is_none(),
                    "non-first chunk must not have tools"
                );
                assert!(
                    chunk_req.generation_config.is_none(),
                    "non-first chunk must not have generation_config"
                );
            }
        }
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
        let events =
            translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "test-model", &mut None)
                .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_delta_signature_produces_block_delta() {
        let events = translate_stream_event(
            STREAM_STEP_DELTA_SIGNATURE,
            "msg-1",
            "test-model",
            &mut None,
        )
        .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_interaction_created_produces_message_start_only() {
        let events =
            translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "test-model", &mut None)
                .unwrap();
        // Only message_start — content_block_start is deferred to step.start events
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_interaction_completed_produces_stop_events() {
        let events = translate_stream_event(
            STREAM_INTERACTION_COMPLETED,
            "msg-1",
            "test-model",
            &mut None,
        )
        .unwrap();
        assert_eq!(events.len(), 2); // message_delta + message_stop (no ContentBlockStop — StepStop already stopped it)
    }

    #[test]
    fn translate_step_start_model_output_produces_text_block() {
        let events =
            translate_stream_event(STREAM_STEP_START_MODEL_OUTPUT, "msg-1", "m", &mut None)
                .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_start_thought_produces_text_block() {
        let events =
            translate_stream_event(STREAM_STEP_START_THOUGHT, "msg-1", "m", &mut None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_step_stop_produces_block_stop() {
        let events = translate_stream_event(STREAM_STEP_STOP, "msg-1", "m", &mut None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_error_event_produces_error() {
        let events = translate_stream_event(STREAM_ERROR, "msg-1", "m", &mut None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn translate_returns_none_for_unknown_event_type() {
        // "interaction.status_update" is a real event type but we skip it
        let status_update = r#"{"event_type":"interaction.status_update","interaction_id":"abc","status":"in_progress"}"#;
        let events = translate_stream_event(status_update, "msg-1", "test-model", &mut None);
        assert!(events.is_none());
    }

    #[test]
    fn translate_returns_none_for_malformed_json() {
        let events = translate_stream_event("not valid json", "msg-1", "test-model", &mut None);
        assert!(events.is_none());
    }

    #[test]
    fn translate_multiple_deltas_accumulate() {
        let events1 =
            translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "m", &mut None).unwrap();
        assert!(!events1.is_empty());
        let events2 =
            translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "m", &mut None).unwrap();
        assert!(!events2.is_empty());
        let delta2 =
            r#"{"event_type":"step.delta","delta":{"type":"text","text":" World"},"index":1}"#;
        let events3 = translate_stream_event(delta2, "msg-1", "m", &mut None).unwrap();
        assert!(!events3.is_empty());
        let events4 =
            translate_stream_event(STREAM_INTERACTION_COMPLETED, "msg-1", "m", &mut None).unwrap();
        assert_eq!(events4.len(), 2); // message_delta + message_stop (ContentBlockStop already emitted by StepStop)
    }

    #[test]
    fn translate_full_dump_sequence() {
        // Simulate the full event sequence from the dump
        let created =
            translate_stream_event(STREAM_INTERACTION_CREATED, "msg-1", "gemini", &mut None)
                .unwrap();
        assert_eq!(created.len(), 1); // message_start only (no content_block_start)

        let thought_start =
            translate_stream_event(STREAM_STEP_START_THOUGHT, "msg-1", "gemini", &mut None)
                .unwrap();
        assert_eq!(thought_start.len(), 1); // content_block_start (thinking)

        let sig_delta =
            translate_stream_event(STREAM_STEP_DELTA_SIGNATURE, "msg-1", "gemini", &mut None)
                .unwrap();
        assert_eq!(sig_delta.len(), 1); // content_block_delta (signature)

        let thought_stop =
            translate_stream_event(STREAM_STEP_STOP, "msg-1", "gemini", &mut None).unwrap();
        assert_eq!(thought_stop.len(), 1); // content_block_stop

        let text_start =
            translate_stream_event(STREAM_STEP_START_MODEL_OUTPUT, "msg-1", "gemini", &mut None)
                .unwrap();
        assert_eq!(text_start.len(), 1); // content_block_start (text)

        let text_delta =
            translate_stream_event(STREAM_STEP_DELTA_TEXT, "msg-1", "gemini", &mut None).unwrap();
        assert_eq!(text_delta.len(), 1); // content_block_delta (text)

        let text_stop =
            translate_stream_event(STREAM_STEP_STOP, "msg-1", "gemini", &mut None).unwrap();
        assert_eq!(text_stop.len(), 1); // content_block_stop

        let completed =
            translate_stream_event(STREAM_INTERACTION_COMPLETED, "msg-1", "gemini", &mut None)
                .unwrap();
        assert_eq!(completed.len(), 2); // message_delta + message_stop (ContentBlockStop already emitted by StepStop)

        // Total events: 1 + 1 + 1 + 1 + 1 + 1 + 1 + 3 = 10
        let total = created.len()
            + thought_start.len()
            + sig_delta.len()
            + thought_stop.len()
            + text_start.len()
            + text_delta.len()
            + text_stop.len()
            + completed.len();
        assert_eq!(total, 9);
    }

    #[test]
    fn build_interaction_url_preserves_query_params_for_lifecycle_operations() {
        // When endpoint_interactions has auth-relevant query params like ?key=ABC,
        // lifecycle operations (cancel/delete/get) must preserve them.
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions?key=ABC".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/int-1/cancel");
        assert_eq!(
            result,
            "https://host/v1beta/interactions/int-1/cancel?key=ABC"
        );
    }

    // ── 2.1 RED: empty interaction_id hits list endpoint ─────────

    #[test]
    fn build_interaction_url_empty_id_reaches_list_endpoint() {
        // Bug: get_interaction("") → build_interaction_url(route, "/")
        // → GET /v1beta/interactions/ which is the LIST endpoint, not
        // a specific interaction. Returns 200 → recovery treats it as
        // "found" instead of "not found".
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/");
        // RED: produces list endpoint URL — used by get_interaction("")
        assert!(result.ends_with("/v1beta/interactions/"));
    }

    #[test]
    fn build_interaction_url_no_query_params_unchanged() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/int-1/cancel");
        assert_eq!(result, "https://host/v1beta/interactions/int-1/cancel");
    }

    #[test]
    fn build_interaction_url_preserves_multiple_query_params() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions?key=ABC&alt=sse".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(
            result,
            "https://host/v1beta/interactions/cancel?key=ABC&alt=sse"
        );
    }

    #[test]
    fn build_interaction_url_preserves_existing_query_params() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some(
                "https://host/v1beta/interactions?model=gemini-2.0-flash&alt=sse".into(),
            ),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(
            result,
            "https://host/v1beta/interactions/cancel?model=gemini-2.0-flash&alt=sse"
        );
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
    fn build_interaction_url_preserves_bare_qmark_param() {
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions?model".into()),
            ..Default::default()
        };
        let result = build_interaction_url(&route, "/cancel");
        assert_eq!(result, "https://host/v1beta/interactions/cancel?model");
    }

    #[test]
    fn build_interaction_url_strips_empty_qmark() {
        // Bare "?" with nothing after it is treated as no query
        let route = crate::config::RouteTarget {
            section: "test".into(),
            endpoint_interactions: Some("https://host/v1beta/interactions?".into()),
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
    async fn resolve_session_id_prefers_x_client_request_id_over_all() {
        let handler = test_interactions_handler();
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Client-Request-Id",
            HeaderValue::from_static("client-req-999"),
        );
        headers.insert("x-request-id", HeaderValue::from_static("req-id-123"));
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("session-456"),
        );
        let body = serde_json::json!({});
        let result = handler.resolve_session_id(&headers, &body);
        assert_eq!(result, "client-req-999");
    }

    #[tokio::test]
    async fn resolve_session_id_prefers_x_claude_code_session_id_over_x_request_id() {
        let handler = test_interactions_handler();
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("req-id-123"));
        headers.insert(
            "x-claude-code-session-id",
            HeaderValue::from_static("session-456"),
        );
        let body = serde_json::json!({});
        let result = handler.resolve_session_id(&headers, &body);
        // x-claude-code-session-id has higher priority than x-request-id
        assert_eq!(result, "session-456");
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
        // x-request-id is excluded — Gemini generates it, client's value is
        // only used as session identifier via resolve_session_id().
        assert!(
            req.headers().get("x-request-id").is_none(),
            "x-request-id must NOT be forwarded to Gemini upstream"
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
        assert!(
            req2.headers().get("x-request-id").is_none(),
            "x-request-id must NOT be forwarded to Gemini upstream when api_key is None"
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
        // x-request-id excluded from upstream forwarding
        assert!(map.get("x-request-id").is_none());
        // Other non-auth headers forwarded
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
        // x-request-id excluded from upstream forwarding
        assert!(map.get("x-request-id").is_none());
    }

    // ── 1.3 RED: Api-Revision override ─────────────────────────

    #[test]
    fn build_interactions_headers_client_api_revision_overwrites_fixed() {
        // Bug: Api-Revision is inserted BEFORE client header forwarding loop,
        // so client's Api-Revision header overwrites the fixed value.
        let mut client_headers = HeaderMap::new();
        client_headers.insert("Api-Revision", HeaderValue::from_static("2025-01-01"));
        let map = build_interactions_headers_map(Some("key"), &client_headers);
        // RED: current code returns "2025-01-01" (client override)
        // GREEN: should return "2026-05-20" (fixed API_REVISION)
        assert_eq!(
            map.get("Api-Revision").unwrap().to_str().unwrap(),
            "2026-05-20",
            "fixed Api-Revision must not be overridden by client headers"
        );
    }

    // --- Streaming function_call event tests (RED — not yet implemented) ---

    /// A step.start event with a function_call step (no arguments — schema patched optional).
    const STREAM_STEP_START_FUNCTION_CALL: &str = r#"{"event_type":"step.start","index":2,"step":{"type":"function_call","id":"call-1","name":"get_weather"}}"#;

    /// A step.delta event with an arguments_delta (function_call arguments).
    const STREAM_STEP_DELTA_ARGUMENTS: &str = r#"{"event_type":"step.delta","index":2,"delta":{"type":"arguments_delta","arguments":"{\"location\":\"Boston\"}"}}"#;

    #[test]
    fn translate_step_start_function_call_produces_tool_use_block() {
        let events =
            translate_stream_event(STREAM_STEP_START_FUNCTION_CALL, "msg-1", "m", &mut None)
                .unwrap();
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
        let events =
            translate_stream_event(STREAM_STEP_DELTA_ARGUMENTS, "msg-1", "m", &mut None).unwrap();
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

    // --- synthesize_openai_chunks tests ---

    #[test]
    fn synthesize_openai_chunks_with_tool_calls() {
        let resp = serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"location\":\"Boston\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let chunks = synthesize_openai_chunks("test-model", &resp);
        let joined = chunks.join("");
        assert!(
            joined.contains("tool_calls"),
            "must contain tool_calls delta chunk"
        );
        assert!(
            joined.contains("get_weather"),
            "must contain tool call name"
        );
        assert!(
            joined.contains("tool_calls"),
            "finish reason must be tool_calls"
        );
    }

    #[test]
    fn synthesize_openai_chunks_text_only_unchanged() {
        let resp = serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }]
        });
        let chunks = synthesize_openai_chunks("test-model", &resp);
        let joined = chunks.join("");
        assert!(
            joined.contains("\"content\":\"hello\""),
            "must contain text content"
        );
        assert!(
            !joined.contains("tool_calls"),
            "must NOT contain tool_calls for text-only response"
        );
    }
}
