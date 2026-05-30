# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project Overview

A cross-platform digital audio workstation (DAW) for desktop (macOS, Windows, Linux):

- **Rust audio engine** — realtime DSP, mixing, plugin hosting, file I/O
- **Flutter UI** — timeline, mixer, plugin windows, automation lanes
- **Bridge** — `flutter_rust_bridge` (codegen-based FFI)

Core features: multitrack recording, VST3 hosting, built-in effects (EQ, compressor, distortion, filters), built-in synths, sample-accurate automation for every parameter.

## Developer Context (Important)

The human is an experienced Flutter/Dart developer and a **Rust beginner**. This shapes how you work in this repo:

- **Flutter/Dart code:** the human leads. Match their style, don't over-explain, treat them as an expert.
- **Rust code:** you lead. Explain what you're doing and why, especially around lifetimes, realtime-safety, `unsafe`, and the lock-free patterns. Don't assume Rust idioms are obvious. Flag the "this compiles but is wrong on the audio thread" cases explicitly.
- **The bridge:** design it to feel like a normal Dart API. The human should rarely have to read generated FFI code or think about Rust semantics from the Dart side. Streams for continuous data, futures for one-shot calls, plain Dart value types crossing the boundary.
- **Tone:** be direct and patient about Rust. The human asked to be treated kindly here; honor that without being patronizing.

When in doubt about a Rust decision, propose options with tradeoffs rather than picking silently.

## Architecture

```
┌─────────────────────────────────────────────────┐
│  Flutter UI (Dart) — human's domain             │
│  - Arrangement view, mixer, plugin windows      │
│  - Automation lanes, transport controls         │
│  - Custom painters for timeline / piano roll    │
└───────────────────┬─────────────────────────────┘
                    │ flutter_rust_bridge
                    │ (Dart-idiomatic API surface)
┌───────────────────┴─────────────────────────────┐
│  Rust Engine — Claude's domain                  │
│  ┌───────────────────────────────────────────┐  │
│  │  Control layer (non-realtime)             │  │
│  │  - Project model, command dispatch        │  │
│  │  - Plugin scanning, file I/O, undo        │  │
│  └────────────┬──────────────────────────────┘  │
│               │ lock-free queue (rtrb/ringbuf)  │
│  ┌────────────┴──────────────────────────────┐  │
│  │  Realtime audio thread                    │  │
│  │  - Graph processing, mixing, automation   │  │
│  │  - VST3 host, built-in DSP                │  │
│  └────────────┬──────────────────────────────┘  │
│               │ cpal                            │
└───────────────┴─────────────────────────────────┘
                │
        Audio I/O (CoreAudio/WASAPI/ALSA)
```

### The realtime boundary is sacred

The audio callback runs on a high-priority OS thread with a hard deadline (typically a few ms per buffer). Inside it, never:

- Allocate or free heap memory (no `Vec::push` that grows, no `Box::new`, no `String` ops)
- Lock a mutex (use lock-free SPSC queues for control→audio messaging)
- Block on I/O, channels, or syscalls
- Panic (panics in audio threads kill the stream silently on some platforms)
- Log via `println!` or `log::` macros that allocate

All buffers are pre-allocated at project-load or stream-start. Control changes from the UI flow through a lock-free ring buffer as POD command structs. Use `assert_no_alloc` in debug builds around the callback to catch violations.

**This is the #1 thing to flag in code review.** Rust's type system does *not* catch realtime-unsafety. Code that compiles and runs correctly under light load can cause audible dropouts under stress. When suggesting any code that touches the audio callback path, explicitly note whether it's realtime-safe.

## Repository Layout

```
/engine            Rust workspace (cargo workspace)
  /core            Project model, command bus, undo, persistence
  /dsp             Built-in effects + synths (no_std-friendly where possible)
  /graph           Audio graph, scheduling, automation evaluation
  /host            VST3 hosting (vst3-sys or vst3 crate)
  /io              File I/O (symphonia for decode, hound/wav for WAV)
  /bridge          flutter_rust_bridge API surface — the only crate Flutter sees
/app               Flutter application
  /lib
    /features      Feature-first organization (arrangement, mixer, etc.)
    /bridge        Generated bridge code (do not hand-edit)
    /widgets       Shared widgets
/native            Per-platform shims if needed (rare)
```

## Tech Stack

**Rust:**
- `cpal` — audio I/O
- `flutter_rust_bridge` — FFI codegen
- `rtrb` or `ringbuf` — lock-free SPSC queues across the RT boundary
- `symphonia` — audio decode (WAV/FLAC/MP3/AAC/OGG)
- `hound` — WAV write (recording)
- VST3 hosting — evaluate `vst3` crate vs `vst3-sys`; vendoring the Steinberg SDK may be necessary
- `serde` + `serde_json` or `bincode` — project file format
- `assert_no_alloc` — dev-only allocation guard for the audio thread
- `criterion` — DSP benchmarks

**Flutter:**
- Standard SDK; state management is the human's call — they have years of Flutter experience.
- High-refresh display support enabled (ProMotion / 120Hz where available).

## Audio Engine Conventions

- **Sample format internally:** `f32`, deinterleaved (planar) buffers. Convert at I/O boundaries.
- **Block size:** configurable, default 256–512 frames. DSP must handle any block size.
- **Sample rates:** support 44.1/48/88.2/96 kHz at minimum. Cache rate-dependent coefficients on rate change, not per-block.
- **Time representation:** sample-accurate `i64` sample positions internally. Convert to musical time (PPQ) and seconds at the UI boundary only.
- **Automation:** event-list style — sorted `(sample_offset, param_id, value)` events per block. Parameters smooth via per-sample or per-block ramps; never apply raw jumps to filter cutoffs or gains (zipper noise).
- **Denormals:** flush-to-zero on the audio thread. Enable FTZ/DAZ on x86; on ARM it's default. Add tiny DC offsets to feedback paths if needed.
- **Channel handling:** mono and stereo first-class; design buses for arbitrary channel counts but don't ship surround until later.

