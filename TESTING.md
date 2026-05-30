# TESTING.md

Testing strategy for the DAW. Tests are not optional and are not retrofitted — they're written alongside each milestone. This document defines what gets tested where, and how CI enforces it.

## Why Test This Hard From Day One

Three reasons specific to this project:

1. **Audio bugs are silent.** A regression that introduces a subtle filter coefficient error or a sample-off automation event won't crash anything. You'll only notice when something sounds wrong, and by then five other things have changed. Tests catch these the moment they appear.
2. **Realtime safety is invisible to the compiler.** Code that allocates in the audio callback compiles fine and runs fine under light load. Under stress, it drops out. Only an `assert_no_alloc` wrapper in tests catches this mechanically.
3. **The Rust↔Dart boundary is fragile.** Generated bridge code can change behavior in ways that are easy to miss. Integration tests that exercise the boundary end-to-end protect you.

## The Five Test Layers

### Layer 1 — Rust unit tests (DSP, math, data structures)

**What:** Pure functions, individual DSP modules, the project data model, command application, undo/redo logic.

**Where:** `#[cfg(test)] mod tests` inside each crate. Run with `cargo test --workspace`.

**Pattern for DSP modules** — golden-file tests:

```rust
#[test]
fn lowpass_at_1khz_attenuates_5khz_input() {
    let mut filter = Biquad::lowpass(48_000.0, 1_000.0, 0.707);
    let input = sine_wave(48_000, 5_000.0, 1024);
    let mut output = [0.0_f32; 1024];
    filter.process(&input, &mut output);
    let attenuation_db = rms_db(&output) - rms_db(&input);
    assert!(attenuation_db < -20.0, "5kHz should be attenuated by >20dB, got {attenuation_db}");
}
```

Golden files (precomputed expected output buffers) live in `engine/<crate>/tests/golden/`. Helpers for generating and comparing them (with epsilon tolerance) live in a shared `engine/test-utils` crate.

**Tolerance rule:** `f32` comparisons use `assert_approx_eq!(a, b, epsilon)` with epsilon chosen per test. Filter outputs typically `1e-5`, envelope outputs `1e-4`, anything involving denormals or feedback `1e-3`. Never `==`.

### Layer 2 — Rust realtime-safety tests

**What:** Every DSP module, the full graph processing path, parameter smoothing, automation evaluation.

**How:** Wrap test bodies with `assert_no_alloc`:

```rust
#[test]
fn graph_process_does_not_allocate() {
    let mut graph = build_test_graph();
    let mut output = [0.0_f32; 512];
    assert_no_alloc(|| {
        graph.process(&mut output);
    });
}
```

If a test that exercises the audio path doesn't have `assert_no_alloc`, it's incomplete. CI rejects PRs that add audio-path tests without it (caught by a custom clippy lint or a CI grep — start with grep, upgrade if needed).

**Stress variant:** the same tests but in a loop running for 1000 buffers, with a panic hook installed. This catches allocations that only happen on specific paths (e.g., a `Vec` that grows on its 17th push when it hits capacity).

### Layer 3 — Rust integration tests (offline rendering)

**What:** Whole-project rendering. Load a project file, render it to a buffer, compare to a golden render.

**Where:** `engine/tests/` at the workspace root. Run with `cargo test --workspace`.

**Pattern:**

```rust
#[test]
fn render_two_track_project_matches_golden() {
    let project = load_test_project("fixtures/two_tracks.json");
    let rendered = render_offline(&project, 48_000, 4 * 48_000); // 4 seconds
    assert_buffer_matches_golden(&rendered, "golden/two_tracks.wav", 1e-4);
}
```

**Determinism is sacred here.** Same project + same sample rate + same buffer size = bit-identical output every time. If a test is flaky, you have a real bug, not a flaky test. Common culprits: uninitialized memory in pre-allocated buffers, non-deterministic iteration order over plugin params, denormal handling differences across CPUs.

These tests get added as you build the engine. By milestone 6 (timeline + clip placement), you should have ~5 of them covering the main signal paths.

### Layer 4 — Flutter widget tests

**What:** Individual widgets in isolation. Mixer channel strip renders correctly. Automation lane responds to gestures. Timeline cursor moves on play. Custom painters draw the right things.

**Where:** `app/test/`. Run with `flutter test`.

**Mock the engine.** Widget tests never touch real Rust. Define an `EngineInterface` abstract class in `app/lib/engine/` and have two implementations: the real bridge-backed one for production, and a `FakeEngine` for tests that exposes the same surface but returns scripted data.

This is also the right thing to do for the architecture overall — the UI shouldn't care whether the engine is real or fake, which means swapping in offline rendering for export or null-engine for tests is straightforward.

**Golden tests for visual surfaces only:**
- Timeline view (the precision-placement surface)
- Mixer (the channel-strip layout)
- Piano roll (when it exists)
- Automation lanes

Not for ordinary widgets. Goldens are slow and platform-sensitive — keep them where they earn it.

### Layer 5 — End-to-end integration tests

**What:** The real engine, real bridge, real Flutter app, all wired up. Run a scripted sequence of actions and assert the output.

**Where:** `app/integration_test/`. Run with `flutter test integration_test/`.

**Pattern:** these are slow, so keep them few and high-value:

- **Smoke test:** launch the app, play a tone, assert audio came out (use a loopback device or a test audio backend that captures to a buffer).
- **Project round-trip:** create a project programmatically, save it, restart the app, load it, assert state matches.
- **Per-milestone happy path:** by milestone 6, a test that places clips on the timeline and renders them correctly.

These run on every PR but slowly — 30s to 2min each is acceptable. If they get slow enough to be annoying, split them across CI jobs.

