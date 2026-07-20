# Prototype Instructions

Run the local server yourself and open the preview in the browser available to this environment. Do not give the user server-start instructions when you can run it.

Before making substantial visual changes, use the Product Design plugin's `get-context` skill when the visual source is unclear or no longer matches the current goal. When the user gives durable prototype-specific design feedback, preferences, or decisions, record them in `AGENTS.md`.

When implementing from a selected generated mock, treat that image as the source of truth for layout, component anatomy, density, spacing, color, typography, visible content, and hierarchy.

## Durable product decisions

- The selected visual source is Product Design ideation option 2, saved at `design/reference-option-2.png`.
- The interface should feel Apple-like through restraint, typography, material depth, spacing, and motion; do not imitate Apple branding or proprietary screens.
- The default desktop form is a compact approximately 380 x 440 widget. Full analytics remain available through a one-click expanded view; pinning above other windows is opt-in, not forced.
- Gemini CLI is explicitly out of scope. The initial adapters are Codex and Claude Code; future agents must use the adapter contract.
- Statistics must distinguish official quota, locally parsed usage, and estimated cost. Never present estimates as official billing.
- Never synthesize a comparison curve or silently replace a failed desktop read with demo numbers. Missing or stale data must be labeled explicitly.
- The product is local-first. Multi-device sync is optional and must not upload prompts, conversation text, credentials, or raw tool output.
- On Windows, compact transparency must come from a native whole-window system backdrop; do not simulate glass by only lowering the opacity of Metrik's own background. The expanded window stays opaque and owns its light/dark theme independently.
- On macOS, the compact menu-bar panel should follow the current system appearance and use native menu/popover material like CodexBar; do not force a permanently dark material. Content overlays must keep text readable on both light and dark desktops.
- macOS must not use the floating strip/capsule form. Its menu bar uses Metrik's own minimal grammar: one monochrome provider icon plus its official remaining percentage for every Agent selected in the existing widget-Agent setting, with `--` for unavailable data and `~` for stale data; clicking any status item opens the anchored compact panel. The selection must update immediately and keep at least one Agent. Provider names should not be repeated as text, and the layout, menu structure, or multi-account detail must not copy CodexBar. Strip remains a Windows-only desktop form.
- Platform-specific window forms must use Tauri's compile-time platform signal. Never use WebView user-agent detection as the authoritative macOS/Windows switch; every release must test that the native platform signal overrides a conflicting UA.
- Pinning (always-on-top + position lock) belongs only to the floating forms (compact widget and quota strip). The expanded view is a normal window: entering it always drops always-on-top and offers no pin button — a pinned 1120x760 window traps the user on top of every app.
- On Windows the three forms are reachable from each other in one click (compact ↔ strip ↔ expanded), and each form returns to its own last position after a switch; positions are remembered per form and must never overwrite one another. Resizing the strip (orientation toggle, cell count change) preserves the screen edge it is flush to, and any position that falls fully outside every monitor must self-recover to center — a strip the user cannot reach is a trap, pinned strips have no drag region. Never persist an off-screen position either; the same applies on restore.
- The strip's window size is derived from the rendered content (measure the main-axis length after layout and resize to fit), never from hand-maintained size constants. Constants may seed the first frame only — they drifted out of sync with the CSS once and clipped the vertical strip's buttons on other DPI/font/scale combinations.
- The compact widget's primary content is per-agent official quota windows (5h/weekly remaining + reset countdown), not local token summaries; token analytics live in the expanded view. Missing or stale quota stays explicitly labeled.
- The strip prefers the five-hour window per cell, falling back to the first available ranked window.
- UI scale is continuous and per-form: compact uses `metrik:uiScale` [0.75–2.0]; the strip has its own `metrik:stripScale` [0.75–2.0]. Both are settings-page sliders that apply on next form entry. The expanded view has no scale setting — its window is freely resizable and webview zoom stays 1.
- Manual refresh triggers `usage_snapshot` with `force: true`, bypassing quota TTL caches; failure still retains and stale-labels old rows — never clears them.

