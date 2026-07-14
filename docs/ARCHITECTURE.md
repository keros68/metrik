# Metrik architecture

## Principles

1. Official quota, locally parsed usage, and cost estimates are different facts.
2. An adapter interprets a source; the ledger owns identity, transactions, and deduplication.
3. Source JSONL is scanned locally, but raw prompts, responses, tool output, and credentials never enter the database.
4. Missing data is shown as unavailable rather than inferred from unrelated metrics.
5. Foreground refresh discovers sources but does not reparse unchanged files; hidden/minimized views pause polling, and failed quota checks use a longer retry backoff.
6. Compact mode does not instantiate the full chart; window resizing and pinning are opt-in user actions.

## Current flow

```text
Codex JSONL ─────┐
Claude JSONL ────┤
ZCode SQLite ────┼─ adapter ─ normalized event ─ SQLite ledger ─ period query ─ UI
OpenCode JSON ───┤
Kimi wire.jsonl ─┘

Codex app-server ────────┐
Claude statusLine hook ──┼─ official quota snapshot ──────────────┘
Claude OAuth (opt-in) ───┘
```

The UI invokes one asynchronous Tauri command, `usage_snapshot(period)`. Blocking discovery, parsing, SQLite work, and the local quota subprocess run inside `spawn_blocking`, guarded by a single scan lock. On each request the engine:

1. discovers recently modified source files for the requested horizon;
2. skips unchanged files already covered by the ledger;
3. reparses changed files through a minimal typed deserializer and counts malformed or unreadable lines;
4. reconciles only the requested coverage slice of that source in one transaction, retaining older observations;
5. removes orphaned events and stale local history;
6. aggregates events in the user's local timezone, downgrading affected source payloads to `partial`.

The initial Today view scans only files that can contain today's events. Expanding to 7 or 30 days widens coverage on demand and records that coverage in `scan_source`. A parser-version upgrade performs one retained-history rebuild before returning to narrow scans.

The user-reachable `rebuild_local_ledger(period)` command takes the same scan lock, transactionally clears only the four derived Metrik tables, and immediately rebuilds the selected period. Agent source logs, source contents, credentials, and unrelated SQLite tables are outside that reset boundary.

## Event identity

- Codex: session ID plus timestamp and cumulative-token fingerprint.
- Claude Code: provider message ID only. Request ID and model are validation metadata; a conflict rejects that message and marks partial coverage without poisoning the rest of the source. Session ID remains metadata and does not prevent cross-session deduplication.
- Kimi: new-format records use the session path plus timestamp and component fingerprint; legacy StatusUpdates use the provider `message_id`.
- Source paths are observations, not event identity, so moving a session into an archive does not duplicate usage.

### Replayed history is not new usage

Two sources replay counters that are already ledgered elsewhere. Counting them is the single most expensive class of bug in this system, because the totals stay plausible:

