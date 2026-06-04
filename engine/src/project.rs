//! Project file model + JSON I/O.
//!
//! This is the **canonical save/load shape** for a project. The Dart UI mirrors
//! its own per-track state and forwards parameter changes to the engine in real
//! time; on Save it pours that state into a [`ProjectFile`] and asks Rust to
//! write it; on Load Rust reads the JSON, hands the DTO back to Dart, and Dart
//! drives the engine's existing per-track setters (and, in Milestone 6, the
//! clip-placement setters) to bring the audio side into sync.
//!
//! ## Why these types are bridge-exposed *and* serde
//!
//! flutter_rust_bridge mirrors them to Dart classes (so the UI never imports the
//! `dsp` crate or generated FRB internals), and serde turns them into JSON on
//! disk. One set of types, two consumers — keeps the shape from drifting.
//!
//! ## Threading
//!
//! Nothing here runs on the audio thread. [`save_to_file`] and [`load_from_file`]
//! do real disk I/O and allocate freely; they're invoked from the control
//! thread (via the bridge API). The audio thread only sees the *effects* of a
//! load (per-track parameter commands and clip hand-offs), never these types.
//!
//! ## Format versioning
//!
//! Every file carries [`FORMAT_VERSION`]. Loaders refuse files from a newer
//! version (so a debug build never silently misinterprets a future schema), and
//! migrate older versions in code. v1 was the Milestone 5 snapshot — one
//! optional `clip_path` per track, no tempo. v2 is the Milestone 6 timeline:
//! each track holds a *list* of placed clips (each with its own start/length/
//! source-offset), and the project carries a tempo so the UI can snap clip
//! placements to musical time.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dsp::FilterKind;

/// Schema version of the project JSON. Bump when the on-disk shape changes in
/// any way the previous version's deserializer can't handle.
pub const FORMAT_VERSION: u32 = 2;

/// Sensible default tempo for a brand-new project and for v1 → v2 migration
/// (v1 files predate tempo entirely). 120 BPM is the universal "demo" tempo;
/// the UI presents it as the project default and lets the user change it.
pub const DEFAULT_TEMPO_BPM: f64 = 120.0;

/// One project's full state on disk and across the bridge.
///
/// `tracks` may have any length up to the engine's track-pool capacity
/// (`max_tracks()`); the Dart side enforces that bound. An empty `tracks` is
/// valid (a freshly-created project before the user adds anything).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectFile {
    /// On-disk schema version. See [`FORMAT_VERSION`].
    pub format_version: u32,
    /// Human-readable project name. Used in the title bar; not constrained.
    pub name: String,
    /// Project tempo in BPM, used by the timeline UI to snap clip placements to
    /// bars/beats. The engine itself is tempo-agnostic — all timing on the
    /// audio thread is in device frames — so this is a UI/data concern.
    pub tempo_bpm: f64,
    /// One entry per user-visible track, in display order. Each entry's slot in
    /// the engine's strip pool is the track's index in this list — adding a
    /// track appends here, removing a track takes that index out and shifts the
    /// rest down so engine slots are contiguous from 0.
    pub tracks: Vec<TrackState>,
    /// The master bus chain.
    pub master: MasterState,
}

impl ProjectFile {
    /// A blank "untitled" project: no tracks, a flat master bus, default tempo.
    pub fn empty() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: "Untitled".into(),
            tempo_bpm: DEFAULT_TEMPO_BPM,
            tracks: Vec::new(),
            master: MasterState::default_flat(),
        }
    }
}

/// One track's persistent state: identity, the clips placed on its timeline,
/// and the parameters of its processing chain.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrackState {
    /// User-facing label ("Track 1", "Kick", …). The UI may auto-fill this on
    /// creation; serializing it lets a renamed track keep its name across loads.
    pub name: String,
    /// Clips placed on this track's timeline. Order matches the engine's clip
    /// slot indices (`clips[i]` lands in sampler slot `i`), so the file's
    /// order is the source of truth for slot assignment. An empty list is a
    /// silent track. The list length is capped by the engine's
    /// `MAX_CLIPS_PER_TRACK`; the Dart side enforces that bound.
    pub clips: Vec<TrackClipState>,
    /// Track fader gain in decibels.
    pub gain_db: f32,
    /// Track pan, `[-1, 1]`.
    pub pan: f32,
    /// EQ bands in order. The engine expects exactly four (matching
    /// `dsp::eq::NUM_BANDS`); a future schema version that loosens this would
    /// migrate here.
    pub eq_bands: Vec<EqBandState>,
}

