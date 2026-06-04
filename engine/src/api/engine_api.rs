//! The bridge API — the *only* surface Flutter sees.
//!
//! These functions run on whatever thread Dart calls them from (an FRB worker or
//! the platform thread), **never** the audio thread. They own the control side
//! of the engine: a single global `Engine` handle guarded by a mutex. Locking
//! that mutex here is fine — it's control-thread code, and the lock is held only
//! long enough to start/stop the engine or push one command.
//!
//! Design goals (from CLAUDE.md): the Dart side should feel like a normal Dart
//! API. `load_wav` is async (it does file I/O and decoding, and can fail → Dart
//! gets a throwing `Future`). The transport calls (`play`/`pause`/`stop`/`seek`)
//! are sync and fire-and-forget — they push one POD command to the lock-free
//! ring and return, so the UI never awaits a transport tap. Continuous data
//! (the scope trace, the playhead, the per-track + master meters) arrives via
//! streams, not polling.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use flutter_rust_bridge::frb;

use crate::audio::Engine;
use crate::frb_generated::StreamSink;
use crate::mixer::MAX_TRACKS;
use crate::sampler::MAX_CLIPS_PER_TRACK;
// Re-export the project DTO types from the bridge module so flutter_rust_bridge
// mirrors them to Dart classes. We don't *implement* serde here — that stays in
// `crate::project` — but FRB only scans `crate::api`, so the public types it
// needs must be reachable from this module's exports.
pub use crate::project::{EqBandState, MasterState, ProjectFile, TrackClipState, TrackState};

/// How often the pump threads sample the engine and push to Dart. ~60 Hz for the
/// scope and meters (one tick per display frame) and a slightly calmer ~30 Hz
/// for the playhead, which doesn't need to update faster than the eye notices.
const SCOPE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const STATUS_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const METER_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Number of min/max columns we summarize a clip's waveform into at load. Plenty
/// for any realistic window width; the Flutter painter sub-samples to fit.
const WAVEFORM_BUCKETS: usize = 2000;

/// The single running engine, or `None` when stopped.
///
/// `Mutex<Option<Engine>>` is `Sync` because `Engine` is `Send` (it holds only
/// `Send` things — ring producers/consumers, `Arc<Atomic*>`, a `JoinHandle`).
/// The audio `Stream` is *not* in here; it lives on the audio thread. So this
/// mutex is never taken on the audio thread, which keeps the realtime path
/// lock-free.
static ENGINE: std::sync::Mutex<Option<Engine>> = std::sync::Mutex::new(None);

/// What `load_wav` hands back to Dart: enough metadata to label the clip, plus a
/// precomputed min/max waveform summary for the display painter. Computing the
/// waveform once at load (not per frame) is the whole point — the UI just draws
/// these arrays. FRB mirrors this to a Dart class with `Float32List` fields.
pub struct LoadedClip {
    /// File sample rate, Hz.
    pub sample_rate: f32,
    /// Channel count (1 = mono, 2 = stereo).
    pub channels: u32,
    /// Length in frames (samples per channel).
    pub frames: u64,
    /// Length in seconds.
    pub duration_secs: f64,
    /// Per-column minimum of the mono-mixed signal, in `[-1, 1]`.
    pub waveform_min: Vec<f32>,
    /// Per-column maximum of the mono-mixed signal, in `[-1, 1]`.
    pub waveform_max: Vec<f32>,
}

/// A snapshot of transport state for the UI to animate the playhead and reflect
/// play/stop. Transport is global — one playhead drives every track. Streamed
/// ~30×/sec.
pub struct PlaybackStatus {
    /// Current playhead position, seconds from the clip start.
    pub position_secs: f64,
    /// Whether playback is currently advancing.
    pub playing: bool,
}

/// One snapshot of every meter in the mixer: per-track L/R post-fader peaks
/// (length [`max_tracks`]), plus the master bus L/R post-fader peak. All values
/// are linear amplitude (0..≈1, may exceed 1 if boosted). The UI maps these to
/// its own dB-scaled meter widgets.
pub struct MixerMeters {
    /// Per-track left-channel peaks, one entry per strip slot.
    pub track_peaks_l: Vec<f32>,
    /// Per-track right-channel peaks, same length as [`track_peaks_l`].
    pub track_peaks_r: Vec<f32>,
    /// Master bus left-channel peak.
    pub master_peak_l: f32,
    /// Master bus right-channel peak.
    pub master_peak_r: f32,
}