- **Codex fork/subagent rollouts** carry `session_meta.forked_from_id` and replay the parent thread's cumulative `token_count` events before their first `turn_context`. Those counters belong to the parent session. The adapter skips them while still advancing the delta baseline, so the fork's first live delta counts only its own increment.
- **Kimi** emits both `usageScope: "turn"` (a single turn's delta) and `usageScope: "session"` (the running session total). Only `turn` records are counted.

`event_observation` allows the same logical event to be seen in more than one source without being counted twice. Progressive Claude usage updates merge component-wise maxima; non-Claude identity collisions still fail hard.

## Token normalization

```text
processed = input_uncached + cache_read + cache_write + output
```

`reasoning_output` is stored as an output sub-detail and is not added again.

Codex exposes cumulative counters. The adapter records the first snapshot, then positive component deltas. An unchanged cumulative snapshot produces no event.

Claude Code can repeat and progressively update the same assistant message. The adapter groups by message identity and keeps component-wise maxima.

Kimi legacy StatusUpdates carry no scope marker and it is not documented whether they progressively update. The adapter merges them by `message_id` taking component-wise maxima, which is correct either way: true deltas appear once per id, and progressive updates collapse to the final value instead of summing.

## Quota

Quota rows are replaced wholesale, never merged, so a window a plan no longer has cannot linger as a stale row:

- **Codex**: `primary` and `secondary` are slots, not window semantics — a plan may carry a weekly window in the `primary` slot and have no `secondary` at all. Windows are classified by `windowDurationMins` (≤ 1440 minutes is a session window, otherwise weekly); the slot name is only a fallback when the duration is absent. A successful `app-server` read replaces the whole Codex row set.
- **Claude**: the statusLine hook file is the zero-credential source. The opt-in OAuth source (off by default) reads the token Claude Code already stores and queries the official usage endpoint; the token is never persisted, uploaded, or logged. A successful read from either source replaces the whole Claude row set; a failed OAuth read falls back to the hook file rather than to a guess.
- A window whose reset time has passed without fresh data renders as `--`, not as its last known percentage.

## Storage

- `scan_source`: local locator, file state, parser version, and covered time horizon
- `usage_event`: normalized immutable usage facts
- `event_observation`: relation between logical facts and local files
- `quota_snapshot`: latest official quota per rolling window

SQLite runs in WAL mode under the operating system's local application-data directory. Source replacement and observation updates are transactional. `PARSER_VERSION` is currently 4; version changes force retained-history reconciliation.

Read-only queries (report, session stream) open the database with `SQLITE_OPEN_READ_ONLY` and skip `ensure_schema`. Running the schema check would issue `PRAGMA user_version` — a write — which blocks behind the scanner's writer and stalls those pages. On upgrade from the earlier Windows layout, the legacy Roaming database and SQLite sidecars are staged and copied only when no local database exists; legacy files are retained.

Migration conflicts fall back to a separately named recovery ledger without overwriting either side. If application-data path resolution and recovery reservation both fail, startup selects a unique temporary ledger path so the window can still open; an unwritable temporary directory then degrades the data command to the UI's explicit unavailable state instead of aborting setup.

Adapter diagnostics store only skipped-line counts in `scan_source.last_error`, never source content. A persisted diagnostic survives unchanged-file skips, so a partially read source cannot silently return to `exact` without a successful rescan.

`PRAGMA user_version` and required-column checks guard the SQLite schema. A compatible unversioned database is adopted in place; an incompatible early schema is rebuilt as a derived cache while the Agent source logs remain untouched; a database from a newer unsupported application version is refused rather than downgraded.

## Adapter boundary

Every future adapter implements:

```rust
trait AgentAdapter {
    fn id(&self) -> &'static str;
    fn discover(&self, cutoff_ms: i64) -> Vec<SourceCandidate>;
    fn parse(&self, candidate: &SourceCandidate, cutoff_ms: i64)
        -> anyhow::Result<ParsedScan>;
}
```

The current test suite covers cumulative Codex deltas, fork replay, Claude progressive updates and cross-session identity, Kimi turn/session scoping and legacy merging, quota window classification by duration, source rewrites, narrow-coverage preservation, malformed/unreadable lines, quota freshness, time buckets, timeout cleanup, and database migration. Future adapters must add their own fixtures for identity, partial input, time boundaries, and cache-token semantics before being enabled.

An adapter is only trustworthy once its field *semantics* are confirmed against real data, not just its field names. Both classes of bug this codebase has hit — Codex fork replay counted as new usage, and a weekly quota window labeled as a five-hour one — came from assuming a plausible meaning for a field that the source defines differently. When a source cannot be observed on a real machine, prefer leaving the agent unimplemented over shipping a parser whose numbers look right.

## Runtime boundary

- Compact mode refreshes every five minutes while visible; expanded mode refreshes every minute. Returning to the window triggers a refresh.
- One in-flight request is allowed from the UI; duplicate period requests are coalesced. The Rust scan remains serialized by one lock.
- A desktop single-instance guard focuses the existing window instead of starting a second scanner.
- Unchanged files are cheap metadata checks. A changed file is still reparsed from the beginning, so very large active logs remain the main CPU and disk bottleneck until an append cursor with durable parser state is implemented.
- Tauri does not remove the platform webview cost: WebView2/WebKit/WebKitGTK dominates resident memory relative to the Rust process.

## Planned device sync

Sync is deliberately outside the first release. The planned boundary is:

- opt-in only;
- end-to-end encrypted;
- standard events or aggregates only;
- deterministic strong event IDs for cross-device deduplication;
- paths, prompts, output, and credentials excluded;
- local application remains fully useful while offline.
