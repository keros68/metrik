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
- The tag is the lock that reserves the number, so push it the moment the release commit lands on `main`, whether that is a direct push (`git push origin main vX.Y.Z`) or a merged release PR. An unpushed bump reserves nothing.
- A tagged version is burned. If `vX.Y.Z` already exists, never reuse or retag it — rebase and take the next number. CI refuses a version whose tag lives outside the current history.