/// The mixer's track-pool size. Exposed so the Dart side can size per-track
/// state arrays without hard-coding the constant in two places.
#[frb(sync)]
pub fn max_tracks() -> u32 {
    MAX_TRACKS as u32
}

/// The per-track clip-slot pool size — the most timeline clips one track can
/// hold simultaneously. Exposed for the same reason as [`max_tracks`].
#[frb(sync)]
pub fn max_clips_per_track() -> u32 {
    MAX_CLIPS_PER_TRACK as u32
}

/// Load and decode a WAV from disk, hand it to `track` in the audio engine, and
/// return its metadata + waveform summary. Starts the engine (opens the audio
/// device) if it isn't already running. The clip loads stopped at the start —
/// call [`play`] to hear it.
///
/// Async on the Dart side (it does real file I/O and decoding); throws a Dart
/// exception if the file can't be read or decoded.
pub fn load_wav(track: u32, path: String) -> Result<LoadedClip, String> {
    // Decode first, holding no lock during the slow part.
    let clip = crate::decode::decode_wav(Path::new(&path))?;

    let sample_rate = clip.sample_rate;
    let channels = clip.channels as u32;
    let frames = clip.frames as u64;
    let duration_secs = clip.duration_secs();
    // Summarize before moving the clip into the Arc.
    let waveform = clip.waveform(WAVEFORM_BUCKETS);
    let arc = Arc::new(clip);

    // Now take the lock just long enough to (lazily start and) hand off the clip.
    let mut guard = lock()?;
    if guard.is_none() {
        *guard = Some(Engine::start()?);
    }
    if let Some(engine) = guard.as_mut() {
        engine.load_clip(track.min(u8::MAX as u32) as u8, arc);
    }

    Ok(LoadedClip {
        sample_rate,
        channels,
        frames,
        duration_secs,
        waveform_min: waveform.min,
        waveform_max: waveform.max,
    })
}

/// Begin or resume playback on every track. Fire-and-forget.
#[frb(sync)]
pub fn play() {
    with_engine(|e| e.play());
}

/// Pause playback, holding the current position, on every track. Fire-and-forget.
#[frb(sync)]
pub fn pause() {
    with_engine(|e| e.pause());
}

/// Stop playback and rewind every track to the start. Fire-and-forget.
#[frb(sync)]
pub fn stop() {
    with_engine(|e| e.stop_playback());
}

/// Seek every track to `secs` from its clip start. Fire-and-forget.
#[frb(sync)]
pub fn seek(secs: f32) {
    with_engine(|e| e.seek(secs));
}

/// Turn looping on/off on every track. Fire-and-forget.
#[frb(sync)]
pub fn set_looping(looping: bool) {
    with_engine(|e| e.set_looping(looping));
}

/// Set track `t`'s gain in dB. Fire-and-forget; clamped + smoothed on audio.
#[frb(sync)]
pub fn set_track_gain_db(track: u32, db: f32) {
    with_engine(|e| e.set_track_gain_db(track as u8, db));
}

/// Set track `t`'s gain as a raw linear multiplier. Fire-and-forget; clamped + smoothed.
#[frb(sync)]
pub fn set_track_gain_linear(track: u32, linear: f32) {
    with_engine(|e| e.set_track_gain_linear(track as u8, linear));
}

/// Set track `t`'s pan in `[-1, 1]`. Fire-and-forget; clamped + smoothed.
#[frb(sync)]
pub fn set_track_pan(track: u32, pan: f32) {
    with_engine(|e| e.set_track_pan(track as u8, pan));
}

/// Set track `t`'s EQ band filter kind, by integer code. Fire-and-forget.
#[frb(sync)]
pub fn set_track_eq_band_kind(track: u32, band: u32, kind: u32) {
    with_engine(|e| e.set_track_eq_band_kind(track as u8, band as u8, kind));
}