## VST3 Hosting Notes

- Plugin scanning happens off the audio thread; cache scan results to disk by plugin path + mtime.
- Each plugin instance owns its own processing context. Parameter changes arrive as automation events in the process block, not via setter calls from other threads.
- GUI handling on desktop: VST3 GUIs are native (Cocoa/HWND/X11). The Flutter side requests a window, Rust opens a native child window and hands the plugin its handle. Plan for this early — it's the trickiest cross-platform piece.
- Sandbox plugin crashes if feasible (out-of-process hosting is a v2 concern, not v1).

## Flutter ↔ Rust Bridge Rules

- The `bridge` crate is the only Rust crate Flutter knows about. Keep its API small and stable.
- **API surface is designed for the Dart side first.** If something is ergonomic in Rust but awkward in Dart, change the Rust side.
- Use streams for anything continuous (meters, playhead, level updates) — don't poll.
- Commands from UI to engine are fire-and-forget where possible; return values only when the UI genuinely needs to wait.
- Generated bridge files are checked in but never hand-edited. Regenerate with `flutter_rust_bridge_codegen generate`.
- Avoid large data crossing the boundary every frame. UI gets summarized state (peak/RMS per track, not raw samples).
- Errors cross the boundary as typed Dart exceptions, not error codes.

## Build & Run

```bash
# First-time setup
cargo install flutter_rust_bridge_codegen
cd app && flutter pub get

# Regenerate bridge after changing Rust API
flutter_rust_bridge_codegen generate

# Run desktop
cd app && flutter run -d macos   # or windows / linux

# Test
cargo test --workspace
cargo bench -p dsp               # DSP benchmarks
cd app && flutter test
```

## Testing Discipline

**Tests are not optional. Every milestone has required tests defined in `TESTING.md`.** Read that file before writing any code in this repo.

Five test layers:

1. **Rust unit tests** — DSP modules, math, data structures. Golden-file pattern for DSP. Tolerance comparisons only, never `==` on floats.
2. **Rust realtime-safety tests** — every test that exercises the audio path is wrapped in `assert_no_alloc`. No exceptions.
3. **Rust integration tests** — offline rendering of whole projects, compared to golden buffers. Bit-identical determinism required.
4. **Flutter widget tests** — UI in isolation against a `FakeEngine`. Golden tests reserved for the precision-painted surfaces (timeline, mixer, piano roll, automation lanes).
5. **End-to-end integration tests** — real engine + real bridge + real Flutter, kept few and high-value.

Every PR runs the full CI pipeline (see `TESTING.md`). When proposing new features, propose the tests alongside them. When fixing a bug, write a failing test first.

**Specifically for Claude:** when writing Rust DSP code, always include the golden-file test in the same response. When writing anything that runs on the audio thread, always include an `assert_no_alloc` test. Don't ship code that lacks these — flag the gap and offer to write the tests next if the human said to skip them.

- **Plugin hosting tests** — keep a small set of free VST3s (e.g., Surge XT, Vital, TDR Nova) for integration tests; do not commit them.

## Performance Targets

- Round-trip latency: under 10 ms at 128-frame buffer / 48 kHz on a midrange machine
- 50+ simultaneous tracks with basic processing at under 30% CPU on the same
- No xruns during normal editing operations
- UI stays at 60 fps independent of engine load (120 fps on capable displays)

## Coding Standards

**Rust:**
- `cargo fmt` and `cargo clippy -- -D warnings` clean before commit
- No `unwrap()` or `expect()` on the audio thread, ever
- Prefer `&mut [f32]` buffer slices over owned `Vec` in DSP signatures
- DSP structs implement a common trait (`Process` or similar) — block-based `process(input, output, events)`
- Document units in type names or comments: `Hz`, `Samples`, `Db`, `Ratio`
- Comments explain Rust-specific reasoning (lifetimes, ownership choices, `unsafe` justifications) since the human is learning Rust here

**Flutter/Dart:**
- `flutter analyze` clean before commit
- Feature-first folder structure, not layer-first
- Widgets stay dumb; state lives in the chosen state-management layer
- The human knows Flutter — don't over-comment idiomatic Dart

## What NOT to Do

- Don't add a heavy framework on the audio thread (no tokio, no async runtimes in DSP)
- Don't use `Arc<Mutex<T>>` to share state with the audio thread — use lock-free queues
- Don't reinvent DSP that has a well-tested crate, but be picky: many audio crates allocate or aren't realtime-safe. Read the source.
- Don't let plugin GUIs block the audio thread — they're on the UI thread, period
- Don't optimize before benchmarking — `criterion` first, then optimize the hot loop
- Don't write Dart-side workarounds for awkward Rust APIs. Fix the Rust API.

## Open Questions / Decisions Pending

- VST3 hosting crate choice — needs prototyping with both options
- Project file format — JSON for v1 (debuggable), binary later if size matters
- Time-stretching / pitch-shifting algorithm — Rubber Band (GPL/commercial) vs SoundTouch vs custom
- Whether to ship LV2 hosting on Linux alongside VST3
- State management library on the Flutter side — human's call

Update this document as those resolve.
