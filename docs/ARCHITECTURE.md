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
Kimi wire.jsonl ─┤
Grok updates ────┤
Pi JSONL ────────┘

Codex app-server ─────────┐
Claude statusLine hook ───┼─ official quota snapshot ──────────────┘
Claude OAuth (opt-in) ────┤
Grok CLI billing log ─────┤
GLM/Kimi/Qoder/WorkBuddy ─┤
GLM key from pi auth ─────┘
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
- Kimi: new-format records use the session path plus timestamp and component fingerprint; legacy StatusUpdates use the provider `message_id`. Kimi Work (kimi-desktop) embeds the same kimi-code kernel and writes the same wire.jsonl under its daimon runtime home; its sessions reuse the CLI parser unchanged, with project attribution from `session_index.jsonl` (`sessionId` → `workDir`) instead of `workspaces.json`.
- Pi: provider `responseId` only (unique across 849 local assistant rows; the one row without it is an aborted message that falls back to the entry ID). `/fork` and `/clone` copy entries verbatim into a new session file, so a copy observes the same event with a different session in its payload and merges component-wise like Claude. Compaction and branch-summary summary usage is counted as its own event; the directory name is a lossy encoding and project attribution comes only from the header `cwd`.
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

`reasoning_output` is stored as an output sub-detail and is not added again. On Pi, `reasoning` is a subset of `output` in the format itself (verified on 833 local rows where `totalTokens == input + output + cacheRead + cacheWrite`); tokscale's Pi parser reaches the same conclusion.

Codex exposes cumulative counters. The adapter records the first snapshot, then positive component deltas. An unchanged cumulative snapshot produces no event.

Claude Code can repeat and progressively update the same assistant message. The adapter groups by message identity and keeps component-wise maxima.

Kimi legacy StatusUpdates carry no scope marker and it is not documented whether they progressively update. The adapter merges them by `message_id` taking component-wise maxima, which is correct either way: true deltas appear once per id, and progressive updates collapse to the final value instead of summing.

## Quota

Quota rows are replaced wholesale, never merged, so a window a plan no longer has cannot linger as a stale row:

- **Codex**: `primary` and `secondary` are slots, not window semantics — a plan may carry a weekly window in the `primary` slot and have no `secondary` at all. Windows are classified by `windowDurationMins` (≤ 1440 minutes is a session window, otherwise weekly); the slot name is only a fallback when the duration is absent. A successful `app-server` read replaces the whole Codex row set.
- **Claude**: the statusLine hook file is the zero-credential source. Every platform handles the hook natively inside `metrik --statusline`, invoked directly by Claude Code before desktop initialization; there is no external interpreter dependency. The chained delegate and the absolute quota-file path live in `metrik-statusline.json` metadata; the delegate runs through the platform shell (`cmd.exe` on Windows, `/bin/sh` on Unix) and is force-terminated by process tree/group after a 10-second timeout. Legacy generated hook scripts (`.ps1` on Windows, `.py` on Unix) are migrated to the native entry on startup. The opt-in OAuth source (off by default) reads the token Claude Code already stores and queries the official usage endpoint; the token is never persisted, uploaded, or logged. A successful read from either source replaces the whole Claude row set; a failed OAuth read falls back to the hook file rather than to a guess. That token expires within hours and only Claude Code itself renews it — on a real Mac `claude auth status --json` exited 0 reporting a logged-in account while the keychain `expiresAt` stayed ten hours in the past, and `claude auth` exposes no refresh command. Metrik does not spend the stored refresh token, since rotation would invalidate the copy Claude Code holds and could log the user out of their own client; an expired token short-circuits to the hook instead of spending a request that would be rejected.
- **Qoder**: Qoder, QoderWork, and Qoder CLI share one account-level Credits quota. The existing dashboard-cookie source reads that one quota only; it does not decrypt CLI credentials. Qoder CLI local telemetry with zero token counters is ignored rather than counted.
- **Pi**: pi is a harness, not a quota identity — it has no coding plan of its
  own. Its session usage is attributed per provider: GLM Coding Plan providers
  (`zai*`) count under the GLM card, Qwen Token Plan providers under the Qwen
  card, and direct providers (Anthropic, OpenAI, …) stay under Pi. The GLM
  quota source additionally accepts the key pi stores in
  `~/.pi/agent/auth.json`, so a pi-only install still shows the GLM quota on
  the GLM card. The Pi card carries local usage only, never a quota.
- **Qwen**: removed. The Bailian personal Token Plan exposes its quota only
  behind the console's interactive login; its cookie stopped working within
  days on a real account, and the product has no programmable quota API
  (official OpenAPI surface checked 2026-08). The Qwen card carries local
  usage attributed from pi sessions only; rows written by older versions are
  cleaned up by the unmanaged-row prune.
- A window whose reset time has passed without fresh data renders as `--`, not as its last known percentage.

## Storage

- `scan_source`: local locator, file state, parser version, and covered time horizon
- `usage_event`: normalized immutable usage facts, including the project working directory when the source reports one
- `event_observation`: relation between logical facts and local files
- `quota_snapshot`: latest official quota per rolling window

