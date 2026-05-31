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
//! holds only `Send` things: ring-buffer producers/consumers, a stop flag, and a
//! couple of shared atomics. It never sees the `Stream` at all.
//!
//! cpal itself runs the audio *callback* on its own high-priority OS thread.
//! Our `daw-audio` thread doesn't process audio — it just keeps the stream
//! alive. All realtime work happens in `audio_callback`.
//!
//! ## Crossing the realtime boundary
//!
//! Three lock-free channels span the boundary, all `rtrb` SPSC rings:
//!   - **commands** (control → audio): small POD transport commands.
//!   - **clip hand-off** (control → audio): a freshly-decoded `Arc<AudioClip>`.
//!   - **clip retirement** (audio → control): clips the audio thread has
//!     displaced, sent back so the *control* thread does the deallocation.
//!
//! Plus a handful of atomics the audio thread writes and the UI reads: the
//! playhead position (in frames), whether playback is live, and the post-fader
//! peak level per channel. Writing an atomic is wait-free, so it's safe in the
//! callback. `f32` has no atomic type, so the meter levels travel as their raw
//! bits in an `AtomicU32` (`f32::to_bits` / `from_bits`).

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer};

use crate::clip::AudioClip;
use crate::commands::{command_channel, Command};
use crate::player::{clip_channel, retire_channel};
use crate::scope::{scope_channel, ScopeReader};
use crate::strip::Strip;

/// A handle to a running audio engine, owned by the control thread.
///
/// Dropping it stops the stream and joins the audio thread, so the engine is
/// strictly RAII — there is no way to leak the audio thread.
pub struct Engine {
    /// Producer half of the control→audio command ring. `Send`, not `Sync`; only
    /// ever touched from the control thread.
    commands: Producer<Command>,
    /// Producer half of the clip hand-off ring. We push a decoded clip here; the
    /// audio callback pops and swaps it in.
    clips: Producer<Arc<AudioClip>>,
    /// Consumer half of the retirement ring. The audio thread pushes clips it has
    /// displaced; we drain (and thereby drop) them here, off the audio thread.
    retired: Consumer<Arc<AudioClip>>,
    /// Output device sample rate (Hz), learned when the stream came up. Used to
    /// convert the playhead frame count into seconds for the UI.
    device_rate: f32,
    /// Current playhead position in clip frames, published by the audio thread.
    playhead: Arc<AtomicI64>,
    /// Whether playback is currently advancing, published by the audio thread.
    playing: Arc<AtomicBool>,
    /// Post-fader peak level per channel, as `f32` bits. Published by the audio
    /// thread each buffer, read by the meter pump on the control side.
    peak_l: Arc<AtomicU32>,
    peak_r: Arc<AtomicU32>,
    /// Signals the audio thread to tear down the stream and exit.
    stop: Arc<AtomicBool>,
    /// Joined on drop. `Option` so `Drop` can `take` it.
    thread: Option<JoinHandle<()>>,
    /// UI-side consumer of the oscilloscope tap. Drained from the control thread
    /// (never the audio thread) by [`Engine::scope_frame`].
    scope: ScopeReader,
}

