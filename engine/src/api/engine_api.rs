//! The bridge API — the *only* surface Flutter sees.
//!
//! These functions run on whatever thread Dart calls them from (the platform /
//! UI thread), **never** the audio thread. They own the control side of the
//! engine: a single global `Engine` handle guarded by a mutex. Locking that
//! mutex here is fine — it's control-thread code, and the lock is held only long
//! enough to start/stop the engine or push one command.
//!
//! Design goals (from CLAUDE.md): the Dart side should feel like a normal Dart
//! API. `start`/`stop` are async (they can fail -> Dart gets a throwing
//! `Future`), while `set_frequency` is sync and fire-and-forget — it just pushes
//! to the lock-free ring and returns, so the slider can call it on every tick
//! without awaiting.

use std::sync::Mutex;
use std::time::Duration;

use flutter_rust_bridge::frb;

use crate::audio::Engine;
use crate::frb_generated::StreamSink;

/// How often the scope pump samples the engine and pushes a frame to Dart.
/// ~60 Hz so the trace updates once per display frame on a 60 Hz screen.
const SCOPE_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// The single running engine, or `None` when stopped.
///
/// `Mutex<Option<Engine>>` is `Sync` because `Engine` is `Send` (it holds only
/// `Send` things — the ring producer, an `Arc<AtomicBool>`, and a `JoinHandle`).
/// The audio `Stream` is *not* in here; it lives on the audio thread. So this
/// mutex is never taken on the audio thread, which keeps the realtime path
/// lock-free.
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// Start the audio engine and begin emitting a sine tone. Idempotent: calling it
/// while already running is a no-op. Returns an error (surfaced as a Dart
/// exception) if no audio device could be opened.
pub fn start_engine() -> Result<(), String> {
    let mut guard = lock()?;
    if guard.is_some() {
        return Ok(()); // already running
    }
    *guard = Some(Engine::start()?);
    Ok(())
}

/// Stop the engine. Idempotent. Dropping the `Engine` tears down the stream and
/// joins the audio thread (RAII), so there's nothing else to clean up.
pub fn stop_engine() -> Result<(), String> {
    *lock()? = None;
    Ok(())
}

/// Set the oscillator frequency in Hz. Fire-and-forget and synchronous: it pushes
/// one command to the lock-free ring and returns immediately, so it's safe to
/// call on every slider tick. No-op if the engine isn't running.
///
/// `frb(sync)` makes this a plain synchronous call on the Dart side (returns
/// `void`, not `Future`) — there's no reason to await a ring-buffer push.
#[frb(sync)]
pub fn set_frequency(hz: f32) {
    if let Ok(mut guard) = ENGINE.lock() {
        if let Some(engine) = guard.as_mut() {
            engine.set_frequency(hz);
        }
    }
}

/// Whether the engine is currently running. Handy for the UI to reflect state.
#[frb(sync)]
pub fn is_running() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Stream of oscilloscope frames for the UI to draw.
///
/// Each item is one trigger-aligned window of mono samples (`Float32List` in
/// Dart). Subscribe once; the returned Dart `Stream` stays live for the app's
/// lifetime, emitting an empty frame while stopped and real audio while playing.
///
/// ## How this stays off the audio thread
///
/// The audio callback only ever *pushes* samples into a lock-free ring (see
/// [`crate::scope`]) — wait-free, no allocation. This function spawns an
/// ordinary background thread (the "pump") that wakes ~60×/sec, briefly locks
/// the control-side engine, drains the ring into one window, and hands it to
/// Dart via `sink`. Nothing here runs on, or blocks, the realtime thread.
///
/// The pump exits when Dart cancels the subscription (`sink.add` returns `Err`).
pub fn scope_stream(sink: StreamSink<Vec<f32>>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    // Guard against more than one live pump fighting over the single ring
    // consumer (e.g. a Flutter hot-restart re-subscribing before the old pump
    // notices its sink closed). Only one pump runs; extra subscriptions end
    // immediately. The `Guard` resets the flag on the way out, including on the
    // `sink.add` error path.
    static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);

    if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
        return; // a pump is already running; let this subscription complete.
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
                // Lock only long enough to pull one frame. If the engine is
                // stopped, send an empty frame so the UI shows a flat line.
                let frame = match ENGINE.lock() {
                    Ok(mut guard) => match guard.as_mut() {
                        Some(engine) => engine.scope_frame(),
                        None => Vec::new(),
                    },
                    Err(_) => Vec::new(),
                };

                // `add` fails once Dart has cancelled the subscription — that's
                // our cue to stop the pump.
                if sink.add(frame).is_err() {
                    break;
                }
                std::thread::sleep(SCOPE_FRAME_INTERVAL);
            }
        });
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
