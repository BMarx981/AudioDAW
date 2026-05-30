//! [`WavPlayer`] — the realtime audio source for Milestone 1.
//!
//! This is the mirror of [`crate::osc::SineOsc`]: instead of synthesizing a
//! sine, it streams a pre-decoded [`AudioClip`] through the audio callback. As
//! with everything on the audio thread, every method here is **allocation-free,
//! lock-free, and panic-free**.
//!
//! ## Resampling, and why it lives in the callback
//!
//! The clip's sample rate (e.g. 44.1 kHz) often differs from the output
//! device's (e.g. 48 kHz). If we just read one stored sample per output frame,
//! the clip would play back at the wrong speed and pitch. So we keep a
//! **fractional** read position `pos` (in clip frames) and advance it by
//! `clip_rate / device_rate` each output frame, linearly interpolating between
//! the two nearest stored samples. This is realtime-safe (pure stack
//! arithmetic, no allocation) and handles any rate ratio. Linear interpolation
//! has a gentle high-frequency rolloff — fine for Milestone 1; a later milestone
//! can swap in a higher-quality resampler if it matters.
//!
//! ## Sharing the clip without dropping it on the audio thread
//!
//! The player holds `Option<Arc<AudioClip>>`. Receiving a new clip is a *move*
//! (the `Arc` is popped out of a ring — no refcount change). Replacing the old
//! clip yields the displaced `Arc`, which the caller (the audio callback) must
//! NOT drop here: dropping the last `Arc` frees the buffer, and freeing on the
//! audio thread is forbidden. So [`WavPlayer::set_clip`] *returns* the old `Arc`
//! and the callback ships it back to the control thread to be dropped. See
//! [`crate::audio`].

use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::clip::AudioClip;
use crate::commands::Command;

/// Realtime playback of a single [`AudioClip`].
pub struct WavPlayer {
    /// Output device sample rate, in Hz. Fixed for the life of the stream.
    device_rate: f32,
    /// The clip currently loaded, or `None` if nothing has been loaded yet.
    clip: Option<Arc<AudioClip>>,
    /// Fractional read position, in clip frames. Advancing by a non-integer step
    /// is what implements resampling.
    pos: f64,
    /// Whether we're actively advancing `pos` and emitting audio.
    playing: bool,
}

impl WavPlayer {
    /// Create a player for an output device running at `device_rate` Hz. Starts
    /// empty and stopped.
    pub fn new(device_rate: f32) -> Self {
        Self {
            device_rate,
            clip: None,
            pos: 0.0,
            playing: false,
        }
    }

    /// Swap in a new clip, returning the one it replaced (if any) **without
    /// dropping it** — the caller is responsible for disposing of it off the
    /// audio thread. Loading rewinds to the start and leaves playback stopped, so
    /// a freshly loaded file doesn't blast out immediately.
    ///
    /// Realtime-safe: `Option::replace` is a move; no allocation, no drop.
    #[inline]
    pub fn set_clip(&mut self, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.pos = 0.0;
        self.playing = false;
        self.clip.replace(clip)
    }

