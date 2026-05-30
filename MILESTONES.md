# MILESTONES.md

The build order for the DAW. Each milestone is a **complete, demoable artifact** — not a checkpoint on the way to something else. You should be able to stop after any milestone and have something that works, even if it's not the final product.

Rule: **don't start milestone N+1 until N is solid.** Every milestone teaches you something the next one depends on. Skipping ahead means debugging two unknowns at once, which is how solo projects die.

---

## Phase 0 — Foundations

### Milestone 0: Sine + Slider (the STARTER)

Covered in `STARTER.md`. A Flutter window, a slider, a Rust engine playing a sine wave through `cpal`.

**Proves:** The whole stack works end-to-end. Bridge, realtime thread, parameter passing, audio I/O on macOS and Windows.

**Stop signal:** Slider drags produce smooth pitch glides with no clicks or dropouts.

---

## Phase 1 — Single-Track Foundation

### Milestone 1: WAV Playback

Load a WAV file from disk in Rust (via `symphonia` or `hound`), stream it through the audio callback, expose play/pause/stop/seek to Dart. Add a basic waveform display in Flutter using `CustomPainter` — peak data computed once on load, not per-frame.

**Proves:** File I/O works. Streaming audio from a buffer (not synthesizing it) works. Your custom-painter chops are intact.

**New concepts:** Streaming a pre-loaded buffer through the audio thread without allocating. Decoding off the audio thread.

**Stop signal:** Load any WAV, scrub through it, see the waveform, hear it play cleanly.

---

### Milestone 2: One Built-in Effect (Gain + Pan)

Add a gain knob and pan knob between the playback source and the output. Parameters are smoothed (per-sample ramps). Add a peak meter that updates from the audio thread via a stream.

**Proves:** Parameter smoothing pattern. The first real cross-thread continuous data stream (audio → UI for meters). Basic signal flow concept.

**New concepts:** The `Process` trait pattern. Designing DSP modules that compose. Lock-free atomics for meter values.

**Stop signal:** Drag the gain knob during playback — meter responds, audio responds, no zipper noise. Pan moves the signal correctly in stereo.

---

### Milestone 3: Biquad Filter + EQ

A low-pass / high-pass / band-pass / shelf / peak biquad. Build a 4-band parametric EQ on top of it. Frequency-response curve drawn in Flutter from coefficients you compute on the Dart side (or stream from Rust — your call).

**Proves:** Real DSP. Coefficient recalculation on parameter change without clicks. Visualizing frequency response.

**New concepts:** Why filter coefficients are expensive to compute (so you cache them). Denormal handling. Why you need separate UI-rate and audio-rate parameter values.

**Stop signal:** Sweep the EQ during playback. Audible filter movement, visible curve movement, zero clicks even on extreme parameter changes.

---

## Phase 2 — Multitrack

### Milestone 4: Two Tracks, One Mixer

Two playback tracks, each with gain/pan/EQ from the previous milestones, summed to a master bus. Master has its own gain and meter. Mixer UI in Flutter with two channel strips and a master strip.

