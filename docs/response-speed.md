# Claude and Codex response speed

Response speed collection is optional and **off by default** for each client.
It adds timing samples alongside Metrik's existing local token accounting.

This collector requires **Claude Code 2.1.212 or later**. Versions
2.1.191–2.1.211 used chunked HTTP exports, which this listener rejects; newer
versions declare the request length. See the
[official monitoring documentation](https://code.claude.com/docs/en/monitoring-usage#common-configuration-variables).
Claude Code first-token traces were checked on **2.1.325**.
Codex parsing was checked against **CLI 0.163.0-alpha.10.2**. Versions that do
not emit the required native spans remain unmeasured; Metrik does not estimate
missing timing.

## Enable collection

1. Open the expanded view, then **Settings → Data sources**.
2. Enable Claude Code, Codex, or both in **Claude / Codex response speed**.
3. Restart the corresponding client or session. Only new requests made with
   that configuration can produce samples; old history is never backfilled.
4. Complete a request, then use **Refresh collection status** to check for
   samples. The Sources page also checks when it regains focus.

If another telemetry exporter is configured, Metrik reports the conflict and
preserves it instead of replacing it. Claude collection also refuses an opt-in
that would activate dormant metrics/traces exporters or use a detailed-beta
destination that bypasses the local logs exporter. With an active external
Claude trace exporter, local log collection can continue and first-token
latency stays unavailable. On a logs-only installation, use **Collect
first-token latency**, then restart Claude Code to collect new traces.
Changing Metrik's exporter settings later
pauses collection for that source. Disabling collection restores only settings still
owned by Metrik; it preserves later edits and existing statistics. Samples use
a 182-day retention window.

## What the number means

For the selected period, Agent and model, the rate is:

```text
sum(output tokens from timed requests) / sum(recorded response seconds)
```

This is a duration-weighted historical response average. It includes waiting
and thinking within the recorded request interval. It is not pure decoding
speed, a live token stream measurement, or a benchmark across providers.
The model lists show the valid sample count; missing timing remains unavailable.

- **Claude Code:** pairs `output_tokens` with `duration_ms` from the same
  `api_request` / `claude_code.api_request` event, using its request ID for
  deduplication. The mean first-token latency uses `ttft_ms` from successful
  `claude_code.llm_request` spans, with its own sample count. The logs and
  traces for the same request share a sample ID.
- **Codex:** uses the `codex.sampling` span's elapsed time. Model metadata comes
  from its `try_run_sampling_request` parent, and output usage must be linked
  through the same request's receiving-stream completion. Joins use trace/span
  relationships, never nearby timestamps. The sampling interval ends before
  tool draining. Mean request duration uses these Codex samples, including
  retained samples, and shows their count separately from response speed.

Incomplete or ambiguous timing does not become a sample. Export batching can
delay a sample until the required related spans have arrived.
Unmatched Codex spans are held only in bounded memory; restarting Metrik can
discard those incomplete samples.

## Local storage and privacy

The collector listens only on `127.0.0.1:46319`. Claude sends local OTLP logs to
`/claude/v1/logs` and traces to `/claude/v1/traces`. Codex sends local OTLP
traces to `/codex/v1/traces`.
Metrik must be running to receive them.
Managed or launcher environment settings may override the user-level exporter
configuration; in that case, enabling the local setting alone cannot produce samples.

Raw exports are processed in bounded memory and are not persisted. The separate
sample table stores only a SHA-256 sample ID, Agent/model identifiers, a sample
timestamp, output token count, response duration, and optional first-token
latency. Raw request/trace IDs, prompts,
responses, tool output and credentials are not stored with samples or exported
through sync.

Samples never create token-usage events or add their output tokens to usage
totals. Repeated exports deduplicate by sample ID. Turning collection off stops
new sampling without deleting existing token usage or retained timing samples.