    /// Apply one transport command. Realtime-safe.
    #[inline]
    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                // Only makes sense with a clip loaded. If we're already at the
                // very end, restart from the top so Play always does something.
                if let Some(clip) = &self.clip {
                    if self.pos >= clip.frames as f64 - 1.0 {
                        self.pos = 0.0;
                    }
                    self.playing = true;
                }
            }
            Command::Pause => self.playing = false,
            Command::Stop => {
                self.playing = false;
                self.pos = 0.0;
            }
            Command::Seek(secs) => {
                if let Some(clip) = &self.clip {
                    // Seconds are relative to the *clip's* timeline, so convert
                    // with the clip's own rate, then clamp into bounds.
                    let target = secs.max(0.0) as f64 * clip.sample_rate as f64;
                    let last = (clip.frames as f64 - 1.0).max(0.0);
                    self.pos = target.min(last);
                }
            }
        }
    }

    /// Current playhead position in clip frames, rounded down. Published to the
    /// UI (as an atomic) so it can draw a moving playhead. Realtime-safe.
    #[inline]
    pub fn pos_frames(&self) -> i64 {
        self.pos as i64
    }

    /// Whether playback is currently advancing. Realtime-safe.
    #[inline]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Render one buffer of interleaved output and tap a mono copy into the
    /// oscilloscope ring. `out.len()` must be a multiple of `channels`.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free:
    /// indexing is bounds-checked but always in range by construction, the scope
    /// push is wait-free (drops on a full ring rather than blocking).
    #[inline]
    pub fn process(&mut self, out: &mut [f32], channels: usize, scope_tx: &mut Producer<f32>) {
        // Steps per output frame through the clip. >1 when the clip rate exceeds
        // the device rate (downsampling), <1 when it's lower (upsampling).
        let step = match &self.clip {
            Some(clip) if self.playing => clip.sample_rate as f64 / self.device_rate as f64,
            _ => 0.0,
        };

        for frame in out.chunks_mut(channels) {
            let mono = if step > 0.0 {
                // Safe: `step > 0.0` only when `self.clip` is `Some` and playing.
                let clip = match &self.clip {
                    Some(c) => c,
                    None => unreachable!(),
                };

                if self.pos >= clip.frames as f64 - 1.0 {
                    // Reached the end: stop, hold the playhead at the end, and
                    // emit silence from here on this buffer.
                    self.playing = false;
                    Self::write_silence(frame);
                    0.0
                } else {
                    let mono = Self::write_frame(clip, self.pos, channels, frame);
                    self.pos += step;
                    mono
                }
            } else {
                // No clip, or paused/stopped: output silence.
                Self::write_silence(frame);
                0.0
            };

            // Feed the scope the mono mix of what we just emitted (flat line when
            // silent). Wait-free; a full ring just drops the sample.
            let _ = scope_tx.push(mono);
        }
    }

    /// Write one interleaved output frame by reading the clip at fractional
    /// position `pos`, mapping clip channels onto the `channels` output channels.
    /// Returns the mono mix of the frame (for the scope). Realtime-safe.
    #[inline]
    fn write_frame(clip: &AudioClip, pos: f64, channels: usize, frame: &mut [f32]) -> f32 {
        let i = pos as usize; // floor; pos < frames-1 guaranteed by caller
        let frac = (pos - i as f64) as f32;

        let mut mono_acc = 0.0;
        for (c, out) in frame.iter_mut().enumerate() {
            // Mono clips broadcast to every output channel; multichannel clips
            // map channel-for-channel, clamping if the device has more channels
            // than the clip.
            let src = if clip.channels == 1 {
                0
            } else {
                c.min(clip.channels - 1)
            };
            let ch = &clip.data[src];
            // Linear interpolation between the two neighbouring stored samples.
            let s = ch[i] + (ch[i + 1] - ch[i]) * frac;
            *out = s;
            mono_acc += s;
        }
        mono_acc / channels as f32
    }

    #[inline]
    fn write_silence(frame: &mut [f32]) {
        for out in frame.iter_mut() {
            *out = 0.0;
        }
    }
}

/// Capacity of the clip hand-off and retirement rings, in clips. Loading is a
/// rare, human-driven action, so a handful of slots is plenty; the control
/// thread drains the retirement ring far faster than clips can pile up.
const CLIP_RING_CAPACITY: usize = 8;

/// Create the control→audio clip hand-off ring. The control thread pushes a
/// freshly-decoded `Arc<AudioClip>`; the audio callback pops it and swaps it in.
pub fn clip_channel() -> (Producer<Arc<AudioClip>>, Consumer<Arc<AudioClip>>) {
    RingBuffer::new(CLIP_RING_CAPACITY)
}

