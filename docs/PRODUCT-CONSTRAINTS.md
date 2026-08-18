# Metrik product constraints

These are public, durable constraints that affect product correctness. They
apply to every implementation regardless of which tool or contributor makes the
change.

## Data truth and privacy

- Official quota, locally parsed usage, and estimated cost are different facts.
  Keep them visibly separate and never present an estimate as official billing.
- Never synthesize a comparison curve or replace a failed desktop read with demo
  numbers. Missing and stale data must be labeled explicitly.
- Manual refresh requests `usage_snapshot` with `force: true`, bypassing quota
  TTL caches. A failed refresh retains the last rows and marks them stale; it
  never clears them silently.
- Metrik is local-first. Optional multi-device sync must not upload prompts,
  conversation text, credentials, or raw tool output.
- The compact widget prioritizes per-agent official quota windows, including
  remaining percentage and reset countdown. Token analytics belong in the
  expanded view.
- Compact rows and strip cells have room for one number, and it answers "can I
  use this agent right now". Normally that is the shortest window (five-hour, or
  the first ranked window for agents that have no five-hour limit). When any
  window drops to the low-quota threshold, that window takes over the row,
  because an exhausted weekly budget blocks the agent no matter how full the
  session window looks. A reading whose reset moment has already passed
  describes a finished cycle and is never used as the current value.

- Usage is grouped by project only from the working directory the source itself
  records (`cwd`, or the session-to-directory mapping the agent maintains). Never
  infer a project from a log path, a session title, or a workspace ID that has no
  mapping to a real directory. Usage from a source that reports no directory is
  counted as unattributed and shown as such; it is never folded into a project or
  into an "other" bucket.

## Agent and adapter behavior

- New data sources use the adapter contract. Source-specific parsing must not
  leak into storage or presentation contracts.
- Gemini CLI is explicitly outside the supported scope.
- Kimi Code and Kimi Work are one visible agent and one quota identity: show
  only `Kimi`. Their credential sources stay separate internally, while
  duplicate official windows keep the fresher reliable sample. The monthly OMNI
  cycle remains visible; gift and booster balances remain hidden.
- Qoder, QoderWork, and Qoder CLI are one visible `Qoder` quota identity.
  Their account-level Credits are shared, so they must never create separate
  agent counters or be summed. Qoder CLI's local telemetry is not a token
  source when it reports zero counters.
- Do not expose credentials or raw provider responses through UI, logs, storage,
  sync, fixtures, or diagnostics.

## Platform forms

- The selected visual direction is `design/reference-option-2.png`. Metrik
  should feel restrained and platform-native through typography, material
  depth, spacing, and motion without imitating Apple branding or proprietary
  screens.
- The default desktop form is a compact approximately 380 × 440 widget. Full
  analytics are one click away in an expanded view. Pinning is opt-in.
- Platform-specific forms use Tauri's compile-time platform signal. A release
  must test that the native signal overrides a conflicting WebView user agent.

### Windows

- Compact transparency comes from the window's creation-time per-pixel alpha, so
  the real desktop, its icons, and any window behind Metrik show through. Do not
  simulate glass by lowering only Metrik's own background opacity, and do not
  switch DWM materials at runtime. See `WINDOWS-GLASS-IMPLEMENTATION.md`.
- The compact and strip glass offers three user-selectable tints: a dark HUD
  tint (default), a bright white frost with dark content, and a clear tint that
  is as see-through as its foreground allows. The clear tint additionally lets
  the user pick the text colour, because the two colours need opposite
  backdrops: dark text rides on a white frost, white text on a thin dark scrim.
  Pairing white text with a white frost is not offered — it cannot reach a
  readable contrast on light wallpapers at any density.
  All tints honor the glass-density slider; the choice is a Windows-only setting
  because the macOS panel material follows the system.
- Expanded mode remains opaque and owns its light/dark theme independently.
- Compact, strip, and expanded forms are reachable from one another in one
  click. Each form remembers its own position and never overwrites another
  form's position.
