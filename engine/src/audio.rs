//! cpal stream setup and the realtime audio callback.
//!
//! ## Threading model (important — read this before touching it)
//!
//! `cpal::Stream` is **not `Send`** on macOS (the CoreAudio stream is tied to
//! the thread that built it). So we never move the stream across threads.
//! Instead `Engine::start` spawns one dedicated audio thread that *builds the
//! stream, plays it, and then parks* until asked to stop. The stream is created
//! and dropped on that single thread and never crosses a thread boundary.
//!
//! The control side (the `Engine` handle, used from the Dart-calling thread)
//! holds only `Send` things: the ring-buffer producer and a stop flag. It never
//! sees the `Stream` at all.
//!
//! cpal itself runs the audio *callback* on its own high-priority OS thread.
//! Our `daw-audio` thread doesn't process audio — it just keeps the stream
//! alive. All realtime work happens in `audio_callback`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer};

use crate::commands::{command_channel, Command};
use crate::osc::SineOsc;
use crate::scope::{scope_channel, ScopeReader};

/// Frequency the engine starts on before the UI sends its first value.
const DEFAULT_HZ: f32 = 440.0;

/// A handle to a running audio engine, owned by the control thread.
///
/// Dropping it stops the stream and joins the audio thread, so the engine is
/// strictly RAII — there is no way to leak the audio thread.
pub struct Engine {
    /// Producer half of the control→audio ring. `Send`, not `Sync`; only ever
    /// touched from the control thread (which is why a plain `&mut` is enough).
    producer: Producer<Command>,
    /// Signals the audio thread to tear down the stream and exit.
    stop: Arc<AtomicBool>,
    /// Joined on drop. `Option` so `Drop` can `take` it.
    thread: Option<JoinHandle<()>>,
    /// UI-side consumer of the oscilloscope tap. The matching producer was moved
    /// into the audio callback. Drained from the control thread (never the audio
    /// thread) by [`Engine::scope_frame`].
    scope: ScopeReader,
}

impl Engine {
    /// Build the output stream and start playing. Runs on the calling (control)
    /// thread and blocks only briefly, until the audio thread reports the
    /// stream is up (or failed). Returns an error string on any setup failure.
    pub fn start() -> Result<Self, String> {
        let (producer, consumer) = command_channel();
        // Audio->UI scope tap. The producer rides into the audio callback; the
        // reader stays here on the control side.
        let (scope_tx, scope_rx) = scope_channel();
        let stop = Arc::new(AtomicBool::new(false));

        // One-shot channel: the audio thread reports whether the stream came up
        // before `start` returns, so the caller learns about device errors.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let stop_for_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("daw-audio".into())
            .spawn(move || run_audio_thread(consumer, scope_tx, stop_for_thread, ready_tx))
            .map_err(|e| format!("failed to spawn audio thread: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                producer,
                stop,
                thread: Some(thread),
                scope: scope_rx,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("audio thread exited before signalling readiness".into()),
        }
    }

    /// Push a new target frequency to the audio thread. Fire-and-forget: if the
    /// ring is momentarily full we drop this update and the next one wins, which
    /// is exactly the right behaviour for a slider. Realtime-irrelevant — this
    /// runs on the control thread, never the audio thread.
    pub fn set_frequency(&mut self, hz: f32) {
        let _ = self.producer.push(Command::SetFrequency(hz));
    }

    /// Drain the oscilloscope tap and return one trigger-aligned window of recent
    /// samples for the UI to draw. Runs on the control thread (it allocates a
    /// small `Vec`); never call it from the audio thread.
    pub fn scope_frame(&mut self) -> Vec<f32> {
        self.scope.frame()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            // The audio thread is parked with a 100 ms timeout, so it notices
            // the stop flag promptly and drops the stream on its own thread.
            let _ = t.join();
        }
    }
}

