# Implementation Tasks: Fix Interactions Response Translation

**Change ID:** `fix-interactions-response-translation`

---

## Phase 1: RED — Unit tests for OpenAI response translation

- [x] 1.1 RED: test `translate_stream_event` produces correct `StreamEvent` variants (already exists) ✓
- [x] 1.2 RED: unit test for StreamEvent→OpenAI chunk translation — covered by E2E test 1.4 (StreamingTranslator integration) ✓
- [x] 1.3 RED: E2E test — OpenAI ingress → Interactions → OpenAI non-streaming response ✓ 2026-06-20
- [x] 1.4 RED: E2E test — OpenAI ingress → Interactions → OpenAI streaming SSE response ✓ 2026-06-20

**Quality Gate:**
- [x] New tests fail (RED phase confirmed) ✓
  - `interactions_openai_ingress_returns_openai_format` — panics: Anthropic format returned instead of OpenAI
  - `interactions_openai_streaming_returns_openai_sse` — panics: Anthropic SSE returned instead of OpenAI SSE

---

## Phase 2: GREEN — Non-streaming response translation

- [x] 2.1 Pass ingress protocol info to `send_and_translate` ✓
- [x] 2.2 Green: branch on protocol — build `ChatCompletionResponse` for OpenAI ingress ✓
- [x] 2.3 Use `anyllm_translate::translate_response` where applicable (Interactions response→OpenAI) — built directly as JSON ✓

**Quality Gate:**
- [x] E2E test 1.3 passes ✓
- [x] Existing Anthropic E2E tests pass (no regression) ✓
- [x] `cargo test --locked` passes ✓

---

## Phase 3: GREEN — Streaming response translation

- [x] 3.1 Pass ingress protocol info to `handle_stream_response` ✓
- [x] 3.2 Green: wrap `StreamEvent` output through `ReverseStreamingTranslator` for OpenAI ingress ✓
- [x] 3.3 Format OpenAI SSE chunks: `data: {json}\n\n` + `data: [DONE]\n\n` ✓

**Quality Gate:**
- [x] E2E test 1.4 passes ✓
- [x] Existing Anthropic streaming E2E test passes ✓
- [x] `cargo test --locked` passes (238 tests) ✓

---

## Completion Checklist

- [x] Spec updated with response-translation scenarios ✓ (routing_delta.md patched, response_translation.md added)
- [x] All phases complete ✓
- [x] All tests pass (238 tests: 173 unit + 21 E2E + 44 protocol) ✓
- [x] `cargo fmt --check` passes ✓
- [x] Ready to merge ✓