/// Set track `t`'s EQ band frequency in Hz. Fire-and-forget; smoothed.
#[frb(sync)]
pub fn set_track_eq_band_freq(track: u32, band: u32, hz: f32) {
    with_engine(|e| e.set_track_eq_band_freq(track as u8, band as u8, hz));
}

/// Set track `t`'s EQ band Q. Fire-and-forget; smoothed.
#[frb(sync)]
pub fn set_track_eq_band_q(track: u32, band: u32, q: f32) {
    with_engine(|e| e.set_track_eq_band_q(track as u8, band as u8, q));
}

/// Set track `t`'s EQ band gain in dB. Fire-and-forget; smoothed.
#[frb(sync)]
pub fn set_track_eq_band_gain_db(track: u32, band: u32, db: f32) {
    with_engine(|e| e.set_track_eq_band_gain_db(track as u8, band as u8, db));
}

/// Enable/disable track `t`'s EQ band. Fire-and-forget.
#[frb(sync)]
pub fn set_track_eq_band_enabled(track: u32, band: u32, on: bool) {
    with_engine(|e| e.set_track_eq_band_enabled(track as u8, band as u8, on));
}

/// Set the master bus gain in dB. Fire-and-forget; clamped + smoothed.
#[frb(sync)]
pub fn set_master_gain_db(db: f32) {
    with_engine(|e| e.set_master_gain_db(db));
}

/// Set the master bus gain as a raw linear multiplier. Fire-and-forget.
#[frb(sync)]
pub fn set_master_gain_linear(linear: f32) {
    with_engine(|e| e.set_master_gain_linear(linear));
}

/// Set the master bus pan in `[-1, 1]`. Fire-and-forget; clamped + smoothed.
#[frb(sync)]
pub fn set_master_pan(pan: f32) {
    with_engine(|e| e.set_master_pan(pan));
}

/// Drop the clip on `track` and reset its strip to defaults. Fire-and-forget —
/// the displaced clip retires on the audio→control ring and is freed off the
/// audio thread. Used by the UI's "remove track" action.
#[frb(sync)]
pub fn clear_track(track: u32) {
    with_engine(|e| e.clear_track(track.min(u8::MAX as u32) as u8));
}

/// Load + decode a WAV from disk and **place** it in `(track, slot)` at the
/// given timeline position. Unlike [`load_wav`], does not stop the transport —
/// the new clip starts contributing as soon as the playhead crosses
/// `start_frame`. Async; throws a Dart exception if decode/file I/O fails.
///
/// `length_frames == 0` is interpreted as "use the source's full length", so
/// the simplest Dart caller (drop a WAV on the timeline at frame N) can pass 0.
pub fn place_clip_on_track(
    track: u32,
    slot: u32,
    path: String,
    start_frame: i64,
    length_frames: u32,
    source_offset_frames: u32,
) -> Result<LoadedClip, String> {
    let clip = crate::decode::decode_wav(Path::new(&path))?;

    let sample_rate = clip.sample_rate;
    let channels = clip.channels as u32;
    let frames = clip.frames as u64;
    let duration_secs = clip.duration_secs();
    let waveform = clip.waveform(WAVEFORM_BUCKETS);
    // Resolve the convenience "0 = whole source" before moving the clip.
    let effective_length = if length_frames == 0 {
        clip.frames as u32
    } else {
        length_frames
    };
    let arc = Arc::new(clip);

    let mut guard = lock()?;
    if guard.is_none() {
        *guard = Some(Engine::start()?);
    }
    if let Some(engine) = guard.as_mut() {
        engine.place_clip(
            track.min(u8::MAX as u32) as u8,
            slot.min(u8::MAX as u32) as u8,
            arc,
            start_frame,
            effective_length,
            source_offset_frames,
        );
    }

    Ok(LoadedClip {
        sample_rate,
        channels,
        frames,
        duration_secs,
        waveform_min: waveform.min,
        waveform_max: waveform.max,
    })
}

/// Move an already-placed clip to a new timeline position. Fire-and-forget.
#[frb(sync)]
pub fn move_clip(track: u32, slot: u32, start_frame: i64) {
    with_engine(|e| {
        e.move_clip(
            track.min(u8::MAX as u32) as u8,
            slot.min(u8::MAX as u32) as u8,
            start_frame,
        )
    });
}