## Test Infrastructure

### Shared test utilities crate

`engine/test-utils/` provides:

- `sine_wave(sample_rate, freq, len)` — generate test signals
- `white_noise(seed, len)` — deterministic noise
- `impulse(len)` — single-sample impulse for IR measurement
- `rms_db(buffer)`, `peak_db(buffer)` — measurements
- `assert_buffer_matches_golden(actual, path, epsilon)` — golden-file comparison with auto-update mode (`UPDATE_GOLDEN=1 cargo test` regenerates)
- `assert_approx_eq!(a, b, epsilon)` — float comparison macro
- `build_test_graph()` — minimal graph for realtime-safety tests
- `load_test_project(path)` — load fixture projects

This crate is `dev-dependencies` only.

### Test fixtures

- `engine/fixtures/` — sample WAVs (short, royalty-free), test project JSONs, MIDI clips
- `engine/<crate>/tests/golden/` — golden output files for that crate's tests

Fixtures are committed. Goldens are committed. If a golden changes, the diff in the PR is the audit trail — review it like code.

### Updating goldens

When DSP output legitimately changes (e.g., you fix a bug in the filter math), regenerate goldens:

```bash
UPDATE_GOLDEN=1 cargo test --workspace
```

Review the resulting file changes in the PR. **A PR that updates a golden file without explanation should fail review.** Goldens are the contract; changing them is changing the contract.

## CI Pipeline

The build flow runs on every PR and every push to main. Order matters — fast checks first, slow checks last, so failures surface quickly.

### Stage 1 — Static checks (~30 seconds)

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `dart format --output=none --set-exit-if-changed app/`
- `cd app && flutter analyze`

### Stage 2 — Bridge regeneration check (~30 seconds)

```bash
flutter_rust_bridge_codegen generate
git diff --exit-code
```

This fails the build if the committed bridge code is out of sync with the Rust API. Forces regeneration to be part of the PR, not a surprise on main.

### Stage 3 — Rust tests (~2 minutes)

```bash
cargo test --workspace --all-features
```

Includes unit tests, realtime-safety tests, integration tests.

### Stage 4 — Flutter widget tests (~2 minutes)

```bash
cd app && flutter test
```

Includes golden tests. Golden mismatches fail the build with a diff image.

### Stage 5 — End-to-end integration tests (~5 minutes)

```bash
cd app && flutter test integration_test/
```

Runs on macOS and Windows. Linux added when you decide it's a target.

### Stage 6 — Benchmarks (informational, doesn't block)

```bash
cargo bench --workspace
```

Runs on main only. Results posted as a comment on the next PR. A regression of >10% on any benchmark gets flagged but doesn't block — humans decide if it's acceptable.

### Local CI shortcut

`scripts/check.sh` runs stages 1–4 locally. Run it before pushing. Stage 5 is too slow for the inner loop.

## Test Coverage by Milestone

This is the **minimum** — add more as you go.

| Milestone | New tests required |
|---|---|
| 0 (sine + slider) | Oscillator unit test (correct freq, no allocs), bridge smoke test |
| 1 (WAV playback) | Decode tests, playback realtime-safety test, waveform-display widget test |
| 2 (gain + pan) | Parameter smoothing unit test, channel strip widget test, end-to-end "play and meter responds" |
| 3 (biquad + EQ) | Filter response golden tests at multiple cutoffs/Qs, EQ widget test, frequency-response painter golden |
| 4 (two tracks) | Two-track mix integration test (golden render), mixer widget test |
| 5 (N tracks + project) | Project save/load round-trip, dynamic track add/remove realtime-safety test |
| 6 (timeline) | Clip scheduling integration test (golden render of placed clips), timeline widget test + golden |
| 7 (undo/redo) | Command round-trip tests, undo-to-empty test, undo coalescing test |
| 8 (recording) | Recording-to-disk integration test, latency-compensation unit test |
| 9 (automation) | Sample-accurate event delivery test, automation evaluation golden tests, lane widget test + golden |
| 10 (synth) | Voice allocation/stealing tests, MIDI handling tests, full-render golden of a MIDI clip → synth → output |
| 11 (compressor/distortion) | Compressor envelope tests, oversampling correctness, distortion golden tests |
| 12 (piano roll) | Note editing widget tests + golden, MIDI clip round-trip test |
| 13–16 (VST3) | Plugin scanning test (with bundled test plugin), parameter automation test, audio routing test |
| 17 (mixdown) | Offline render matches realtime render test |
| 18 (perf) | Benchmark regressions become blocking past this point |

## What NOT to Test

Some things waste effort:

- **Generated bridge code.** Don't write tests for what flutter_rust_bridge generates — test your hand-written wrappers around it instead.
- **Trivial getters/setters.** Test behavior, not field access.
- **The Flutter framework.** Don't test that `Slider` emits values when dragged. Test what you do with those values.
- **Third-party crates.** Trust that `cpal` works. Test your code that uses it.
- **Exact pixel positions of arbitrary widgets** with goldens. Use goldens for surfaces where pixel layout is the point (timeline, mixer).

## When Tests Slow You Down

They will, sometimes. The discipline is: **slow tests get fixed, not deleted**. Profile slow tests, parallelize them, reduce fixture sizes, use shorter render lengths. If a test is genuinely not worth its runtime, delete it explicitly with a PR comment explaining why — don't just silently skip it.

The only tests that get skipped temporarily (with a `#[ignore]` and a TODO comment with a date) are ones blocked by a known incoming refactor. If the date passes and the test is still ignored, the refactor isn't happening — un-ignore or delete.
