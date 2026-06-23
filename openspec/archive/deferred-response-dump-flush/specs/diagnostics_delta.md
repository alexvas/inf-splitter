# Delta: Diagnostics

**Change ID:** `deferred-response-dump-flush`
**Affects:** `RequestDiagnostics` response dump deferral, `RotatingWriter` disk flush, test helper stabilization

---

## MODIFIED

### Requirement: RequestDiagnostics Session Guard (v2)

Response dumps are now deferred alongside ingress/egress dumps, flushed together in `finish`/`finish_with_error`.

#### Scenario: All dumps flushed atomically
- GIVEN a request that produces ingress, egress, and response dumps
- WHEN `guard.finish()` is called
- THEN all three dump events are sent to the writer channel in a single `flush_deferred_dumps` call
- AND a reader sees either 0 lines (before flush) or all lines (after flush)

### Requirement: File Rotation — RotatingWriter::flush

`RotatingWriter::flush()` now calls `sync_data()` on the underlying file after `BufWriter::flush()`, ensuring data reaches the storage device.

---

## ADDED

### Requirement: Poll Diagnostics File Stabilization

`poll_diagnostics_file` must wait for file content to stabilize before returning.

#### Scenario: Writer still appending
- GIVEN the writer thread has written 1 of 3 pending dump lines
- AND the predicate is already satisfied
- WHEN `poll_diagnostics_file` checks the file
- THEN it waits 20ms and re-reads
- AND if the size grew, continues polling until stable
- AND returns only when consecutive reads have the same size
