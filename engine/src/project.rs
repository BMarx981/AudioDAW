//! Project file model + JSON I/O.
//!
//! This is the **canonical save/load shape** for a project. The Dart UI mirrors
//! its own per-track state and forwards parameter changes to the engine in real
//! time; on Save it pours that state into a [`ProjectFile`] and asks Rust to
//! write it; on Load Rust reads the JSON, hands the DTO back to Dart, and Dart
//! drives the engine's existing per-track setters to bring the audio side into
//! sync.
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
//! we have room to write a migration when [`FORMAT_VERSION`] bumps. v1 is
//! Milestone 5: tracks, master bus, EQ — nothing about clips' arrangement on a
//! timeline yet (that's Milestone 6).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dsp::FilterKind;

/// Schema version of the project JSON. Bump when the on-disk shape changes in
/// any way the previous version's deserializer can't handle.
pub const FORMAT_VERSION: u32 = 1;

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
    /// One entry per user-visible track, in display order. Each entry's slot in
    /// the engine's strip pool is the track's index in this list — adding a
    /// track appends here, removing a track takes that index out and shifts the
    /// rest down so engine slots are contiguous from 0.
    pub tracks: Vec<TrackState>,
    /// The master bus chain.
    pub master: MasterState,
}

impl ProjectFile {
    /// A blank "untitled" project: no tracks, a flat master bus.
    pub fn empty() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: "Untitled".into(),
            tracks: Vec::new(),
            master: MasterState::default_flat(),
        }
    }
}

/// One track's persistent state: identity, the WAV (if any) it loads, and the
/// parameters of its processing chain.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrackState {
    /// User-facing label ("Track 1", "Kick", …). The UI may auto-fill this on
    /// creation; serializing it lets a renamed track keep its name across loads.
    pub name: String,
    /// Absolute (or workspace-relative — Dart's choice) path to the WAV the
    /// track was last loaded with. `None` for an empty track. On load, missing
    /// files surface in the UI (the rest of the project still opens).
    pub clip_path: Option<String>,
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
    /// A new empty track with `name`, no clip loaded, and the same defaults the
    /// engine's [`crate::dsp::Chain`] starts at — unity gain, centered pan, flat
    /// EQ. Stays in sync with `Eq::new` and `Gain::new(0.0)` / `Pan::new(0.0)`.
    pub fn default_named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            clip_path: None,
            gain_db: 0.0,
            pan: 0.0,
            eq_bands: default_eq_bands(),
        }
    }
}

/// Master bus state. Same shape as a track minus identity and clip — the master
/// has its own EQ even though Milestone 4's UI doesn't expose it, so the
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

/// Read a project JSON file from `path`. Rejects files claiming a newer schema
/// than this build supports rather than risk silently misinterpreting them.
/// Older schemas (when we have them) would migrate here.
pub fn load_from_file(path: &Path) -> Result<ProjectFile, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let project: ProjectFile = serde_json::from_str(&json)
        .map_err(|e| format!("invalid project file at {}: {e}", path.display()))?;
    if project.format_version > FORMAT_VERSION {
        return Err(format!(
            "{} is from a newer format (v{}); this build supports up to v{}",
            path.display(),
            project.format_version,
            FORMAT_VERSION
        ));
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_project() -> ProjectFile {
        ProjectFile {
            format_version: FORMAT_VERSION,
            name: "Demo".into(),
            tracks: vec![
                TrackState {
                    name: "Drums".into(),
                    clip_path: Some("/tmp/drums.wav".into()),
                    gain_db: -3.0,
                    pan: -0.25,
                    eq_bands: default_eq_bands(),
                },
                TrackState {
                    name: "Bass".into(),
                    clip_path: None,
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
    /// silently misinterpreted as v1.
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