/// Resize an already-placed clip on the timeline. Fire-and-forget.
#[frb(sync)]
pub fn resize_clip(track: u32, slot: u32, length_frames: u32) {
    with_engine(|e| {
        e.resize_clip(
            track.min(u8::MAX as u32) as u8,
            slot.min(u8::MAX as u32) as u8,
            length_frames,
        )
    });
}

/// Shift where in the source an already-placed clip starts reading.
/// Fire-and-forget.
#[frb(sync)]
pub fn set_clip_source_offset(track: u32, slot: u32, source_offset_frames: u32) {
    with_engine(|e| {
        e.set_clip_source_offset(
            track.min(u8::MAX as u32) as u8,
            slot.min(u8::MAX as u32) as u8,
            source_offset_frames,
        )
    });
}

/// Remove one placed clip from a track. The displaced source clip retires on
/// the audio→control ring and is freed off the audio thread. Fire-and-forget.
#[frb(sync)]
pub fn remove_clip(track: u32, slot: u32) {
    with_engine(|e| {
        e.remove_clip(
            track.min(u8::MAX as u32) as u8,
            slot.min(u8::MAX as u32) as u8,
        )
    });
}

/// Write `project` to `path` as JSON. Throws on the Dart side if the file
/// can't be written (path doesn't exist, permission denied, etc.) — the
/// Result→Future mapping turns the error string into a Dart exception. Runs
/// off the audio thread (file I/O).
pub fn save_project(path: String, project: ProjectFile) -> Result<(), String> {
    crate::project::save_to_file(Path::new(&path), &project)
}

/// Read a project from `path`. Returns the DTO; the Dart side then loads each
/// referenced WAV (best-effort) and pushes parameter setters to bring the
/// engine into sync. Throws on the Dart side if the file is missing, malformed,
/// or claims a newer schema than this build supports. Runs off the audio thread.
pub fn load_project(path: String) -> Result<ProjectFile, String> {
    crate::project::load_from_file(Path::new(&path))
}

/// The output sample rate (Hz) the engine is running at, or 48000 if it hasn't
/// started yet. The UI draws the EQ response curve at this rate so it matches
/// what the audio thread actually filters with.
#[frb(sync)]
pub fn engine_sample_rate() -> f32 {
    if let Ok(guard) = ENGINE.lock() {
        if let Some(e) = guard.as_ref() {
            return e.sample_rate();
        }
    }
    48_000.0
}

/// Whether the audio engine is running (device open). Handy for the UI.
#[frb(sync)]
pub fn is_running() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Stream of oscilloscope frames for the UI to draw. Each item is one
/// trigger-aligned window of mono samples (`Float32List` in Dart) — now the live
/// output of the master mix. Subscribe once; the stream stays live for the app's
/// lifetime, emitting an empty frame while stopped and real audio while playing.
///
/// The audio callback only ever *pushes* samples into a lock-free ring; this
/// spawns a background pump that drains it ~60×/sec and hands frames to Dart.
/// Nothing here runs on, or blocks, the realtime thread.
pub fn scope_stream(sink: StreamSink<Vec<f32>>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Guard against more than one live pump fighting over the single ring
    // consumer (e.g. a Flutter hot-restart re-subscribing before the old pump
    // notices its sink closed). Only one pump runs; extra subscriptions end
    // immediately.
    static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);
    if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }

    let _ = std::thread::Builder::new()
        .name("daw-scope-pump".into())
        .spawn(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    PUMP_RUNNING.store(false, Ordering::Release);
                }
            }
            let _guard = Guard;

            loop {
                let frame = match ENGINE.lock() {
                    Ok(mut guard) => match guard.as_mut() {
                        Some(engine) => engine.scope_frame(),
                        None => Vec::new(),
                    },
                    Err(_) => Vec::new(),
                };
                if sink.add(frame).is_err() {
                    break; // Dart cancelled the subscription.
                }
                std::thread::sleep(SCOPE_FRAME_INTERVAL);
            }
        });
}

