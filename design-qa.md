# Metrik Design QA

- Primary source visual truth: `F:\文档\xwechat_files\wxid_76st6qr3yybp22_7a50\temp\InputTemp\7169a345-f017-4d90-9db8-e5c5f0af3d8a.png`
- Supplementary references: `design/ref-cc-bar-overview.png`, `design/ref-codexbar-popover.png`, `design/reference-option-2.png`
- Compact standard implementation: `design/metrik-compact-standard-final.png`
- Compact transparent implementation: `design/metrik-compact-transparent-final.png`
- Expanded implementation: `design/metrik-expanded-final-v3.png`
- Full-view comparison evidence: `design/comparison-widget-v3.png`, `design/comparison-expanded-v3.png`
- Focused comparison evidence: `design/comparison-widget-period-v3.png`
- Browser-rendered surface: `http://127.0.0.1:4173/`
- Browser viewport: 320 × 320 for compact evidence; 1280 × 720 for expanded evidence
- Native state checked: Windows Tauri 320 × 320, standard and transparent materials, real local data
- Browser state checked: today/all Agents/demo data, seven-day state, Agent-filtered state, compact and expanded views

## Findings

No actionable P0, P1, or P2 findings remain.

- [P3] The compact product intentionally does not reproduce cc-bar's dense colored popover or CodexBar's long provider menu.
  - The references are used for material, official provider identification, thin quota bars, and scan-first hierarchy. Detailed charts, source diagnostics, and secondary quota stay in the expanded view.
- [P3] Browser screenshots can only demonstrate the two material opacities against the preview stage.
  - The native Windows check confirmed that the desktop wallpaper is visibly transmitted through the transparent Tauri window while the text cards stay readable.
- [P3] A third-party Claude review retained an objection that the CSS outer shadow has no transparent client-area margin inside the exact 320 × 320 footprint.
  - This is accepted for the current compact target: increasing the window to 360 × 360 would enlarge the click-intercepting footprint by 26.6%. The native window keeps Tauri's system shadow plus a visible inset edge; compactness wins over a browser-only ambient shadow.
- [P3] Official service marks need a final brand-permission review before mass commercial distribution.
  - The current use is the narrow identification case requested by the user, uses official downloadable assets, keeps source pixels unchanged, records provenance, and explicitly disclaims endorsement.

## Required fidelity surfaces

- Fonts and typography: passed. Geist carries compact UI text; Newsreader and Instrument Serif retain the editorial number treatment. Large values use lining/tabular numerals and remain inside the 320 px frame at million and billion scale.
- Spacing and layout rhythm: passed. In the actual 320px viewport, the period selector is `top 44 / bottom 78`, the summary begins at `82`, and measured overlap is `0`. The summary, Agent card, and footer each have 4px separation; no compact region scrolls, clips, or overlaps. Footer bottom is at 312px inside the 320px shell, leaving 8px total bottom clearance.
- Colors and visual tokens: passed. Standard compact and expanded modes are fully opaque. Transparent compact uses a 0.72 shell, protected title/metric surfaces, darker `#35373b` small text, and reduced-transparency/forced-colors fallbacks. Warm neutral, graphite, cobalt, Claude coral, and green provenance remain distinct.
- Image quality and asset fidelity: passed. ChatGPT uses OpenAI's official published app icon and Claude Code uses Anthropic's official Claude app icon from its App Store listing. Source pixels and aspect ratio are unchanged; CSS only clips their display to the rounded icon frame. No placeholder, emoji, CSS drawing, inline SVG, or approximated provider mark remains.
- Copy and content: passed. The visible provider is named ChatGPT while the compact quota explicitly says `ChatGPT · Codex 短窗`; expanded sources use `ChatGPT / Codex`. Demo provenance appears inside both the quota card and footer. Claude Code is unchanged, and “桌面小插件” replaces the earlier imprecise “挂件” wording.
- Icons and controls: passed. Window controls remain one maintained Phosphor family; the transparent toggle has distinct on/off labels, `aria-pressed`, focus treatment, and a persisted state.
- Accessibility and resilience: passed for the current desktop target. The period selector is an explicitly labelled group; controls are semantic buttons; selected states are exposed; focus remains visible; reduced motion/transparency and forced colors are supported; and the actual 320px compact frame has no horizontal or vertical overflow. The 30px window controls are appropriate for a pointer-first desktop widget, not a touch target.

## Comparison history

### Pass 1 — blocked

- [P1] The supplied screenshot showed the period selector touching the metric-label band, visually merging “今日 / 7 天 / 30 天” with “总用量 / Codex 短窗”.
  - Fix: changed compact geometry from 380 × 440 to a 320 × 320 desktop-widget composition, reduced the title bar to 42 px, moved the 34 px selector directly below it, and gave the summary its own following row.
  - Evidence: `design/comparison-widget-period-v3.png`.