impl Engine {
    /// Build the output stream and start it. Runs on the calling (control)
    /// thread and blocks only briefly, until the audio thread reports the stream
    /// is up (returning the device sample rate) or failed.
    pub fn start() -> Result<Self, String> {
        let (commands, command_rx) = command_channel();
        let (clips, clip_rx) = clip_channel();
        let (retire_tx, retired) = retire_channel();
        let (scope_tx, scope_rx) = scope_channel();
        let playhead = Arc::new(AtomicI64::new(0));
        let playing = Arc::new(AtomicBool::new(false));
        let peak_l = Arc::new(AtomicU32::new(0));
        let peak_r = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        // One-shot channel: the audio thread reports the device sample rate (or
        // an error) before `start` returns, so the caller learns about device
        // failures synchronously and gets the rate for frame<->seconds maths.
        let (ready_tx, ready_rx) = mpsc::channel::<Result<f32, String>>();

        let stop_for_thread = stop.clone();
        let playhead_for_thread = playhead.clone();
        let playing_for_thread = playing.clone();
        let peak_l_for_thread = peak_l.clone();
        let peak_r_for_thread = peak_r.clone();
        let thread = std::thread::Builder::new()
            .name("daw-audio".into())
            .spawn(move || {
                run_audio_thread(
                    command_rx,
                    clip_rx,
                    retire_tx,
                    scope_tx,
                    playhead_for_thread,
                    playing_for_thread,
                    peak_l_for_thread,
                    peak_r_for_thread,
                    stop_for_thread,
                    ready_tx,
                )
            })
            .map_err(|e| format!("failed to spawn audio thread: {e}"))?;

        match ready_rx.recv() {
            Ok(Ok(device_rate)) => Ok(Self {
                commands,
                clips,
                retired,
                device_rate,
                playhead,
                playing,
                peak_l,
                peak_r,
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

    /// Hand a freshly-decoded clip to the audio thread. Fire-and-forget: pushes
    /// the `Arc` onto the hand-off ring and returns. We first reclaim any retired
    /// clips so their memory is freed here, on the control thread.
    pub fn load_clip(&mut self, clip: Arc<AudioClip>) {
        self.collect_garbage();
        // Moving the Arc into the ring is a pointer copy — no refcount change. If
        // the ring is somehow full (many loads between two audio buffers), the
        // push fails and we simply drop this clip here, which is safe and means
        // the load is a no-op the user can retry.
        let _ = self.commands.push(Command::Stop); // rewind any current playback
        let _ = self.clips.push(clip);
    }

    /// Begin/resume playback. Fire-and-forget.
    pub fn play(&mut self) {
        let _ = self.commands.push(Command::Play);
    }

    /// Pause, holding position. Fire-and-forget.
    pub fn pause(&mut self) {
        let _ = self.commands.push(Command::Pause);
    }

    /// Stop and rewind to the start. Fire-and-forget.
    pub fn stop_playback(&mut self) {
        let _ = self.commands.push(Command::Stop);
    }

    /// Seek to `secs` from the clip start. Fire-and-forget.
    pub fn seek(&mut self, secs: f32) {
        let _ = self.commands.push(Command::Seek(secs));
    }

    /// Turn looping on/off. Fire-and-forget.
    pub fn set_looping(&mut self, on: bool) {
        let _ = self.commands.push(Command::SetLooping(on));
    }

    /// Set the strip gain target, in dB. Fire-and-forget; the value is clamped
    /// and smoothed on the audio thread.
    pub fn set_gain_db(&mut self, db: f32) {
        let _ = self.commands.push(Command::SetGainDb(db));
    }

    /// Set the strip gain target as a raw linear multiplier. Fire-and-forget.
    pub fn set_gain_linear(&mut self, linear: f32) {
        let _ = self.commands.push(Command::SetGainLinear(linear));
    }

    /// Set the strip pan target, in `[-1, 1]`. Fire-and-forget.
    pub fn set_pan(&mut self, pan: f32) {
        let _ = self.commands.push(Command::SetPan(pan));
    }

    /// Latest post-fader peak levels `(left, right)`, linear (0..≈1, can exceed
    /// 1 if boosted), as published by the audio thread.
    pub fn peak_levels(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak_l.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_r.load(Ordering::Relaxed)),
        )
    }

    /// Current playhead position in seconds, derived from the frame count the
    /// audio thread publishes.
    pub fn playhead_secs(&self) -> f64 {
        let frames = self.playhead.load(Ordering::Relaxed);
        if self.device_rate <= 0.0 {
            return 0.0;
        }
        frames as f64 / self.device_rate as f64
    }

    /// Whether playback is currently advancing.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Drain the retirement ring, dropping any clips the audio thread displaced.
    /// This is where displaced clip memory is actually freed — on the control
    /// thread, never the audio thread. Cheap to call often.
    pub fn collect_garbage(&mut self) {
        while self.retired.pop().is_ok() {
            // The popped `Arc` is dropped at the end of this iteration; if it was
            // the last reference, the buffer is freed here. Safe: control thread.
        }
    }

    /// Drain the oscilloscope tap and return one trigger-aligned window of recent
    /// samples for the UI to draw. Runs on the control thread.
    pub fn scope_frame(&mut self) -> Vec<f32> {
        self.scope.frame()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            // The audio thread is parked with a 100 ms timeout, so it notices the
            // stop flag promptly and drops the stream on its own thread.
            let _ = t.join();
        }
        // Reclaim anything still in the retirement ring so we don't leak on exit.
        self.collect_garbage();
    }
}

/// Body of the dedicated audio thread: build + play the stream, then park until
/// asked to stop. The `Stream` lives entirely within this function's scope, so
/// it is created and dropped on this one thread.
#[allow(clippy::too_many_arguments)]
fn run_audio_thread(
    command_rx: Consumer<Command>,
    clip_rx: Consumer<Arc<AudioClip>>,
    retire_tx: Producer<Arc<AudioClip>>,
    scope_tx: Producer<f32>,
    playhead: Arc<AtomicI64>,
    playing: Arc<AtomicBool>,
    peak_l: Arc<AtomicU32>,
    peak_r: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<f32, String>>,
) {
    let (stream, device_rate) = match build_stream(
        command_rx, clip_rx, retire_tx, scope_tx, playhead, playing, peak_l, peak_r,
    ) {
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
    let _ = ready_tx.send(Ok(device_rate));

    // Keep this thread — and therefore the stream — alive until stop is set. We
    // park rather than spin so the thread costs nothing while idle.
    while !stop.load(Ordering::Acquire) {
        std::thread::park_timeout(Duration::from_millis(100));
    }
    // `stream` drops here, on the same thread that built it. Good.
}

/// Open the default output device and build the f32 stream. All per-stream state
/// (the player) is allocated here, *before* the callback ever runs, then moved
/// into the callback closure. Nothing is allocated inside the callback itself.
/// Returns the stream and the device sample rate.
#[allow(clippy::too_many_arguments)]
fn build_stream(
    command_rx: Consumer<Command>,
    clip_rx: Consumer<Arc<AudioClip>>,
    retire_tx: Producer<Arc<AudioClip>>,
    scope_tx: Producer<f32>,
    playhead: Arc<AtomicI64>,
    playing: Arc<AtomicBool>,
    peak_l: Arc<AtomicU32>,
    peak_r: Arc<AtomicU32>,
) -> Result<(cpal::Stream, f32), String> {
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

    // Pre-allocate the strip now, on the spawning thread (this is where its
    // scratch buffers are allocated — never inside the callback). It is moved
    // into the callback below and never reallocated.
    let mut strip = Strip::new(sample_rate);
    let mut command_rx = command_rx;
    let mut clip_rx = clip_rx;
    let mut retire_tx = retire_tx;
    let mut scope_tx = scope_tx;

    let err_fn = |_err: cpal::StreamError| {
        // Fires only on stream-level errors (device unplugged, etc.), never per
        // buffer. We deliberately avoid `println!`/panics here.
    };

    // The starter only handles f32 output. The real engine will match I16/U16
    // too and convert at the boundary.
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    audio_callback(
                        data,
                        channels,
                        &mut strip,
                        &mut command_rx,
                        &mut clip_rx,
                        &mut retire_tx,
                        &mut scope_tx,
                        &playhead,
                        &playing,
                        &peak_l,
                        &peak_r,
                    );
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("failed to build f32 output stream: {e}"))?,
        other => return Err(format!("unsupported sample format: {other:?}")),
    };
    Ok((stream, sample_rate))
}