- Pinning and position lock apply only to compact and strip. Entering expanded
  mode always drops always-on-top and provides no pin control.
- Strip resizing preserves the screen edge it is flush to. Fully off-screen
  positions recover to the center and must never be persisted.
- Unpinned compact and strip forms dock to any work-area edge, auto-hide after
  pointer exit, and reveal from that edge's remaining visible sliver. Pinning
  immediately keeps the complete form visible. Horizontal and vertical strip
  placements are remembered independently.
- Floating-form size uses the destination monitor's DPI. Compact and strip
  reassert size from native DPI-change payloads, and window mutations are
  serialized so stale resizes cannot overwrite corrections.
- The rendered CSS viewport is the final sizing authority for Windows floating
  forms. After native resize, compact and strip must compensate WebView zoom
  drift and verify the full design viewport rather than trusting HWND size alone.
- Strip window size is measured from rendered content. Constants may seed the
  first frame but are not the source of truth.
- Compact and strip have independent continuous UI scales in the range
  0.75–2.0, applied on the next entry into that form. Expanded mode is freely
  resizable and keeps webview zoom at 1.
- Border-drag scaling is intentionally unsupported without an OS-level hit-test
  solution and native regression coverage.

### macOS

- Compact mode is a native menu-bar panel that follows current system
  appearance and material. It is not a floating Windows-style strip.
- The optional desktop component is a native WidgetKit extension embedded at
  `Metrik.app/Contents/PlugIns/MetrikWidget.appex`; shipping only its source or
  a standalone preview host does not count as releasing the feature.
- The containing app and embedded WidgetKit extension use the same
  `CFBundleVersion` and release version. Production builds never assign a
  per-build timestamp only to the extension: WidgetKit validates the archived
  bundle stub against LaunchServices before accepting a refreshed timeline.
  Release packaging must reject a version mismatch.
- The WidgetKit snapshot follows the user's compact-widget Agent selection and
  order exactly. Large widgets use a density-responsive grid for every selected
  Agent rather than a fixed six-item slice, and cold startup must not replace a
  saved selection with the full registry.
- Timeline reloads are budgeted by WidgetKit at the app level, so snapshot
  publishing never calls `reloadTimelines`: routine freshness comes from the
  five-minute timeline policy, and explicit reloads fire only on a user Agent
  selection change and once per app launch. Burning the budget on polling
  freezes the widget on a stale snapshot.
- Gallery placeholders may use illustrative data, but an installed production
  widget whose shared snapshot is unavailable renders an explicit empty state;
  it never presents the six-Agent gallery preview as live user data.
- WidgetKit owns desktop material, corner shape, placement, tint mode, and
  accessibility adaptations. Metrik does not paint a second outer card or
  expose an opacity slider for the system widget.
- The host and extension share only a compact sanitised snapshot through the
  Widget extension's sandbox preferences. It contains derived totals and quota
  metadata, never prompts, responses, credentials, or raw source paths. The
  unsandboxed host delegates all shared-data writes to a sandboxed signed
  publisher. For ad-hoc local
  builds, the publisher embeds the Widget extension's bundle identifier and
  both use standard `UserDefaults` in that extension container; App Groups are
  not reliable without a signing TeamIdentifier on macOS 26. Neither process
  opens a raw shared-container file.
  A sandboxed standalone helper must embed an Info.plist (`__info_plist`
  section with a bundle identifier); a bare binary crashes in libsecinit
  before `main()` and the snapshot silently stops updating.
  The timeline reload helper likewise presents the containing app's bundle
  identity and keeps its run loop alive long enough for WidgetCenter's
  asynchronous XPC request to reach the system before the helper exits.
- The panel material is system vibrancy from the HUD family, kept in the
  active state because the non-activating panel never becomes key. The
  glass-density slider adjusts a scrim above vibrancy, so blur stays native
  while density sweeps continuously from airy to near-solid in both light
  and dark appearance.
