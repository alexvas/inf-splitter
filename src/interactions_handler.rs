//! Gemini Interactions API handler.
//!
//! Handles Anthropic→Interactions and OpenAI→Interactions translation,
//! session state management, control messages, proxy_limit splitting,
//! and response translation back to the client's protocol.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use reqwest::Client as HttpClient;

use crate::auth::forward_request_headers;
use crate::config::{Config, RouteTarget};
use crate::control::{scan_control_messages, ControlAction};
use crate::diagnostics::{Diagnostics, DumpBody, StatsEvent};
use crate::error::AppError;
use crate::interactions as interactions_lib;
use crate::interactions_types::Interaction;
use crate::session::SessionStore;

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
            match action {
                ControlAction::CleanAll => {
                    let all = self.session_store.remove_all().await.map_err(|e| {
                        AppError::Internal(format!("session clean-all failed: {e}"))
                    })?;
                    let mut cancelled = 0usize;
                    let mut deleted = 0usize;
                    for (_sid, state) in &all {
                        if !state.interaction_id.is_empty() {
                            let _ = self.cancel_interaction(&state.interaction_id, route).await;
                            let _ = self.delete_interaction(&state.interaction_id, route).await;
                            cancelled += 1;
                            deleted += 1;
                        }
                    }
                    let msg = format!(
                        "Cleaned all {} sessions ({} cancelled, {} deleted)",
                        all.len(),
                        cancelled,
                        deleted
                    );
                    return Ok((
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": "ok", "message": msg})),
                    )
                        .into_response());
                }
                ControlAction::ExtendLifetime(until) => {
                    self.session_store
                        .extend_lifetime(&session_id, *until)
                        .await
                        .map_err(|e| AppError::Internal(format!("session extend failed: {e}")))?;
                    let msg = format!("Session {} lifetime extended to UTC {}", session_id, until);
                    return Ok((
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": "ok", "message": msg})),
                    )
                        .into_response());
                }
            }
        }

        // Get session state for delta computation
        let session = self.session_store.get_or_create(&session_id).await;
        let delivered = session.message_count;
        let incoming_count = messages.len() - control_result.stripped_count;
        let (start_index, new_count) = crate::session::compute_delta(delivered, incoming_count);

        // Build the request
        let req_body_lib = interactions_lib::build_interactions_request_anthropic(
            &body_val,
            start_index,
            route,
            if session.interaction_id.is_empty() {
                None
            } else {
                Some(&session.interaction_id)
            },
        );

        let stream = body_val
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        // Send to upstream
        let backend_url = endpoint.to_string();
        let request_body =
            serde_json::to_vec(&req_body_lib).map_err(|e| AppError::Internal(e.to_string()))?;

        // Apply proxy_limit splitting if needed
        if let Some(limit) = route.proxy_limit {
            let contents = req_body_lib
                .get("input")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            let size = serde_json::to_vec(&contents).map(|v| v.len()).unwrap_or(0);
            if size > limit {
                // Check for unsplittable element
                if interactions_lib::single_element_too_large(
                    &contents
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect::<Vec<crate::interactions_types::Content>>(),
                    limit,
                ) {
                    return Err(AppError::BadRequest(
                        "Unable to split ingress message into chunks under proxy limit."
                            .to_string(),
                    ));
                }
                return self
                    .handle_split_send(
                        &req_body_lib,
                        &contents,
                        limit,
                        &backend_url,
                        route,
                        &session_id,
                        new_count,
                        stream,
                        &model,
                        endpoint,
                        body.len(),
                        "anthropic->interactions",
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
            body.len(),
            "anthropic->interactions",
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
            match action {
                ControlAction::CleanAll => {
                    let all = self.session_store.remove_all().await.map_err(|e| {
                        AppError::Internal(format!("session clean-all failed: {e}"))
                    })?;
                    let mut cancelled = 0usize;
                    let mut deleted = 0usize;
                    for (_sid, state) in &all {
                        if !state.interaction_id.is_empty() {
                            let _ = self.cancel_interaction(&state.interaction_id, route).await;
                            let _ = self.delete_interaction(&state.interaction_id, route).await;
                            cancelled += 1;
                            deleted += 1;
                        }
                    }
                    let msg = format!(
                        "Cleaned all {} sessions ({} cancelled, {} deleted)",
                        all.len(),
                        cancelled,
                        deleted
                    );
                    return Ok((
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": "ok", "message": msg})),
                    )
                        .into_response());
                }
                ControlAction::ExtendLifetime(until) => {
                    self.session_store
                        .extend_lifetime(&session_id, *until)
                        .await
                        .map_err(|e| AppError::Internal(format!("session extend failed: {e}")))?;
                    let msg = format!("Session {} lifetime extended to UTC {}", session_id, until);
                    return Ok((
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": "ok", "message": msg})),
                    )
                        .into_response());
                }
            }
        }

        let session = self.session_store.get_or_create(&session_id).await;
        let delivered = session.message_count;
        let incoming_count = messages.len() - control_result.stripped_count;
        let (start_index, new_count) = crate::session::compute_delta(delivered, incoming_count);

        let req_body_lib = interactions_lib::build_interactions_request_openai(
            &body_val,
            start_index,
            route,
            if session.interaction_id.is_empty() {
                None
            } else {
                Some(&session.interaction_id)
            },
        );

        let stream = body_val
            .get("stream")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        let backend_url = endpoint.to_string();
        let request_body =
            serde_json::to_vec(&req_body_lib).map_err(|e| AppError::Internal(e.to_string()))?;

        if let Some(limit) = route.proxy_limit {
            let contents = req_body_lib
                .get("input")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            let size = serde_json::to_vec(&contents).map(|v| v.len()).unwrap_or(0);
            if size > limit {
                if interactions_lib::single_element_too_large(
                    &contents
                        .iter()
                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                        .collect::<Vec<crate::interactions_types::Content>>(),
                    limit,
                ) {
                    return Err(AppError::BadRequest(
                        "Unable to split ingress message into chunks under proxy limit."
                            .to_string(),
                    ));
                }
                return self
                    .handle_split_send(
                        &req_body_lib,
                        &contents,
                        limit,
                        &backend_url,
                        route,
                        &session_id,
                        new_count,
                        stream,
                        &model,
                        endpoint,
                        body.len(),
                        "openai->interactions",
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
            body.len(),
            "openai->interactions",
        )
        .await
    }

    /// Send a single interaction request and translate response.
    async fn send_and_translate(
        &self,
        url: &str,
        body: &[u8],
        route: &RouteTarget,
        session_id: &str,
        new_count: usize,
        stream: bool,
        model: &str,
        upstream_label: &str,
        request_size: usize,
        direction: &str,
    ) -> Result<Response, AppError> {
        let builder = build_interactions_headers(
            self.http
                .post(url)
                .header(header::CONTENT_TYPE, "application/json"),
            route.api_key.as_deref(),
        );

        let start = std::time::Instant::now();
        let upstream = builder.body(body.to_vec()).send().await?;
        let duration_ms = start.elapsed().as_millis() as u64;

        if !upstream.status().is_success() {
            let status = upstream.status();
            let error_body = upstream.text().await.unwrap_or_default();
            let request_id = self.diagnostics.new_request_id();
            self.diagnostics.record_stats(&StatsEvent {
                section: route.section.clone(),
                request_id: request_id.clone(),
                ts: crate::diagnostics::ts_string(),
                direction: direction.into(),
                model: model.into(),
                upstream: upstream_label.into(),
                status: status.as_u16(),
                duration_ms,
                request_size_bytes: request_size,
                response_size_bytes: Some(error_body.len()),
                streaming: stream,
                input_messages: None,
                max_tokens: None,
                messages_detail_ingress: None,
                messages_detail_egress: None,
                error: Some(error_body.clone()),
            });
            let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = crate::apply_error_translation(sc, error_body, &self.error_translation);
            return Response::builder()
                .status(sc)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .map_err(|err| AppError::Internal(err.to_string()));
        }

        let response_body = upstream.text().await?;
        let interaction: Interaction = serde_json::from_str(&response_body).map_err(|e| {
            AppError::Upstream(format!("failed to parse interaction response: {e}"))
        })?;

        // Update session
        let interaction_id = interaction.id.clone();
        let _ = self
            .session_store
            .update(session_id, interaction_id, new_count, false)
            .await;

        // Record success diagnostics
        let request_id = self.diagnostics.new_request_id();
        self.diagnostics.record_stats(&StatsEvent {
            section: route.section.clone(),
            request_id,
            ts: crate::diagnostics::ts_string(),
            direction: direction.into(),
            model: model.into(),
            upstream: upstream_label.into(),
            status: 200,
            duration_ms,
            request_size_bytes: request_size,
            response_size_bytes: Some(response_body.len()),
            streaming: stream,
            input_messages: None,
            max_tokens: None,
            messages_detail_ingress: None,
            messages_detail_egress: None,
            error: None,
        });

        // Translate response back to ingress protocol
        let text = interactions_lib::extract_interaction_text(&interaction);
        let resp = serde_json::json!({
            "id": interaction.id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": interaction.usage.as_ref().and_then(|u| u.total_input_tokens).unwrap_or(0),
                "output_tokens": interaction.usage.as_ref().and_then(|u| u.total_output_tokens).unwrap_or(0)
            }
        });

        Ok((StatusCode::OK, axum::Json(resp)).into_response())
    }

    /// Handle split sending: break content into chunks under proxy_limit.
    async fn handle_split_send(
        &self,
        req_body: &serde_json::Value,
        contents: &[serde_json::Value],
        limit: usize,
        url: &str,
        route: &RouteTarget,
        session_id: &str,
        total_message_count: usize,
        _stream: bool,
        model: &str,
        upstream_label: &str,
        request_size: usize,
        direction: &str,
    ) -> Result<Response, AppError> {
        let content_types: Vec<crate::interactions_types::Content> = contents
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        let chunks = interactions_lib::split_content_for_limit(&content_types, limit);
        let mut last_id: Option<String> = None;

        // Check if there's a system_instruction that needs splitting
        let system_instruction = req_body
            .get("system_instruction")
            .and_then(|v| v.as_str())
            .map(String::from);

        // If system_instruction + empty content exceeds limit, split system_instruction first
        if let Some(ref sys) = system_instruction {
            let empty_body = serde_json::json!({
                "input": [],
                "stream": false,
                "system_instruction": sys
            });
            let empty_size = serde_json::to_vec(&empty_body)
                .map(|v| v.len())
                .unwrap_or(0);
            if empty_size > limit {
                // System instruction is too large — split it
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
                        request_size,
                        direction,
                    )
                    .await;
            }
        }

        // Send each chunk sequentially
        let mut current_prev = req_body
            .get("previous_interaction_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        for chunk in &chunks {
            let mut chunk_req = serde_json::json!({
                "input": chunk,
                "stream": false,
            });
            if let Some(ref prev) = current_prev {
                chunk_req["previous_interaction_id"] = serde_json::json!(prev);
            }
            if let Some(ref sys) = system_instruction {
                chunk_req["system_instruction"] = serde_json::json!(sys);
            }
            let chunk_body =
                serde_json::to_vec(&chunk_req).map_err(|e| AppError::Internal(e.to_string()))?;

            let builder = build_interactions_headers(
                self.http
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
            );
            let upstream = builder.body(chunk_body).send().await?;
            if !upstream.status().is_success() {
                let status = upstream.status();
                let error_body = upstream.text().await.unwrap_or_default();
                let sc = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                let body = crate::apply_error_translation(sc, error_body, &self.error_translation);
                return Response::builder()
                    .status(sc)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()));
            }
            let response_text = upstream.text().await?;
            let interaction: Interaction = serde_json::from_str(&response_text).map_err(|e| {
                AppError::Upstream(format!("failed to parse split interaction: {e}"))
            })?;
            current_prev = Some(interaction.id.clone());
            last_id = Some(interaction.id);
        }

        // Store the LAST chunk's interaction ID
        if let Some(ref final_id) = last_id {
            let _ = self
                .session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await;
        }

        // Return the last response
        let text = "Split interactions completed".to_string();
        let resp = serde_json::json!({
            "id": last_id.unwrap_or_default(),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn"
        });
        Ok((StatusCode::OK, axum::Json(resp)).into_response())
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
        _model: &str,
        _upstream_label: &str,
        _request_size: usize,
        _direction: &str,
    ) -> Result<Response, AppError> {
        // Split system_instruction on natural boundaries
        let sys_parts = split_text_for_limit(sys, limit).map_err(|e| AppError::BadRequest(e))?;
        let mut last_id: Option<String> = None;
        let mut current_prev: Option<String> = None;

        // Send empty interactions with system_instruction chunks
        for (i, part) in sys_parts.iter().enumerate() {
            let is_last_sys = i == sys_parts.len() - 1;
            let input_for_chunk: serde_json::Value = if is_last_sys && !chunks.is_empty() {
                serde_json::to_value(&chunks[0]).unwrap_or(serde_json::json!([]))
            } else {
                serde_json::json!([])
            };
            let mut chunk_req = serde_json::json!({
                "input": input_for_chunk,
                "stream": false,
                "system_instruction": part,
            });
            if let Some(ref prev) = current_prev {
                chunk_req["previous_interaction_id"] = serde_json::json!(prev);
            }
            let chunk_body =
                serde_json::to_vec(&chunk_req).map_err(|e| AppError::Internal(e.to_string()))?;
            let builder = build_interactions_headers(
                self.http
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json"),
                route.api_key.as_deref(),
            );
            let upstream = builder.body(chunk_body).send().await?;
            let upstream_status = upstream.status();
            if !upstream_status.is_success() {
                let error_body = upstream.text().await.unwrap_or_default();
                let sc = StatusCode::from_u16(upstream_status.as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let body = crate::apply_error_translation(sc, error_body, &self.error_translation);
                return Response::builder()
                    .status(sc)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .map_err(|err| AppError::Internal(err.to_string()));
            }
            let response_text = upstream.text().await?;
            if let Ok(interaction) = serde_json::from_str::<Interaction>(&response_text) {
                current_prev = Some(interaction.id.clone());
                last_id = Some(interaction.id);
            }
        }

        // Send remaining chunks if more than one
        if chunks.len() > 1 {
            for chunk in chunks.iter().skip(1) {
                let mut chunk_req = serde_json::json!({
                    "input": chunk,
                    "stream": false,
                });
                if let Some(ref prev) = current_prev {
                    chunk_req["previous_interaction_id"] = serde_json::json!(prev);
                }
                let chunk_body = serde_json::to_vec(&chunk_req)
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                let builder = build_interactions_headers(
                    self.http
                        .post(url)
                        .header(header::CONTENT_TYPE, "application/json"),
                    route.api_key.as_deref(),
                );
                let upstream = builder.body(chunk_body).send().await?;
                let upstream_status = upstream.status();
                if !upstream_status.is_success() {
                    let error_body = upstream.text().await.unwrap_or_default();
                    let sc = StatusCode::from_u16(upstream_status.as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let body =
                        crate::apply_error_translation(sc, error_body, &self.error_translation);
                    return Response::builder()
                        .status(sc)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .map_err(|err| AppError::Internal(err.to_string()));
                }
                let response_text = upstream.text().await?;
                if let Ok(interaction) = serde_json::from_str::<Interaction>(&response_text) {
                    current_prev = Some(interaction.id.clone());
                    last_id = Some(interaction.id);
                }
            }
        }

        if let Some(ref final_id) = last_id {
            let _ = self
                .session_store
                .update(session_id, final_id.clone(), total_message_count, false)
                .await;
        }

        let resp = serde_json::json!({
            "id": last_id.unwrap_or_default(),
            "type": "message",
            "role": "assistant",
            "model": _model,
            "content": [{"type": "text", "text": "Split interactions completed"}],
            "stop_reason": "end_turn"
        });
        Ok((StatusCode::OK, axum::Json(resp)).into_response())
    }

    /// Cancel an interaction upstream (ignores 404).
    async fn cancel_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<(), String> {
        let url = build_interaction_url(route, &format!("/{interaction_id}/cancel"));
        let builder = build_interactions_headers(self.http.post(&url), route.api_key.as_deref());
        match builder.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "cancel interaction failed");
                Ok(()) // tolerate errors
            }
        }
    }

    /// Delete an interaction upstream (ignores 404).
    async fn delete_interaction(
        &self,
        interaction_id: &str,
        route: &RouteTarget,
    ) -> Result<(), String> {
        let url = build_interaction_url(route, &format!("/{interaction_id}"));
        let builder = build_interactions_headers(self.http.delete(&url), route.api_key.as_deref());
        match builder.send().await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "delete interaction failed");
                Ok(()) // tolerate errors
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
        let builder = build_interactions_headers(self.http.get(&url), route.api_key.as_deref());
        match builder.send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) => {
                tracing::warn!(interaction_id = %interaction_id, error = %e, "get interaction failed");
                Err(e.to_string())
            }
        }
    }

    fn resolve_session_id(&self, headers: &HeaderMap, body: &serde_json::Value) -> String {
        // Priority: x-request-id header → request_id body field → random
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
        uuid_v4()
    }
}