impl TrackState {
    /// A new empty track with `name`, no clips loaded, and the same defaults the
    /// engine's [`crate::dsp::Chain`] starts at — unity gain, centered pan, flat
    /// EQ. Stays in sync with `Eq::new` and `Gain::new(0.0)` / `Pan::new(0.0)`.
    pub fn default_named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            clips: Vec::new(),
            gain_db: 0.0,
            pan: 0.0,
            eq_bands: default_eq_bands(),
        }
    }
}

/// One clip placed on a track's timeline. The shape mirrors
/// [`crate::sampler::TimelineClip`] minus the live `Arc<AudioClip>` — on save we
/// persist the source file path; on load the Dart side re-decodes each path
/// (best-effort) and pushes a `place_clip` call to put it back on the timeline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrackClipState {
    /// Absolute (or workspace-relative — Dart's choice) path to the source WAV.
    /// On load, missing files surface in the UI as a broken-link clip; the rest
    /// of the project still opens.
    pub path: String,
    /// Position on the project timeline (device frames at the project's working
    /// sample rate) where this placement starts. Signed so a future "negative"
    /// placement is representable.
    pub start_frame: i64,
    /// Length on the timeline in device frames. `0` is the sentinel "use the
    /// source's full frame count" — keeps the JSON terse for the common case of
    /// dropping a whole WAV and not trimming it.
    pub length_frames: u32,
    /// Where in the source to begin reading, in source-rate frames. Lets a
    /// trimmed-from-the-head clip persist without re-encoding the WAV.
    pub source_offset_frames: u32,
}

/// Master bus state. Same shape as a track minus identity and clips — the
/// master has its own EQ even though Milestone 4's UI doesn't expose it, so the
/// project file is forward-compatible with a future "master EQ" pane.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MasterState {
    pub gain_db: f32,
    pub pan: f32,
    pub eq_bands: Vec<EqBandState>,
}

impl MasterState {
    /// Defaults: unity gain, centered pan, flat EQ.
    pub fn default_flat() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            eq_bands: default_eq_bands(),
        }
    }
}

/// One band of a 4-band EQ as it lives in the project file.
///
/// `kind` is the same small integer code [`crate::dsp::FilterKind::from_code`]
/// reads, so it round-trips through the existing bridge without a new mapping.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct EqBandState {
    /// Wire code matching [`FilterKind`] — 0 peak, 1 low-shelf, 2 high-shelf,
    /// 3 low-pass, 4 high-pass, 5 band-pass, 6 notch.
    pub kind: u8,
    pub freq_hz: f32,
    pub q: f32,
    pub gain_db: f32,
    pub enabled: bool,
}

/// Wire code for a [`FilterKind`]. Inverse of `FilterKind::from_code`. Kept
/// here (not on the enum) because it's only used at serialization time, never
/// on the audio path.
fn filter_kind_to_code(k: FilterKind) -> u8 {
    match k {
        FilterKind::Peak => 0,
        FilterKind::LowShelf => 1,
        FilterKind::HighShelf => 2,
        FilterKind::Lowpass => 3,
        FilterKind::Highpass => 4,
        FilterKind::Bandpass => 5,
        FilterKind::Notch => 6,
    }
}

/// The same 4-band default layout the engine's `Eq::new` starts with:
/// low-shelf @ 120 Hz, bell @ 500 Hz, bell @ 3 kHz, high-shelf @ 8 kHz, all flat.
/// If `Eq::new` ever changes, change this in lock-step.
pub fn default_eq_bands() -> Vec<EqBandState> {
    vec![
        EqBandState {
            kind: filter_kind_to_code(FilterKind::LowShelf),
            freq_hz: 120.0,
            q: 0.707,
            gain_db: 0.0,
            enabled: true,
        },
        EqBandState {
            kind: filter_kind_to_code(FilterKind::Peak),
            freq_hz: 500.0,
            q: 1.0,
            gain_db: 0.0,
            enabled: true,
        },
        EqBandState {
            kind: filter_kind_to_code(FilterKind::Peak),
            freq_hz: 3_000.0,
            q: 1.0,
            gain_db: 0.0,
            enabled: true,
        },
        EqBandState {
            kind: filter_kind_to_code(FilterKind::HighShelf),
            freq_hz: 8_000.0,
            q: 0.707,
            gain_db: 0.0,
            enabled: true,
        },
    ]
}

