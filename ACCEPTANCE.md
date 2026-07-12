# Metrik Windows x64 acceptance

Date: 2026-07-12

## Deliverables

| Artifact | Size | SHA-256 | Signature |
| --- | ---: | --- | --- |
| `src-tauri/target/release/bundle/nsis/Metrik_0.1.0_x64-setup.exe` | 2,218,143 bytes | `0C03E70FD5F97A7E2EB9A619ED0DCAE86AE561B57107DD99EE2824C8853BC800` | Not signed |
| `src-tauri/target/release/bundle/msi/Metrik_0.1.0_x64_en-US.msi` | 2,916,352 bytes | `BF9DABC6E24F132E58DE06548CA5C1FE92B39E0B49B50631C39168944DE8699C` | Not signed |
| `src-tauri/target/release/metrik.exe` | 4,994,048 bytes | `D561F666BDC225E8A9D8E67631EA81CB64EB5A22EE260D5167FE5B1ACFC42D95` | Not signed |

The PE header is x64 (`0x8664`), PE32+ (`0x020B`), Windows GUI subsystem (`2`).

## Automated verification

- `npm run build`: passed.
- `cargo fmt --check`: passed.
- `cargo test`: 48 passed, 0 failed, 2 ignored live-machine smoke tests.
- `cargo clippy --all-targets -- -D warnings`: passed.
- Ignored Codex app-server smoke test: passed with independent primary and secondary official windows.
- Claude Opus completed three read-only review rounds. It found and helped fix the native-width margin regression, transparent contrast, and quota-provenance issues. Its final label remained `NEEDS_FIX` on two accepted tradeoffs (no extra transparent window perimeter for a browser CSS shadow; public commercial brand-permission review). Full evidence is retained under `.dispatch/`.

## Native Windows verification

- Compact window measured and operated at 320 × 320 in both standard and native transparent modes; expanded window at 1120 × 760.
- ChatGPT and Claude Code rows were visually checked with their official app icons; the transparent preference persisted and the expanded view stayed opaque.
- Actual 320px browser metrics: period `44–78`, summary starts at `82`, overlap `0`, footer bottom `312`, shell bottom `320`, and no scroll overflow.
- Expand, collapse, pin/unpin, Today/7-day switching, source drawer, Escape close, and the two-step ledger-rebuild confirmation were exercised in the release executable.
- Pending state remained responsive during the one-time retained-history re-index.
- A second launch kept exactly one `metrik.exe` process and focused the existing window.
- Billion-scale real totals render as `1.77B` rather than clipping the compact headline.
- The legacy Roaming database remained byte-for-byte unchanged: SHA-256 `90DEEF3658C775C62DA6B5426F3918323863DF41F033CCF5BDE9ED8AFA9CD2CA` before and after migration.
- Local ledger schema version is 1; all 634 Codex and 384 Claude sources reached parser version 3 with zero source errors at acceptance time.

## Accuracy reconciliation

- Codex point-in-time cross-check: Codex's own `state_5.sqlite` reported 1,767,159,613 tokens for today's threads; Metrik reported 1,766,978,726. The 180,887-token gap was 0.0102% while the active acceptance thread was still writing.
- Claude Code independent raw-log regrouping produced 1,752,158 tokens across 32 provider message IDs; Metrik produced exactly 1,752,158.
- Metrik labels Codex log quota snapshots separately from live app-server quota and never invents Claude quota.

## Resource observation

Visible compact idle sample over 15 seconds on a 16-logical-processor machine:

- whole process tree: 0.55% machine CPU, 389.2 MB working set, 189.1 MB private memory;
- native `metrik.exe`: 30.4 MB working set, 7.5 MB private memory;
- the remainder is the system WebView2 process tree.

This is a small binary and low idle CPU result, but it is not a native-toolkit memory footprint. Large active JSONL files are still reparsed after they change; an append cursor remains the main performance follow-up.

## Acceptance boundary

- Verified: Windows 10/11 x64 release executable and both installer formats.
- Not yet delivered: macOS/Linux artifacts, Windows ARM64, cross-device sync, code signing, and a formal provider-mark permission review before mass commercial distribution.
