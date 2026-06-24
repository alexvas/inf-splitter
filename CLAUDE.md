# CLAUDE.md

Respond like smart caveman. Cut all filler, keep technical substance.
- Drop articles (a, an, the), filler (just, really, basically, actually).
- Drop pleasantries (sure, certainly, happy to).
- No hedging. Fragments fine. Short synonyms.
- Technical terms stay exact. Code blocks unchanged.
- Pattern: [thing] [action] [reason]. [next step].

## Build & test

```bash
cargo fmt --check              # formatting
cargo clippy --locked  # lint
cargo test --locked            # all tests (unit + integration)
cargo test -p inf-splitter -- test_name  # single test
./scripts/docker-smoke-test.sh # Docker integration smoke test
```

All three checks (`fmt`, `clippy`, `test`) must pass before merging. CI runs them on every push to main and every PR.

## Architecture

inf-splitter is an HTTP proxy that routes LLM inference requests to OpenAI-, Gemini- and Anthropic-compatible upstreams based on model name from a TOML config.

### Request flow

```
Client → POST /v1/chat/completions  or  /v1/messages
       → router::dispatch_messages:
           1. Peek `model` field from JSON body
           2. Config::resolve_route(&model) → RouteTarget
           3. Match ingress protocol against available endpoints:
              - endpoint_openai is set → passthrough via OpenAiHandler
              - only endpoint_anthropic → translate via AnthropicHandler
              - endpoint_interactions → translate via InteractionsHandler
              - (and vice versa for Anthropic ingress)
       → Handler sends request upstream, translates response if needed
```

### Commit

- Never commit unless explicitely asked
- Never add Co-Authored-By line into commit message