/// Build headers for interactions upstream requests.
fn build_interactions_headers(
    builder: reqwest::RequestBuilder,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut b = builder.header("Content-Type", "application/json");
    b = b.header("Api-Revision", API_REVISION);
    if let Some(key) = api_key {
        b = b.header("x-goog-api-key", key);
    }
    b
}

/// Build a URL for interactions lifecycle operations.
fn build_interaction_url(route: &RouteTarget, suffix: &str) -> String {
    // Strip trailing query params from the endpoint to build lifecycle URLs
    let base = route
        .endpoint_interactions
        .as_deref()
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta/interactions");
    let base = base.trim_end_matches('?');
    // Replace "?model" suffix with actual path
    let base = base.replace("?model", "");
    format!("{base}{suffix}")
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("{:x}", ts)
}

/// Split text into chunks that each fit under `limit` bytes.
/// Uses hierarchical boundaries: \\n\\n → \\n → . → ! → ? → , → ; → char.
/// Each chunk is as large as possible while staying under the limit.
fn split_text_for_limit(text: &str, limit: usize) -> Result<Vec<String>, String> {
    if text.as_bytes().len() <= limit {
        return Ok(vec![text.to_string()]);
    }

    let delimiters: &[&str] = &["\n\n", "\n", ". ", "! ", "? ", ", ", "; ", " "];
    let chunks = split_by_best_delimiter(text, limit, delimiters);
    if chunks.is_empty() || (chunks.len() == 1 && chunks[0].as_bytes().len() > limit) {
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

            if candidate.as_bytes().len() <= limit {
                current = candidate;
            } else {
                if !current.is_empty() {
                    // Current chunk is full — push it
                    if current.as_bytes().len() > limit {
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
                if part.as_bytes().len() <= limit {
                    current = part.to_string();
                } else {
                    // Single part too large, try finer delimiter
                    let sub = split_by_best_delimiter(part, limit, rest);
                    if sub.is_empty() {
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
            if current.as_bytes().len() > limit {
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
            Vec::new()
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
        let limit = text.as_bytes().len();
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
                chunk.as_bytes().len() <= limit,
                "chunk '{}' len {} exceeds limit {}",
                chunk,
                chunk.as_bytes().len(),
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
            assert!(chunk.as_bytes().len() <= limit);
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
                annotations: None,
                r#type: serde_json::Value::Null,
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
                annotations: None,
                r#type: serde_json::Value::Null,
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
            annotations: None,
            r#type: serde_json::Value::Null,
        });
        let size = serde_json::to_vec(&c).unwrap().len();
        assert!(!single_element_too_large(&[c], size + 100));
    }

    #[test]
    fn single_element_above_limit() {
        let c = Content::TextContent(TextContent {
            text: "hello".into(),
            annotations: None,
            r#type: serde_json::Value::Null,
        });
        let size = serde_json::to_vec(&c).unwrap().len();
        assert!(single_element_too_large(&[c], size.saturating_sub(1)));
    }

    // --- split_text_for_limit: more edge cases ---

    #[test]
    fn split_text_single_paragraph_fits() {
        let text = "You are a helpful and concise assistant.";
        let limit = text.as_bytes().len();
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
            assert!(chunk.as_bytes().len() <= limit);
        }
        // "one two" = 7, "one two three" = 13 > 10 → split
        // "three four" = 11 > 10 → split
        // "three" = 5, "four five" = 9 → probably 3 chunks
        assert!(result.len() >= 3);
    }
}