/// Stream of [`PlaybackStatus`] snapshots so the UI can animate the playhead and
/// reflect play/stop without polling. Also the convenient place to reclaim
/// retired clips: this pump runs on a control thread and locks the engine ~30×/s
/// anyway, so it drains the retirement ring each tick.
pub fn playback_status_stream(sink: StreamSink<PlaybackStatus>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);
    if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }

    let _ = std::thread::Builder::new()
        .name("daw-status-pump".into())
        .spawn(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    PUMP_RUNNING.store(false, Ordering::Release);
                }
            }
            let _guard = Guard;

            loop {
                let status = match ENGINE.lock() {
                    Ok(mut guard) => match guard.as_mut() {
                        Some(engine) => {
                            engine.collect_garbage(); // free displaced clips here
                            PlaybackStatus {
                                position_secs: engine.playhead_secs(),
                                playing: engine.is_playing(),
                            }
                        }
                        None => PlaybackStatus {
                            position_secs: 0.0,
                            playing: false,
                        },
                    },
                    Err(_) => PlaybackStatus {
                        position_secs: 0.0,
                        playing: false,
                    },
                };
                if sink.add(status).is_err() {
                    break;
                }
                std::thread::sleep(STATUS_FRAME_INTERVAL);
            }
        });
}

/// Stream of post-fader [`MixerMeters`] (~60 Hz). Each event carries the peak
/// L/R for every track in the pool plus the master bus, so the UI can drive a
/// per-strip meter from one subscription.
///
/// As with the other pumps, nothing here runs on or blocks the audio thread —
/// reading an atomic is wait-free, and the lock taken is the control-side engine
/// mutex (never touched by the realtime callback).
pub fn meter_stream(sink: StreamSink<MixerMeters>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);
    if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }

    let _ = std::thread::Builder::new()
        .name("daw-meter-pump".into())
        .spawn(move || {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    PUMP_RUNNING.store(false, Ordering::Release);
                }
            }
            let _guard = Guard;

            loop {
                let meters = match ENGINE.lock() {
                    Ok(guard) => match guard.as_ref() {
                        Some(engine) => {
                            let mut l = Vec::with_capacity(MAX_TRACKS);
                            let mut r = Vec::with_capacity(MAX_TRACKS);
                            for t in 0..MAX_TRACKS {
                                let (tl, tr) = engine.track_peak_levels(t);
                                l.push(tl);
                                r.push(tr);
                            }
                            let (ml, mr) = engine.master_peak_levels();
                            MixerMeters {
                                track_peaks_l: l,
                                track_peaks_r: r,
                                master_peak_l: ml,
                                master_peak_r: mr,
                            }
                        }
                        None => MixerMeters {
                            track_peaks_l: vec![0.0; MAX_TRACKS],
                            track_peaks_r: vec![0.0; MAX_TRACKS],
                            master_peak_l: 0.0,
                            master_peak_r: 0.0,
                        },
                    },
                    Err(_) => MixerMeters {
                        track_peaks_l: vec![0.0; MAX_TRACKS],
                        track_peaks_r: vec![0.0; MAX_TRACKS],
                        master_peak_l: 0.0,
                        master_peak_r: 0.0,
                    },
                };
                if sink.add(meters).is_err() {
                    break;
                }
                std::thread::sleep(METER_FRAME_INTERVAL);
            }
        });
}

/// Run `f` against the running engine, if any. No-op when stopped. Keeps the
/// transport functions to one line each and the poisoned-mutex handling in one
/// place.
fn with_engine(f: impl FnOnce(&mut Engine)) {
    if let Ok(mut guard) = ENGINE.lock() {
        if let Some(engine) = guard.as_mut() {
            f(engine);
        }
    }
}

/// Lock the global engine, converting a poisoned mutex into a plain error string
/// rather than panicking across the FFI boundary.
fn lock() -> Result<std::sync::MutexGuard<'static, Option<Engine>>, String> {
    ENGINE
        .lock()
        .map_err(|_| "engine state was poisoned by an earlier panic".to_string())
}

/// flutter_rust_bridge initialization hook. Called once from Dart at startup.
#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}