SQLite runs in WAL mode under the operating system's local application-data directory. Source replacement and observation updates are transactional. `PARSER_VERSION` is currently 5; version changes force retained-history reconciliation.

`usage_event.project_path` records the working directory an event happened in, so usage can be grouped by project as well as by session. Optional columns like it are added with `ALTER TABLE` and stay out of the required-column check: listing one there would classify every existing ledger as incompatible and rebuild it, when the column simply reads NULL until the next scan. The path is deliberately **not** part of `payload_hash` — it is attribution, not a measured quantity, and hashing it would make the same event hash differently after a parser upgrade, which non-mergeable adapters reject as an identity collision. Backfill is therefore its own write: a rescan fills the column when it is NULL and never overwrites a value already there, because where usage happened is settled fact.

How raw directories become displayed *projects* is a query-layer concern (`projects.rs`), never written into events. User rules live in one `app_setting` row: registered project roots absorb their subdirectories, hidden prefixes drop out of the project list, and edits take effect on the next read with no rescan. Unmatched paths fall back to the nearest `.git` ancestor (a worktree's `.git` file counts) and finally to the raw directory; the home directory itself, home-level dot-directories, Downloads, and the system temp directory are built-in non-projects, though a registered root overrides them because the most specific rule wins. Prefix matching is byte-wise and segment-aligned — Chinese directory names made char-boundary slicing panic, and `D:/work/usa` must not match `D:/work/usage` — and case-insensitive on Windows and macOS (NTFS and default APFS are case-insensitive; Linux stays case-sensitive). Hidden usage is reported as an explicit count, not folded into an "other" project.

The parse horizon is the retention window (65 days) for every source, and it deliberately does **not** follow the period the user selected. It used to: `today` parsed 8 days back, `month` 61. Because `scan_source.coverage_start_ms` records how far back a file was parsed, selecting a wider period invalidated every source parsed under a narrower one, and the whole re-scan ran synchronously inside one `usage_snapshot` request. On a 3 GB Codex log directory that made "30 days" a 23-minute frozen request. A fixed horizon means a file is parsed once, on arrival or on change; switching periods is then a pure SQL query. Sources still needing a parse are queued newest-first and cut off by `PARSE_BUDGET`; the remainder is reported as `indexing.pending` and the UI labels the numbers as incomplete rather than presenting a partial ledger as exact.

Per-source work must stay proportional to that source, never to the size of the ledger. Re-scanning one source once cost 1.3–2.4 s regardless of the file's size — a 70 KB log with a single event took as long as a large one — because clearing its observations scanned all of `event_observation` (no index on `source_id`; the primary key is `(event_id, source_id)`), and orphan collection then scanned all of `usage_event`. Both are now scoped: `idx_event_observation_source` makes the clear an indexed delete, and orphan collection only considers the event ids that source had just stopped observing. Same work, 4 ms.

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

Reading another project's parser does not substitute for that confirmation. Competitors reliably tell you *where the logs are* and *what the fields are called* — those are facts. They are not reliable about what a field *means*: the widely used `tokscale` assumes `output_tokens` excludes reasoning tokens and adds them separately, which 112,386 readings across 654 local Codex sessions disprove (`total_tokens == input_tokens + output_tokens` in every one, never `+ reasoning`). Projects that share such a dependency are wrong together, so agreement between several of them is not verification.

### Accounting self-check

Where a source reports its own total, `TokenVector::disagrees_with_reported_total` compares it against the components we derived. A mismatch increments `ScanDiagnostics::total_mismatches`, which marks that source incomplete and states in the sources drawer that the displayed numbers may be wrong. This exists because a misread field does not crash — it produces a plausible wrong number that nobody notices. The check converts that silent failure into a visible one, which is what makes it defensible to add adapters based on formats we have not exercised ourselves.

## Runtime boundary

- Compact mode refreshes every five minutes while visible; expanded mode refreshes every minute. While `indexing.pending > 0` the UI polls every 400 ms instead, so each snapshot spends another `PARSE_BUDGET` on the backlog until it is drained. Returning to the window triggers a refresh. A hidden window also keeps the five-minute cadence while the Windows tray quota badge is enabled, because the taskbar number must not freeze.
- One in-flight request is allowed from the UI; duplicate period requests are coalesced. The Rust scan remains serialized by one lock.
- A desktop single-instance guard focuses the existing window instead of starting a second scanner.
- Unchanged files are cheap metadata checks. A changed file is still reparsed from the beginning, so very large active logs remain the main CPU and disk bottleneck until an append cursor with durable parser state is implemented. The budget bounds a snapshot between files, not within one, so a single very large file can still overrun it.
- Tauri does not remove the platform webview cost: WebView2/WebKit/WebKitGTK dominates resident memory relative to the Rust process.

## Planned device sync

Sync is deliberately outside the first release. The planned boundary is:

- opt-in only;
- end-to-end encrypted;
- standard events or aggregates only;
- deterministic strong event IDs for cross-device deduplication;
- paths, prompts, output, and credentials excluded;
- local application remains fully useful while offline.
