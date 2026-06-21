# Spec: Diagnostics

Component: `src/diagnostics.rs`

## Requirement: Diagnostic Configuration

The optional `[diagnostics]` TOML section controls stats and dump collection:

| Field | Values | Default |
|-------|--------|---------|
| `stats_output` | `"stderr"`, `"stdout"`, file path, `{per_section = path}` | `"stderr"` |
| `dump_output` | `"stderr"`, `"stdout"`, file path, `{per_section = path}` | `"stderr"` |
| `stats_mode` | `"off"`, `"error"`, `"all"` | `"off"` |
| `dump_mode` | `"off"`, `"error"`, `"all"` | `"off"` |
| `flush_period` | duration string (`"10s"`, `"1m"`) | flush every line |
| `max_file_size` | size string (`"100m"`) | no rotation |
| `max_rotated_size` | size string (`"5g"`) | no cleanup |
| `compression` | `"zip"`, `"bz2"`, `"7z"` | no compression |

### Scenario: All diagnostics off
- GIVEN no `[diagnostics]` section in config
- WHEN the proxy runs
- THEN no stats or dump events are collected (zero overhead)

### Scenario: Error-only mode
- GIVEN `stats_mode = "error"` and `dump_mode = "off"`
- WHEN a request completes successfully
- THEN no stats or dump line is written
- WHEN a request fails
- THEN a stats line is written, no dump

## Requirement: Stats Event Format

Each stats line is an NDJSON `StatsEvent` with fields:

```json
{
  "section": "deepseek",
  "request_id": "1718570000-0",
  "ts": "2026-06-17T14:30:25Z",
  "direction": "openai->anthropic",
  "model": "deepseek-v4-pro",
  "upstream": "https://api.deepseek.com/anthropic",
  "status": 200,
  "duration_ms": 1234,
  "request_size_bytes": 512,
  "response_size_bytes": 1024,
  "streaming": false,
  "input_messages": 3,
  "max_tokens": 4096,
  "messages_detail_ingress": [...],
  "messages_detail_egress": [...]
}
```

### Scenario: Stats serialization
- GIVEN a request has completed
- WHEN the stats event is recorded
- THEN it is written as a single NDJSON line to the configured sink

### Scenario: Optional fields omitted
- GIVEN `response_size_bytes` is `None` (streaming request)
- WHEN the event is serialized
- THEN the field is omitted from JSON output

### Scenario: Error field contains decoded text
- GIVEN upstream returns a gzip-compressed error response (e.g. Gemini API)
- WHEN the stats event is recorded
- THEN `error` contains the decompressed JSON text (reqwest with `gzip` feature auto-decompresses)

## Requirement: Dump Event Format

Each dump line is an NDJSON `DumpEvent` with fields:

```json
{
  "section": "deepseek",
  "request_id": "1718570000-0",
  "ts": "2026-06-17T14:30:25Z",
  "stage": "ingress",
  "direction": "request",
  "model": "deepseek-v4-pro",
  "headers": [["content-type", "application/json"]],
  "body": "{\"model\":\"deepseek-v4-pro\",\"max_tokens\":100}",
  "status": null
}
```

### Scenario: Dump with UTF-8 body
- GIVEN request body is valid UTF-8
- WHEN a dump event is recorded
- THEN `body` contains the plain text and no `encoding` field

### Scenario: Dump with binary body
- GIVEN request body is not valid UTF-8
- WHEN a dump event is recorded
- THEN `body` contains base64-encoded content and `"encoding": "base64"` is set

### Scenario: Binary body truncation
- GIVEN binary body exceeds `MAX_NON_UTF8_DUMP_LEN` (65536 bytes)
- WHEN a dump event is recorded
- THEN the body is truncated to 65536 bytes before base64 encoding

## Requirement: Timestamp Format

All diagnostic timestamps use ISO 8601 UTC format: `YYYY-MM-DDTHH:MM:SSZ`. This applies to both `StatsEvent.ts` and `DumpEvent.ts`.

### Scenario: Timestamp format
- GIVEN the current time is 2026-06-17 14:30:25 UTC
- WHEN a diagnostic event is created
- THEN `ts` is `"2026-06-17T14:30:25Z"`

## Requirement: Per-Section Output

When `stats_output` or `dump_output` uses `{per_section = path}`, separate files are created per config section.

### Scenario: Per-section file naming
- GIVEN `dump_output = {per_section = "/var/log/dump.ndjson"}` and section `deepseek`
- WHEN a dump event is recorded for that section
- THEN it writes to `/var/log/dump-deepseek.ndjson`

## Requirement: Non-Blocking Writes

All diagnostic recording uses `try_send` on bounded MPSC channels. When the channel is full, events are silently dropped — never blocking request processing.

### Scenario: Backpressure handling
- GIVEN the diagnostic writer is slow (disk I/O)
- WHEN the MPSC channel fills up (1024 capacity)
- THEN new events are dropped without affecting request latency

## Requirement: File Rotation

When `max_file_size` is set, the current output file is rotated when it exceeds the limit. Rotated files are named with a date-sequence suffix. When `compression` is set, rotated files are compressed in a background thread. When `max_rotated_size` is set, oldest rotated files are deleted when total exceeds the limit.

### Scenario: Rotation triggered
- GIVEN `max_file_size = "100m"` and current file reaches 100 MiB
- WHEN the next line is written
- THEN the current file is renamed and a new file is started

### Scenario: Compression after rotation
- GIVEN `compression = "zip"` and a file has been rotated
- WHEN rotation completes
- THEN the rotated file is compressed to `.ndjson.zip` in a background thread

### Scenario: Old file cleanup
- GIVEN `max_rotated_size = "1g"` and rotated files total 1.2 GiB
- WHEN rotation occurs
- THEN the oldest rotated files are deleted until total is under 1 GiB