/// Create the audio→control retirement ring. The audio callback pushes clips it
/// has displaced (so it never drops them); the control thread drains and drops
/// them, which is where the actual deallocation safely happens.
pub fn retire_channel() -> (Producer<Arc<AudioClip>>, Consumer<Arc<AudioClip>>) {
    RingBuffer::new(CLIP_RING_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::scope_channel;
    use assert_no_alloc::assert_no_alloc;

    fn test_clip(rate: f32, channels: usize, frames: usize) -> Arc<AudioClip> {
        // A simple per-channel ramp so interpolation/position effects are visible.
        let data: Vec<Vec<f32>> = (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|f| ((f + c) as f32 / frames as f32) * 2.0 - 1.0)
                    .collect()
            })
            .collect();
        Arc::new(AudioClip::new(rate, data).unwrap())
    }

    #[test]
    fn silent_until_played() {
        let (mut scope_tx, _r) = scope_channel();
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 1, 1000));
        // Loaded but not playing -> silence.
        let mut buf = [1.0_f32; 256];
        player.process(&mut buf, 2, &mut scope_tx);
        assert!(
            buf.iter().all(|&s| s == 0.0),
            "loaded-but-paused must be silent"
        );
    }

    #[test]
    fn play_advances_and_emits_audio() {
        let (mut scope_tx, _r) = scope_channel();
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 2, 10_000));
        player.handle(Command::Play);

        let mut buf = [0.0_f32; 512]; // 256 stereo frames
        player.process(&mut buf, 2, &mut scope_tx);

        assert!(player.is_playing());
        assert_eq!(
            player.pos_frames(),
            256,
            "rate-matched playback advances 1:1"
        );
        // Some non-zero output was produced.
        assert!(buf.iter().any(|&s| s != 0.0), "playing should emit audio");
    }

    #[test]
    fn resamples_when_rates_differ() {
        // 44.1 kHz clip on a 48 kHz device: position advances by 44100/48000 per
        // output frame, i.e. slower than 1:1.
        let (mut scope_tx, _r) = scope_channel();
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(44_100.0, 1, 100_000));
        player.handle(Command::Play);

        let mut buf = [0.0_f32; 480]; // 480 mono frames
        player.process(&mut buf, 1, &mut scope_tx);

        let expected = (480.0 * 44_100.0 / 48_000.0) as i64; // ~441
        assert!(
            (player.pos_frames() - expected).abs() <= 1,
            "resampled position should be ~{expected}, got {}",
            player.pos_frames()
        );
    }

    #[test]
    fn stops_and_holds_at_end_of_clip() {
        let (mut scope_tx, _r) = scope_channel();
        let mut player = WavPlayer::new(48_000.0);
        let frames = 300;
        player.set_clip(test_clip(48_000.0, 1, frames));
        player.handle(Command::Play);

        // Render more frames than the clip has.
        let mut buf = [0.0_f32; 512];
        player.process(&mut buf, 1, &mut scope_tx);

        assert!(!player.is_playing(), "playback should stop at the clip end");
        assert!(player.pos_frames() <= frames as i64);
    }

    #[test]
    fn seek_and_stop_move_the_playhead() {
        let (mut scope_tx, _r) = scope_channel();
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 1, 48_000)); // exactly 1 second

        player.handle(Command::Seek(0.5)); // half a second in
        assert!((player.pos_frames() - 24_000).abs() <= 1);

        // Seeking past the end clamps to the last frame.
        player.handle(Command::Seek(999.0));
        assert!(player.pos_frames() <= 48_000);

        player.handle(Command::Stop);
        assert_eq!(player.pos_frames(), 0, "stop rewinds to the start");

        let _ = &mut scope_tx; // silence unused-mut on some toolchains
    }

    #[test]
    fn playback_path_does_not_allocate() {
        // The realtime-safety contract from TESTING.md: exercise the exact
        // callback path — command handling, clip swap via the rings, and
        // rendering — all under assert_no_alloc.
        let (mut scope_tx, _r) = scope_channel();
        let (mut clip_tx, mut clip_rx) = clip_channel();
        let (mut retire_tx, mut retire_rx) = retire_channel();
        let mut player = WavPlayer::new(48_000.0);
        let mut buf = [0.0_f32; 512];

        // Pre-build clips on the control side (allocating here is fine).
        let clips: Vec<Arc<AudioClip>> = (0..4).map(|_| test_clip(44_100.0, 2, 20_000)).collect();
        let mut clip_iter = clips.iter().cloned();

        assert_no_alloc(|| {
            for i in 0..1000 {
                // Occasionally hand in a new clip via the ring, exactly as the
                // control thread would.
                if i % 250 == 0 {
                    if let Some(c) = clip_iter.next() {
                        let _ = clip_tx.push(c);
                    }
                }
                // Drain the clip ring and swap, retiring the displaced clip
                // without dropping it on this (pretend-audio) thread.
                while let Ok(c) = clip_rx.pop() {
                    if let Some(old) = player.set_clip(c) {
                        // If the retire ring were full we'd leak rather than drop
                        // here; with matched capacities it never fills.
                        if let Err(rtrb::PushError::Full(old)) = retire_tx.push(old) {
                            std::mem::forget(old);
                        }
                    }
                }
                player.handle(Command::Play);
                player.process(&mut buf, 2, &mut scope_tx);
            }
        });

        // Draining retired clips (and thus dropping them) happens off the audio
        // thread — here, after the guarded section.
        while retire_rx.pop().is_ok() {}
    }
}