## Durable cross-platform development workflow

This repository is worked on from two machines in parallel: a Windows shell and a macOS shell. These rules exist because both sides once allocated version 0.6.7 independently and one release commit had to be thrown away.

- This file is the only source of truth for these rules. Never keep a working copy of them outside the repository: a rule the other machine cannot read is not a rule.
- Classify every change before implementation as `shared`, `macOS shell`, or `Windows shell`. Shared work includes adapters, quota parsing, storage, sync, settings contracts, and statistics; implement it once behind platform-neutral interfaces and verify it on both operating systems.
- Develop and visually approve the macOS shell only on macOS, and the Windows shell only on Windows. Do not copy a window form, transparency implementation, positioning behavior, or menu-bar/taskbar interaction from one shell into the other.
- Keep platform entry points explicit: native `cfg(target_os)` code and the Tauri compile-time platform signal decide which shell runs. A platform shell must never be selected by browser/WebView heuristics or by falling back to the other platform's UI.
- Shared fixes land in the shared layer first; both shells consume the same contract. If a shared change requires shell updates, make the macOS and Windows adaptations as separate, clearly scoped changes and review each on its own operating system.
- Every pull request must identify its scope (`shared`, `macOS`, `Windows`, or a declared combination). Before release, CI must build and test both macOS and Windows, platform-selection regression tests must pass, and every affected shell must receive a native smoke check on its operating system.
- Never publish directly from a platform development branch. Merge through the protected release path only after the two-platform matrix is green; create the version tag from the verified merge commit, then inspect all macOS and Windows release assets before publishing.

### The release protocol (identical on both machines)

The shells are separate, but the version number is not: one app, one number, one tag, one Release carrying both platforms' assets. It is a shared global resource, so releases serialize — they never run in parallel.

- Either machine may release, but both follow this protocol exactly.
- The version lives in `package.json`, `package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `src-tauri/Cargo.lock`. All five must always agree. CI enforces this.
- Bump late. Rebase onto a freshly fetched `origin/main` first, change the version only in the release commit, and never let a version bump sit on a branch waiting to merge — that is exactly how the same number gets handed out twice.
- The tag is the lock that reserves the number, so push it the moment the release commit lands on `main`. The release commit must reach `main` through a merged release PR — `main` is branch-protected (pull_request + required_status_checks with `strict_required_status_checks_policy: true`), so a direct `git push origin main` is declined and only the tag push succeeds. The sequence that works: open `release/vX.Y.Z` from the release commit, push the tag in the same session, then merge the PR (which the tag workflow runs in parallel against). An unpushed bump reserves nothing.
- A tagged version is burned. If `vX.Y.Z` already exists, never reuse or retag it — rebase and take the next number. CI refuses a version whose tag lives outside the current history.
- The Release workflow runs the Windows and macOS jobs in parallel, and `tauri-action`'s `releaseDraft: true` plus `includeUpdaterJson: true` race when both jobs create the draft Release at once: you end up with two drafts for the same tag, each carrying only its own platform's assets, and each uploading a `latest.json` that lists only its own platform. Before publishing any release, verify there is exactly one draft for the tag; if two exist, download one job's assets via `gh api -H "Accept: application/octet-stream" repos/.../releases/assets/<id>` (drafts are not reachable through `browser_download_url`), `gh release upload` them onto the other draft, delete the empty one, then rebuild `latest.json` so it carries all of `darwin-aarch64`, `darwin-x86_64`, `darwin-aarch64-app`, `darwin-x86_64-app`, `windows-x86_64`, `windows-x86_64-msi`, `windows-x86_64-nsis` with signatures read from the uploaded `.sig` files. Publishing a `latest.json` that omits a platform silently breaks auto-update for that platform's installed users.