**Proves:** Mixing math. Per-track processing chains. The graph concept (even though it's hardcoded).

**New concepts:** Bus architecture. Why each track needs its own pre-allocated buffer set. Sample-accurate sync between tracks.

**Stop signal:** Load two WAVs, play them in sync, mix them with the channel strips.

---

### Milestone 5: N Tracks + Project Model

Generalize milestone 4 to N tracks. Introduce a real `Project` struct in Rust (tracks, clips, master bus). Serialize it to JSON. Load/save projects from disk. Add/remove tracks at runtime — without dropouts, which means tracks are pre-allocated in a pool, not created on the audio thread.

**Proves:** Dynamic track count without allocating in the callback. Project persistence. The data model is real.

**New concepts:** Object pools for realtime allocation. The control thread vs audio thread division becomes real (control owns the project, audio sees a snapshot or message stream).

**Stop signal:** Create a project with 8 tracks, save it, quit, relaunch, reload, hear the same mix.

---

### Milestone 6: Timeline + Clip Placement

The first real DAW UI moment. A horizontal timeline in Flutter. Audio clips placed on tracks at specific sample positions. Drag clips, resize clips, snap to bars (assume fixed tempo for now — say 120 BPM). Playhead follows playback.

**Proves:** The "high accuracy placement" requirement you asked about. Sample-accurate clip scheduling. Custom painter performance with many clips.

**New concepts:** Sample-position arithmetic vs musical time. Clip scheduling in the audio thread (which clips are "live" at this sample position). Click-through, drag, resize hit-testing.

**Stop signal:** Place 20 clips across 4 tracks, drag them around with bar-snapping, hit play, hear the arrangement.

---

## Phase 3 — Production-Grade Plumbing

### Milestone 7: Undo/Redo + Command Bus

Every project mutation goes through a command bus. Commands are reversible. Undo stack with reasonable memory limits. UI binds to keyboard shortcuts and dirty-state tracking.

**Proves:** The architecture scales to a real editing app. Without this, every feature added later becomes harder.

**New concepts:** Command pattern. Why this needs to be in place *before* you have lots of features, not after. Coalescing rapid commands (slider drags shouldn't make 100 undo entries).

**Stop signal:** Make 50 edits, undo to start, redo to end, save, reload, undo still works on the loaded project.

---

### Milestone 8: Recording

Record audio input via `cpal` input streams. Write to disk as WAV during recording (off the audio thread, via a queue). Punch-in to a track at a specific position. Render the new clip into the timeline when done.

**Proves:** Input streams. Disk writing without dropouts. Round-tripping audio through the system.

**New concepts:** Why disk writing happens on a separate thread fed by a ring buffer from the audio thread. Latency compensation (recorded audio needs to land at the right sample position). Monitoring while recording.

**Stop signal:** Arm a track, hit record, perform, hit stop, hear what you just played back at the right position.

---

### Milestone 9: Automation

Every parameter you've built (gain, pan, EQ bands, filter cutoff) becomes automatable. Automation lanes on tracks with breakpoints. Sample-accurate event evaluation in the audio thread. Lane UI lets you draw, drag, delete points.

**Proves:** The automation system the whole DAW will hang off. Every future plugin parameter gets this for free.

**New concepts:** Event lists per block. Interpolation between breakpoints. Why automation is harder than it looks (curves, multiple points at the same time, parameter ID stability across project loads).

**Stop signal:** Automate a filter sweep on a track, hear it play correctly, scrub the timeline backward and the automation evaluates correctly.

---

## Phase 4 — Built-in Instruments and Effects

### Milestone 10: Built-in Synth (Subtractive)

A real polyphonic subtractive synth: 2 oscillators (saw/square/sine with PolyBLEP anti-aliasing), one filter, ADSR envelope, basic LFO. MIDI input support — start with a virtual on-screen keyboard, then add MIDI device input via `midir`.

**Proves:** Polyphonic voice management. MIDI handling. Note scheduling against the timeline.

**New concepts:** Voice stealing. Anti-aliased oscillators (the math is non-obvious). MIDI clock sync. Note-on/note-off as events alongside automation events.

**Stop signal:** Play the synth from a MIDI keyboard or on-screen, record a MIDI clip onto a track, play it back in time with audio tracks.

---

### Milestone 11: Compressor + Distortion

A proper compressor: threshold, ratio, attack, release, knee, makeup gain, sidechain input. A waveshaper distortion: drive, tone, mix, multiple curve types (soft clip, hard clip, asymmetric). Both fully automatable.

**Proves:** Dynamics processing. Nonlinear DSP. Building a small but credible effect library.

**New concepts:** Envelope followers. Look-ahead. Oversampling for distortion to avoid aliasing. Sidechain routing (a track's gain is reduced by another track's level).

**Stop signal:** A drum bus with compression set up, sidechained from a kick, audibly pumping. Distortion on a bass that doesn't alias horribly.

---

### Milestone 12: MIDI Editor (Piano Roll)

A piano roll editor for MIDI clips. Draw, drag, resize, delete notes. Velocity editing in a lower lane. Quantize commands.

**Proves:** A second precision-editing surface (after the timeline). Your custom painter knowledge generalizes.

**New concepts:** Note data structures. Quantization math. Snap-to-grid that respects the project tempo.

**Stop signal:** Write a 16-bar bassline by hand in the piano roll, route to the built-in synth, hear it play in arrangement.

---

## Phase 5 — VST3 (the hard part)

### Milestone 13: VST3 Plugin Scanning

Walk the standard VST3 directories, load each `.vst3` bundle, query its metadata without instantiating it (or with the minimum instantiation needed), cache results to disk by path + mtime. Plugin browser UI in Flutter listing what's available.

**Proves:** You can load plugin code into your process at all. Steinberg SDK linking works. Bundle handling on all three desktop OSes.

**New concepts:** COM-style interfaces in Rust (this is rough). The Steinberg SDK build setup. Why bundle/path handling differs per OS.

**Stop signal:** Scan results in a populated plugin list with names, vendors, and categories.

---

### Milestone 14: VST3 Audio Effect Hosting

Instantiate a VST3 audio effect, route audio through it, see parameters, automate them. No GUI yet — use a generic parameter list UI.

**Proves:** The hosting contract works. Parameter changes flow correctly. Process callbacks are timed right.

**New concepts:** VST3 process contexts. Parameter ID mapping between the plugin and your automation system. Bus configuration negotiation.

**Stop signal:** Load TDR Nova (free EQ), put it on a track, automate its parameters, hear the effect.

---

### Milestone 15: VST3 Plugin GUIs

The genuinely hard one. Get the plugin's native GUI to appear inside or alongside your Flutter window. On macOS that's an `NSView`, on Windows an `HWND`, on Linux an `X11 Window`. The plugin draws into a child window you create.

**Proves:** Cross-platform native window embedding. You can ship a real DAW.

**New concepts:** Platform window handles. The interaction between Flutter's compositor and native sub-windows. Why some plugins flat-out won't cooperate.

**Stop signal:** Open Surge XT or Vital, see its real GUI, twist its knobs, hear it respond.

---

### Milestone 16: VST3 Instrument Hosting

Same as 14 but for instruments — MIDI in, audio out. Route piano-roll MIDI to a hosted VST3 instrument.

**Proves:** The full instrument path works with third-party plugins.

**Stop signal:** Drop Vital on an instrument track, play it from a MIDI clip on the timeline.

---

## Phase 6 — Production Polish

### Milestone 17: Mixdown / Bounce

Render the project to a WAV (or FLAC) file offline. Faster-than-realtime if possible. Selectable bit depth and sample rate.

**Proves:** The graph can run deterministically off the realtime thread. This is also the foundation for offline freeze/render-in-place later.

**Stop signal:** Bounce a project, open the WAV in another app, hear the same mix.

---

### Milestone 18: Performance Pass

Profile under load. Find the hot loops. SIMD where it matters (`std::simd` or `wide` crate). Multi-thread the graph if a single core isn't enough. Hit the 50-track target from CLAUDE.md.

**Proves:** It's not a toy anymore.

**Stop signal:** 50 tracks, mix of audio and instrument tracks, plugins on most of them, under 30% CPU on your dev machine.

---

### Milestone 19: The Hundred Tiny Things

The list every DAW has and no marketing page mentions: keyboard shortcuts, mouse-wheel zooming, marker tracks, loop regions, count-in, metronome, time signature changes, tempo changes, fade in/out on clips, crossfades, clip gain, group/ungroup, color labels, track folders, freeze tracks, sends and returns, solo-in-place vs AFL/PFL, preferences dialog, recent projects menu.

This is a milestone but it's really a backlog. Pick from it in priority order based on what you actually miss while using your own DAW.

**Stop signal:** You're using your own DAW for real work and only occasionally getting frustrated.

---

### Milestone 20: Ship It

Code signing. Notarization on macOS. Installer on Windows. AppImage or Flatpak on Linux. Crash reporting. A real website. A name.

**Stop signal:** Someone who isn't you has used it and made a song with it.

---

## Reality Check

This is a multi-year project at hobby pace. That's fine — Reaper started as one person, Bitwig started small, Renoise was a solo project for years. The milestones are designed so that you have something usable at every stage, which means you can:

1. Use your own tool as you build it (this is the single best motivator)
2. Stop at any milestone with a working artifact, not a half-built ruin
3. Reorder later phases based on what you actually want — VST3 hosting could move earlier if you're impatient, the synth could come last if you're patient

What I'd discourage: working on milestones 13–16 (VST3) before milestone 9 (automation) is solid. VST3 plugins assume an automation system exists. Building the host before the system it plugs into is painful.

The order from 0 through 12 is the one I'd defend hardest. After that, reshuffle to taste.
