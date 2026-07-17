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