/// **THE REALTIME CALLBACK.** Runs on cpal's high-priority audio thread with a
/// hard per-buffer deadline. Everything here must be allocation-free, lock-free,
/// and panic-free:
///   - draining the rings with `pop()` is wait-free and never allocates
///   - swapping a clip is a move; the displaced clip is pushed to the retirement
///     ring (also wait-free) rather than dropped here
///   - `Strip::process` only does stack arithmetic + a wait-free scope push
///   - publishing the playhead/playing/peak state is a wait-free atomic store
#[inline]
#[allow(clippy::too_many_arguments)]
fn audio_callback(
    data: &mut [f32],
    channels: usize,
    strip: &mut Strip,
    command_rx: &mut Consumer<Command>,
    clip_rx: &mut Consumer<Arc<AudioClip>>,
    retire_tx: &mut Producer<Arc<AudioClip>>,
    scope_tx: &mut Producer<f32>,
    playhead: &AtomicI64,
    playing: &AtomicBool,
    peak_l: &AtomicU32,
    peak_r: &AtomicU32,
) {
    // 1. Apply any pending commands, in order (transport + gain/pan).
    while let Ok(cmd) = command_rx.pop() {
        strip.handle(cmd);
    }

    // 2. Swap in any newly-loaded clip. The clip we displace must NOT be dropped
    //    here (that would free memory on the audio thread); ship it back through
    //    the retirement ring for the control thread to drop.
    while let Ok(clip) = clip_rx.pop() {
        if let Some(old) = strip.set_clip(clip) {
            if let Err(rtrb::PushError::Full(old)) = retire_tx.push(old) {
                // Pathological: the control thread hasn't drained in a long time.
                // Leaking is realtime-safe (no deallocation); dropping here would
                // not be. With matched ring capacities this never happens.
                std::mem::forget(old);
            }
        }
    }

    // 3. Render the strip (source → gain → pan → output) and tap the scope.
    strip.process(data, channels, scope_tx);

    // 4. Publish playhead + transport + meter state for the UI. Wait-free atomic
    //    stores; the meter levels travel as `f32` bits.
    playhead.store(strip.pos_frames(), Ordering::Relaxed);
    playing.store(strip.is_playing(), Ordering::Relaxed);
    peak_l.store(strip.peak_left().to_bits(), Ordering::Relaxed);
    peak_r.store(strip.peak_right().to_bits(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::AudioClip;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn callback_does_not_allocate() {
        // Exercise the exact callback path (drain commands + clip ring + render +
        // publish) under assert_no_alloc. No real audio device is opened.
        let (mut cmd_tx, cmd_rx) = command_channel();
        let mut command_rx = cmd_rx;
        let (mut clip_tx, mut clip_rx) = clip_channel();
        let (mut retire_tx, mut retire_rx) = retire_channel();
        let (mut scope_tx, _scope_rx) = scope_channel();
        let playhead = AtomicI64::new(0);
        let playing = AtomicBool::new(false);
        let peak_l = AtomicU32::new(0);
        let peak_r = AtomicU32::new(0);
        let mut strip = Strip::new(48_000.0);
        let mut buf = [0.0_f32; 512];
        let channels = 2;

        // A clip to hand in (built on the control side, outside the guard).
        let clip = Arc::new(
            AudioClip::new(44_100.0, vec![vec![0.1; 20_000], vec![-0.1; 20_000]]).unwrap(),
        );
        let _ = clip_tx.push(clip);

        for i in 0..1000 {
            // Push commands from the "control" side, outside the guard — cover
            // transport *and* the new gain/pan parameter commands.
            let _ = cmd_tx.push(match i % 4 {
                0 => Command::Play,
                1 => Command::Seek((i % 10) as f32 * 0.01),
                2 => Command::SetGainDb(-(i % 24) as f32),
                _ => Command::SetPan(((i % 200) as f32 / 100.0) - 1.0),
            });
            assert_no_alloc(|| {
                audio_callback(
                    &mut buf,
                    channels,
                    &mut strip,
                    &mut command_rx,
                    &mut clip_rx,
                    &mut retire_tx,
                    &mut scope_tx,
                    &playhead,
                    &playing,
                    &peak_l,
                    &peak_r,
                );
            });
        }

        // Reclaim retired clips off the (pretend) audio thread.
        while retire_rx.pop().is_ok() {}
    }
}