- [P2] The first 320 px implementation allocated 76 px to a summary whose children had an 86 px minimum height, producing a 6 px visual collision with the Agent card.
  - Fix: allocated 86 px to the summary, reduced the Agent card to 106 px and the footer to 30 px, and added `min-height: 0` to the summary grid items.
  - Post-fix evidence: browser measurements show summary `clientHeight = scrollHeight = 86`, no region overflow, and 4 px gaps from selector → summary → Agent card → footer. Final raster: `design/metrik-compact-standard-final.png`.

### Pass 2 — passed locally

- Opened the user screenshot, cc-bar, CodexBar, standard compact, and transparent compact together in `design/comparison-widget-v3.png`.
- Opened the selected expanded direction and current expanded implementation together in `design/comparison-expanded-v3.png`.
- Rechecked the focused selector/metric region in `design/comparison-widget-period-v3.png`.
- No remaining P0/P1/P2 mismatch was found.

### Pass 3 — Claude R1 blocked, then fixed

- Claude reported three P2 items: harden the zero-spare summary row, protect transparent small-text contrast, and make compact quota provenance as explicit as expanded mode.
- Fixes: explicit metric/quota containment; transparent alpha/text/fallback revisions; compact `ChatGPT · Codex` wording; period `role="group"`; attribution wording.
- Independent verification: all compact elements had equal client/scroll dimensions; worst-case black-wallpaper contrast was calculated at 5.56:1 for shell text and 6.61:1 for card text; full build/test/lint passed.
- Evidence: `.dispatch/20260712-1544-claude-opus-review-r1.md`.

### Pass 4 — Claude R2 found a real native-width regression, then fixed

- Claude found that `@media (max-width: 880px)` leaked `margin-top: 22px` into compact mode. Wide browser evidence had not triggered that rule; a real 320px viewport would overlap the summary by 18px.
- Fix: compact period control now explicitly resets `margin: 0` and `max-width: none`.
- Additional fix: compact quota now says `ChatGPT · Codex 短窗`, uses a 116px protected column, and prioritizes demo provenance.
- Post-fix actual-viewport evidence: media query matched, computed margin was `0px`, period bottom was 78px, summary top was 82px, overlap was 0, and quota client/scroll size was exactly 116 × 86.
- Evidence: `.dispatch/20260712-1554-claude-opus-review-r2.md`.

### Pass 5 — final local/native pass after the three-round Claude cap

- Claude R3 confirmed all user-targeted behavior and visual corrections, then retained three advisory P2 classifications: external-shadow breathing room, possible DPI rounding, and brand-permission process.
- DPI concern was hardened with 2px of unused grid budget; actual footer clearance is 8px and the shell has no scroll overflow.
- Shadow concern is an explicit compactness tradeoff after native inspection; window size remains 320 × 320 rather than adding a click-intercepting 360px transparent perimeter.
- Brand concern is retained as a public-commercial-release check; it does not replace the official icons the user explicitly requested.
- Under the three-round review cap, Claude's final label remained `NEEDS_FIX`; local/browser/native design QA classifies the remaining items as P3 constraints rather than current Windows P0/P1/P2 defects.

## Interaction and runtime checks

- Transparent toggle changes the material, exposes `aria-pressed`, and survives a reload.
- Native Windows Tauri alpha was checked in both modes; standard mode was fully opaque, while wallpaper content was visible in transparent mode behind protected title/metric/card surfaces.
- Compact → expanded → compact works; expanded mode remains an opaque 1120 × 760 analysis view.
- Today and 7-day controls were clicked; the selected state and totals updated.
- ChatGPT Agent filtering was clicked; the metric label, total, and comparison copy updated.
- Official provider images loaded in both compact and expanded views.
- Browser page logs were checked after the primary journey: no application warnings or errors.
- Final Windows release build produced both MSI and NSIS bundles. The release executable was launched and checked at 320 × 320, in transparent mode, expanded at 1120 × 760, and collapsed back with transparency preserved.
- Frontend production build passed. Rust result: 48 passed, 0 failed, 2 live-environment tests ignored. Rust formatting and strict Clippy passed.

## Implementation checklist

- [x] Raised and separated the period control.
- [x] Reframed compact mode as a true 320 × 320 desktop small widget.
- [x] Added persisted standard/transparent modes and native window alpha.
- [x] Replaced provider glyph approximations with official ChatGPT and Claude app icons.
- [x] Preserved the full analysis view and accurate source semantics.
- [x] Compared full and focused source/implementation evidence after the final fix.
- [x] Checked overflow, interactions, console output, native alpha, build, tests, formatting, and lint.

final result: passed