- The panel has a fixed design size (width 320, height follows content). The
  widget-scale setting is a Windows-only concept and is hidden on macOS; the
  panel is part of the system UI and does not scale.
- Appearance changes made in the separate expanded window (glass density)
  propagate live to the panel webview via the Tauri event bus — WKWebView
  storage events do not cross windows.
- Content overlays must remain readable on both light and dark desktops.
- The menu bar uses Metrik's own minimal grammar: one monochrome provider icon
  plus official remaining percentage for every selected agent, `--` for
  unavailable data, and `~` for stale data.
- Status items are fixed slots with stable AppKit autosave names, all created
  once at app startup and never removed or re-created within a session.
  Every native item remains visible to ControlCenter; deselecting an agent
  clears the slot and collapses its `NSStatusItem.length` to zero, while
  selecting it restores variable length. Toggling `NSStatusItem.isVisible`
  retains the object but Tahoe reinserts it at a new menu-bar position, so it
  cannot preserve the user's order. Removing/recreating items can additionally
  be silently rejected or misassociated. Empty titles must be written as an
  empty string because the current tray dependency treats a `None` title as
  “leave unchanged”.
- Clicking any status item opens the anchored compact panel. Agent selection
  updates immediately across the separate settings window, compact panel,
  WidgetKit snapshot, and menu-bar status items, and always keeps at least one
  agent. The backend database holds the single authoritative selection: every
  window reads it on startup and only an explicit settings toggle may overwrite
  it, so a hidden window can never write an older selection back over the
  current one — not during a data refresh, and not from its stale local cache.
- Provider names are not repeated as menu-bar text, and the menu structure must
  not copy another product's layout or multi-account detail.

### Linux

- The supported Linux release baseline is Ubuntu 24.04 on x86_64. Releases
  provide both a Debian package and an AppImage built on that exact runner.
- Linux uses the floating compact card, horizontal/vertical quota strip, and
  expanded view. It uses the shared content hierarchy, not Windows DWM APIs or
  macOS panel/WidgetKit behavior.
- The default Ubuntu Wayland session remains the safe window-management
  baseline: the compositor owns placement because Wayland deliberately
  withholds global window and pointer coordinates. Under X11, compact,
  horizontal-strip, and vertical-strip positions are persisted independently;
  invalid off-screen positions are rejected, and edge auto-hide is enabled.
  Always-on-top remains a best-effort compositor request.
- A pinned compact or strip surface responds immediately to pointer hover. The
  user can choose either a configurable opacity or complete visual hiding; it
  restores immediately on pointer exit so an always-on-top monitor does not
  visually obscure the desktop beneath it. On X11, hover appearance is driven
  by a dedicated native `XQueryPointer` connection and physical window bounds,
  independent of WebKit's input delivery after the surface becomes transparent.
  Wayland deliberately withholds global pointer coordinates, so it uses local
  window-boundary events and retains a visually imperceptible input surface for
  complete hiding.
- Linux pinning is a read-only presentation surface: its controls and drag
  regions are inactive, and only Linux Settings or the Linux tray menu can
  cancel pinning. The native window retains pointer input solely to drive
  immediate hover appearance; content controls must never receive those events.
- Compact and strip surfaces use the CSS glass fallback. Linux desktop stacks
  do not expose one blur protocol with consistent WebKitGTK behavior, so a
  successful-looking platform effect must not be treated as proof of native
  blur.
- Closing the main window keeps the application in the system tray when the
  desktop exposes StatusNotifier/AppIndicator items. The tray menu remains the
  explicit way to restore the window or quit.

## Window state and statistics

- Pinning belongs only to floating forms. Expanded mode is always a normal
  window.
- On Windows, compact, strip, and expanded positions are remembered separately.
- Official quota failures, local parse coverage, and estimated pricing each
  retain their own status and provenance.