/// Body of the dedicated audio thread: build + play the stream, then park until
/// asked to stop. The `Stream` lives entirely within this function's scope, so
/// it is created and dropped on this one thread.
fn run_audio_thread(
    consumer: Consumer<Command>,
    scope_tx: Producer<f32>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    let stream = match build_stream(consumer, scope_tx) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    if let Err(e) = stream.play().map_err(|e| e.to_string()) {
        let _ = ready_tx.send(Err(e));
        return;
    }
    let _ = ready_tx.send(Ok(()));

    // Keep this thread — and therefore the stream — alive until stop is set.
    // We park rather than spin so the thread costs nothing while idle.
    while !stop.load(Ordering::Acquire) {
        std::thread::park_timeout(Duration::from_millis(100));
    }
    // `stream` drops here, on the same thread that built it. Good.
}

/// Open the default output device and build the f32 stream. All per-stream
/// state (the oscillator) is allocated here, *before* the callback ever runs,
/// then moved into the callback closure. Nothing is allocated inside the
/// callback itself.
fn build_stream(
    consumer: Consumer<Command>,
    scope_tx: Producer<f32>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("no default output config: {e}"))?;

    // Read the rate from the device — do NOT hardcode 48 kHz. cpal picks the
    // device default, which may be 44.1/48/96 kHz or something unusual.
    let sample_rate = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();

    // Pre-allocate the oscillator now, on the spawning thread. It is moved into
    // the callback below and never reallocated.
    let mut osc = SineOsc::new(sample_rate, DEFAULT_HZ);
    let mut consumer = consumer;
    let mut scope_tx = scope_tx;

    let err_fn = |_err: cpal::StreamError| {
        // Fires only on stream-level errors (device unplugged, etc.), never per
        // buffer. We deliberately avoid `println!`/panics here; the real engine
        // will route this through a non-allocating error channel.
    };

    // The starter only handles f32 output. The real engine will match I16/U16
    // too and convert at the boundary.
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    audio_callback(data, channels, &mut osc, &mut consumer, &mut scope_tx);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("failed to build f32 output stream: {e}"))?,
        other => return Err(format!("unsupported sample format: {other:?}")),
    };
    Ok(stream)
}

/// **THE REALTIME CALLBACK.** Runs on cpal's high-priority audio thread with a
/// hard per-buffer deadline. Everything here must be allocation-free,
/// lock-free, and panic-free:
///   - draining the ring with `pop()` is wait-free and never allocates
///   - pushing to the scope ring with `push()` is wait-free and never allocates
///   - the oscillator only does stack arithmetic
///   - no `unwrap`, no `Mutex`, no `println!`
#[inline]
fn audio_callback(
    data: &mut [f32],
    channels: usize,
    osc: &mut SineOsc,
    consumer: &mut Consumer<Command>,
    scope_tx: &mut Producer<f32>,
) {
    // 1. Apply all pending control changes up front. For frequency the last one
    //    wins, which falls out naturally from applying them in order.
    while let Ok(cmd) = consumer.pop() {
        match cmd {
            Command::SetFrequency(hz) => osc.set_target_hz(hz),
        }
    }

    // 2. Render one mono sample per frame and fan it out to every channel of the
    //    interleaved output buffer (mono sine -> all speakers).
    for frame in data.chunks_mut(channels) {
        let s = osc.next_sample();
        for out in frame.iter_mut() {
            *out = s;
        }
        // 3. Tap the mono signal for the oscilloscope. Wait-free; if the UI has
        //    fallen behind and the ring is full, the sample is dropped rather
        //    than blocking the audio thread (`Err(Full)` ignored).
        let _ = scope_tx.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn callback_does_not_allocate() {
        // Exercise the exact callback path (drain ring + render) under
        // assert_no_alloc, including applying a command each iteration. No real
        // audio device is opened — we test the callback in isolation.
        let (mut tx, rx) = command_channel();
        let mut consumer = rx;
        // Scope producer is part of the realtime callback path now, so it must be
        // exercised under the no-alloc guard too.
        let (mut scope_tx, _scope_rx) = scope_channel();
        let mut osc = SineOsc::new(48_000.0, DEFAULT_HZ);
        let mut buf = [0.0_f32; 512]; // pre-allocated, stereo-interleavable
        let channels = 2;

        // Pushing to the ring happens on the "control" side, outside the guard.
        for i in 0..1000 {
            let _ = tx.push(Command::SetFrequency(200.0 + (i % 800) as f32));
            assert_no_alloc(|| {
                audio_callback(&mut buf, channels, &mut osc, &mut consumer, &mut scope_tx);
            });
        }
    }
}
