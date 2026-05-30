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
//! (the scope trace, the playhead) arrives via streams, not polling.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use flutter_rust_bridge::frb;

use crate::audio::Engine;
use crate::frb_generated::StreamSink;

/// How often the pump threads sample the engine and push to Dart. ~60 Hz for the
/// scope (one trace per display frame) and a slightly calmer ~30 Hz for the
/// playhead, which doesn't need to update faster than the eye notices.
const SCOPE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const STATUS_FRAME_INTERVAL: Duration = Duration::from_millis(33);

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
/// play/stop. Streamed ~30×/sec.
pub struct PlaybackStatus {
    /// Current playhead position, seconds from the clip start.
    pub position_secs: f64,
    /// Whether playback is currently advancing.
    pub playing: bool,
}

/// Load and decode a WAV from disk, hand it to the audio engine, and return its
/// metadata + waveform summary. Starts the engine (opens the audio device) if it
/// isn't already running. The clip is loaded stopped at the start — call
/// [`play`] to hear it.
///
/// Async on the Dart side (it does real file I/O and decoding); throws a Dart
/// exception if the file can't be read or decoded.
pub fn load_wav(path: String) -> Result<LoadedClip, String> {
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
        engine.load_clip(arc);
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

/// Begin or resume playback. Fire-and-forget; no-op if no clip is loaded.
#[frb(sync)]
pub fn play() {
    with_engine(|e| e.play());
}

/// Pause playback, holding the current position. Fire-and-forget.
#[frb(sync)]
pub fn pause() {
    with_engine(|e| e.pause());
}

/// Stop playback and rewind to the start. Fire-and-forget.
#[frb(sync)]
pub fn stop() {
    with_engine(|e| e.stop_playback());
}

/// Seek to `secs` from the clip start. Fire-and-forget; safe to call on every
/// scrub tick (the engine clamps to the clip bounds).
#[frb(sync)]
pub fn seek(secs: f32) {
    with_engine(|e| e.seek(secs));
}

/// Whether the audio engine is running (device open). Handy for the UI.
#[frb(sync)]
pub fn is_running() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Stream of oscilloscope frames for the UI to draw. Each item is one
/// trigger-aligned window of mono samples (`Float32List` in Dart) — now the live
/// output of the WAV player. Subscribe once; the stream stays live for the app's
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