/// Write `project` to `path` as pretty-printed JSON. Returns a plain string
/// error so the bridge can rethrow it as a Dart exception.
///
/// Pretty-printed because the file is meant to be readable and diffable in v1
/// — CLAUDE.md picks JSON precisely for the debuggability. Switch to binary
/// later if size becomes a problem.
pub fn save_to_file(path: &Path, project: &ProjectFile) -> Result<(), String> {
    let mut to_save = project.clone();
    to_save.format_version = FORMAT_VERSION; // always write the current version
    let json = serde_json::to_string_pretty(&to_save)
        .map_err(|e| format!("failed to serialize project: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// Read a project JSON file from `path`, migrating older schemas to the current
/// [`FORMAT_VERSION`] in code. Rejects files claiming a newer schema than this
/// build supports rather than risk silently misinterpreting them.
pub fn load_from_file(path: &Path) -> Result<ProjectFile, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    // Two-step parse: peek at `format_version` first, then deserialize into the
    // matching schema. This is more flexible than trying to make one struct
    // backwards-compatible via `#[serde(default)]` because the v1 → v2 shape
    // change isn't field-additive (one field was renamed and structurally
    // changed: `clip_path: Option<String>` became `clips: Vec<TrackClipState>`).
    #[derive(Deserialize)]
    struct PeekVersion {
        format_version: u32,
    }
    let peek: PeekVersion = serde_json::from_str(&json)
        .map_err(|e| format!("invalid project file at {}: {e}", path.display()))?;

    if peek.format_version > FORMAT_VERSION {
        return Err(format!(
            "{} is from a newer format (v{}); this build supports up to v{}",
            path.display(),
            peek.format_version,
            FORMAT_VERSION
        ));
    }

    match peek.format_version {
        1 => {
            let v1: V1ProjectFile = serde_json::from_str(&json)
                .map_err(|e| format!("invalid v1 project file at {}: {e}", path.display()))?;
            Ok(migrate_v1_to_v2(v1))
        }
        2 => serde_json::from_str(&json)
            .map_err(|e| format!("invalid v2 project file at {}: {e}", path.display())),
        n => Err(format!(
            "{} claims unsupported format version {n}",
            path.display()
        )),
    }
}

// ─── v1 → v2 migration ──────────────────────────────────────────────────────
//
// v1 was the Milestone 5 snapshot: one optional clip per track, no tempo. The
// types below match that on-disk shape exactly so v1 files keep loading; the
// migration function below converts them into the current public types.

/// The Milestone 5 on-disk shape — kept private and frozen so v1 files keep
/// loading even as the public schema evolves.
#[derive(Deserialize)]
struct V1ProjectFile {
    #[allow(dead_code)] // re-validated in `load_from_file`; matched explicitly.
    format_version: u32,
    name: String,
    tracks: Vec<V1TrackState>,
    master: MasterState,
}

/// The Milestone 5 per-track shape: `clip_path: Option<String>` is what got
/// generalized into `TrackState.clips: Vec<TrackClipState>` in v2.
#[derive(Deserialize)]
struct V1TrackState {
    name: String,
    clip_path: Option<String>,
    gain_db: f32,
    pan: f32,
    eq_bands: Vec<EqBandState>,
}

/// Convert a v1 file into the v2 schema:
/// - Add a default tempo (v1 had none).
/// - Promote each track's `Some(clip_path)` into a single-clip
///   `Vec<TrackClipState>` placed at frame 0 with `length_frames = 0` (the
///   "use whole source" sentinel). `None` becomes an empty clip list.
fn migrate_v1_to_v2(v1: V1ProjectFile) -> ProjectFile {
    let tracks = v1
        .tracks
        .into_iter()
        .map(|t| TrackState {
            name: t.name,
            clips: match t.clip_path {
                Some(path) => vec![TrackClipState {
                    path,
                    start_frame: 0,
                    length_frames: 0, // sentinel: "use the source's full length"
                    source_offset_frames: 0,
                }],
                None => Vec::new(),
            },
            gain_db: t.gain_db,
            pan: t.pan,
            eq_bands: t.eq_bands,
        })
        .collect();
    ProjectFile {
        format_version: FORMAT_VERSION,
        name: v1.name,
        tempo_bpm: DEFAULT_TEMPO_BPM,
        tracks,
        master: v1.master,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_project() -> ProjectFile {
        ProjectFile {
            format_version: FORMAT_VERSION,
            name: "Demo".into(),
            tempo_bpm: 128.0,
            tracks: vec![
                TrackState {
                    name: "Drums".into(),
                    clips: vec![
                        TrackClipState {
                            path: "/tmp/kick.wav".into(),
                            start_frame: 0,
                            length_frames: 48_000,
                            source_offset_frames: 0,
                        },
                        TrackClipState {
                            path: "/tmp/snare.wav".into(),
                            start_frame: 24_000,
                            length_frames: 0, // "whole source"
                            source_offset_frames: 1024,
                        },
                    ],
                    gain_db: -3.0,
                    pan: -0.25,
                    eq_bands: default_eq_bands(),
                },
                TrackState {
                    name: "Bass".into(),
                    clips: Vec::new(),
                    gain_db: 1.5,
                    pan: 0.0,
                    eq_bands: {
                        let mut b = default_eq_bands();
                        // A non-default tweak so the round-trip exercises real values.
                        b[2].gain_db = 6.0;
                        b[2].freq_hz = 2_500.0;
                        b
                    },
                },
            ],
            master: MasterState {
                gain_db: -1.0,
                pan: 0.0,
                eq_bands: default_eq_bands(),
            },
        }
    }

    /// The headline contract: every field a Save writes is recovered byte-for-byte
    /// by a Load. If this ever fails, the project file format has silently drifted.
    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir_for_test();
        let path = dir.join("demo.daw.json");
        let p = sample_project();
        save_to_file(&path, &p).unwrap();
        let loaded = load_from_file(&path).unwrap();
        assert_eq!(p, loaded);
        let _ = std::fs::remove_file(&path);
    }

    /// An empty (zero-track) project must round-trip — that's the state a freshly
    /// created project lives in before the user adds anything.
    #[test]
    fn empty_project_round_trips() {
        let dir = tempdir_for_test();
        let path = dir.join("empty.daw.json");
        let p = ProjectFile::empty();
        save_to_file(&path, &p).unwrap();
        let loaded = load_from_file(&path).unwrap();
        assert_eq!(p, loaded);
        let _ = std::fs::remove_file(&path);
    }

    /// A file from a future schema must be rejected with a useful error, not
    /// silently misinterpreted as the current version.
    #[test]
    fn rejects_newer_format_version() {
        let dir = tempdir_for_test();
        let path = dir.join("future.daw.json");
        let mut p = ProjectFile::empty();
        p.format_version = FORMAT_VERSION + 1;
        // Hand-write the JSON since save_to_file pins the version to ours.
        std::fs::write(&path, serde_json::to_string_pretty(&p).unwrap()).unwrap();
        let err = load_from_file(&path).unwrap_err();
        assert!(err.contains("newer format"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }

    /// Garbage JSON returns a clear error rather than panicking — the bridge
    /// turns this into a Dart exception the UI can surface.
    #[test]
    fn malformed_json_errors_cleanly() {
        let dir = tempdir_for_test();
        let path = dir.join("garbage.daw.json");
        std::fs::write(&path, "this is not json {[").unwrap();
        assert!(load_from_file(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// A v1 file from Milestone 5 must still load — `clip_path: Some(...)`
    /// becomes a single-element `clips: [...]`, `clip_path: None` becomes an
    /// empty list, and the tempo defaults to [`DEFAULT_TEMPO_BPM`]. This is
    /// the contract that lets old projects keep opening in new builds.
    #[test]
    fn v1_file_migrates_to_v2_on_load() {
        let dir = tempdir_for_test();
        let path = dir.join("v1.daw.json");

        // Hand-written v1 JSON — the exact shape Milestone 5 wrote, with no
        // tempo field and `clip_path` on each track.
        let v1_json = r#"{
            "format_version": 1,
            "name": "M5 demo",
            "tracks": [
                {
                    "name": "Drums",
                    "clip_path": "/tmp/drums.wav",
                    "gain_db": -2.5,
                    "pan": 0.1,
                    "eq_bands": [
                        {"kind": 1, "freq_hz": 120.0, "q": 0.707, "gain_db": 0.0, "enabled": true},
                        {"kind": 0, "freq_hz": 500.0, "q": 1.0,   "gain_db": 0.0, "enabled": true},
                        {"kind": 0, "freq_hz": 3000.0,"q": 1.0,   "gain_db": 0.0, "enabled": true},
                        {"kind": 2, "freq_hz": 8000.0,"q": 0.707, "gain_db": 0.0, "enabled": true}
                    ]
                },
                {
                    "name": "Bass",
                    "clip_path": null,
                    "gain_db": 0.0,
                    "pan": 0.0,
                    "eq_bands": [
                        {"kind": 1, "freq_hz": 120.0, "q": 0.707, "gain_db": 0.0, "enabled": true},
                        {"kind": 0, "freq_hz": 500.0, "q": 1.0,   "gain_db": 0.0, "enabled": true},
                        {"kind": 0, "freq_hz": 3000.0,"q": 1.0,   "gain_db": 0.0, "enabled": true},
                        {"kind": 2, "freq_hz": 8000.0,"q": 0.707, "gain_db": 0.0, "enabled": true}
                    ]
                }
            ],
            "master": {
                "gain_db": 0.0,
                "pan": 0.0,
                "eq_bands": [
                    {"kind": 1, "freq_hz": 120.0, "q": 0.707, "gain_db": 0.0, "enabled": true},
                    {"kind": 0, "freq_hz": 500.0, "q": 1.0,   "gain_db": 0.0, "enabled": true},
                    {"kind": 0, "freq_hz": 3000.0,"q": 1.0,   "gain_db": 0.0, "enabled": true},
                    {"kind": 2, "freq_hz": 8000.0,"q": 0.707, "gain_db": 0.0, "enabled": true}
                ]
            }
        }"#;
        std::fs::write(&path, v1_json).unwrap();

        let loaded = load_from_file(&path).unwrap();
        assert_eq!(loaded.format_version, FORMAT_VERSION);
        assert_eq!(loaded.name, "M5 demo");
        assert!(
            (loaded.tempo_bpm - DEFAULT_TEMPO_BPM).abs() < 1e-9,
            "v1 → v2 must default tempo to {DEFAULT_TEMPO_BPM}, got {}",
            loaded.tempo_bpm
        );

        // Track with a clip_path: a single migrated TrackClipState.
        assert_eq!(loaded.tracks.len(), 2);
        let drums = &loaded.tracks[0];
        assert_eq!(drums.name, "Drums");
        assert_eq!(drums.clips.len(), 1);
        assert_eq!(drums.clips[0].path, "/tmp/drums.wav");
        assert_eq!(drums.clips[0].start_frame, 0);
        assert_eq!(
            drums.clips[0].length_frames, 0,
            "v1 → v2 must use the 0 sentinel for 'whole source' length"
        );
        assert_eq!(drums.clips[0].source_offset_frames, 0);
        assert!((drums.gain_db - -2.5).abs() < 1e-6);
        assert!((drums.pan - 0.1).abs() < 1e-6);

        // Track with clip_path == null: empty clips list.
        let bass = &loaded.tracks[1];
        assert_eq!(bass.name, "Bass");
        assert!(bass.clips.is_empty(), "null clip_path → no clips");

        // Re-saving the loaded project writes a v2 file (so old projects upgrade
        // on first save — by design).
        let resave_path = dir.join("v1-resaved.daw.json");
        save_to_file(&resave_path, &loaded).unwrap();
        let reloaded = load_from_file(&resave_path).unwrap();
        assert_eq!(reloaded.format_version, FORMAT_VERSION);
        assert_eq!(reloaded, loaded, "v2 resave of a migrated v1 must round-trip");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&resave_path);
    }

    /// Per-test scratch directory under the OS temp dir. We avoid the `tempfile`
    /// crate to keep dev-dependencies minimal; std::env::temp_dir() is plenty for
    /// the few small files these tests write.
    fn tempdir_for_test() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // PID + nanos-since-epoch keeps tests in the same binary from colliding.
        // Both std::process::id() and SystemTime are fine here (control side, not
        // the audio thread) and need no extra deps.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("daw-project-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).ok();
        p
    }
}
