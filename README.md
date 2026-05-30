# DAW Project

A cross-platform digital audio workstation. Rust audio engine + Flutter UI for desktop (macOS, Windows, Linux).

## The Documents

Read in this order:

1. **[CLAUDE.md](./CLAUDE.md)** — Project rules and architecture. The contract Claude Code follows when working in this repo.
2. **[MILESTONES.md](./MILESTONES.md)** — The build order. 20 milestones from "sine wave with a slider" through "ship it."
3. **[STARTER.md](./STARTER.md)** — Milestone 0 in detail. The first concrete project to build.
4. **[TESTING.md](./TESTING.md)** — The testing strategy. Required reading before writing code.

## Status

Pre-development. No code yet. Start with the starter project described in `STARTER.md` once the toolchain is set up.

## Toolchain You'll Need

- Rust (stable) via rustup
- Flutter SDK with desktop support enabled
- `flutter_rust_bridge_codegen` (cargo install)
- macOS: Xcode command-line tools
- Windows: Visual Studio with C++ build tools
- Linux: standard build tools, ALSA dev headers

## How to Work in This Repo

Open this folder in VS Code with the recommended extensions installed (you'll be prompted). The workspace is configured for Rust analyzer + Dart/Flutter side-by-side.

When working with Claude Code, the documents above are read automatically as context. Reference them by name in conversation when relevant ("per MILESTONES.md milestone 3..." or "the realtime-safety rule in CLAUDE.md").
