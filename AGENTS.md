# Repository agent instructions

This public file contains the rules required to build and change Metrik correctly.
It must not contain maintainer credentials, private coordination notes, release
recovery procedures, or machine-specific configuration.

## Required context

Before changing code:

- Read `docs/ARCHITECTURE.md` for data flow, identity, storage, and adapter boundaries.
- Read `docs/PRODUCT-CONSTRAINTS.md` for durable product and platform behavior.
- If `.maintainer/MAINTAINER.md` exists, read it after the public documentation.
  It is an optional private maintainer overlay. A normal clone and CI must work
  without it, and its contents must never be copied into the public repository.

## Scope and platform boundaries

Classify each change as `shared`, `macOS shell`, `Windows shell`, `Linux shell`,
or a declared combination.

- Shared work includes adapters, quota parsing, storage, sync, settings
  contracts, and statistics. Implement it behind platform-neutral interfaces
  and verify it on all supported operating systems.
- Develop and visually approve macOS shell behavior on macOS and Windows shell
  behavior on Windows. Develop Linux shell behavior on Ubuntu 24.04 x86_64,
  with the default Wayland session as the portability baseline. Do not copy
  platform-specific window forms, materials, positioning, or menu-bar/taskbar
  behavior between shells.
- Platform selection must use native `cfg(target_os)` code and Tauri's
  compile-time platform signal. WebView user-agent detection is not an
  authoritative platform switch.
- Pull requests should state their affected scope. CI must build and test
  Windows, macOS, and Ubuntu 24.04 x86_64, and every affected shell needs a
  native smoke check before release.
- Do not bump application versions during feature work. Release preparation is
  a separate, serialized maintainer operation.

## Working style

- State assumptions that materially affect the result.
- Make surgical changes and preserve unrelated worktree changes.
- Reproduce bugs or define the expected passing behavior before fixing them.
- Run the most relevant tests, lint, type check, build, or native smoke check.
- Report what was verified and what still requires another operating system.

Run the local server and open the available browser preview yourself when a
visual task can be checked locally. Do not ask the user to start it when the
environment can do so.

For substantial visual changes, treat `design/reference-option-2.png` and the
public constraints as the source of truth for layout, anatomy, density, spacing,
color, typography, visible content, and hierarchy. Record new durable public
product decisions in `docs/PRODUCT-CONSTRAINTS.md`, not in this file. Record
maintainer-only process notes in the private overlay.

## Verification baseline

Use the checks appropriate to the change:

```bash
npm test
npm run build
cd src-tauri
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

CI is the cross-platform baseline. It does not replace native visual and window
behavior checks on the affected operating system.
