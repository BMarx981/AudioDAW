//! [`WavPlayer`] — the realtime audio source.
//!
//! Streams a pre-decoded [`AudioClip`] through the audio callback. As with
//! everything on the audio thread, every method here is **allocation-free,
//! lock-free, and panic-free**.
//!
//! ## Planar output (changed in Milestone 2)
//!
//! The player renders into **planar** buffers — one `&mut [f32]` per channel,
//! deinterleaved — rather than writing interleaved device frames directly. This
//! is the engine's internal format (CLAUDE.md: "f32, deinterleaved internally")
//! and it's what lets the DSP units ([`crate::dsp`]) process the signal before
//! it's interleaved onto the device at the very end (see [`crate::strip`]). The
//! internal bus is **stereo**: a mono clip is duplicated to both sides, a stereo
//! clip maps channel-for-channel, and any extra clip channels are ignored.
//!
//! ## Resampling, and why it lives in the callback
//!
//! The clip's sample rate (e.g. 44.1 kHz) often differs from the output
//! device's (e.g. 48 kHz). We keep a **fractional** read position `pos` (in clip
//! frames) and advance it by `clip_rate / device_rate` each output frame,
//! linearly interpolating between the two nearest stored samples. Realtime-safe
//! (pure stack arithmetic) and handles any rate ratio. Linear interpolation has
//! a gentle high-frequency rolloff — fine here; a later milestone can swap in a
//! higher-quality resampler.
//!
//! ## Sharing the clip without dropping it on the audio thread
//!
//! The player holds `Option<Arc<AudioClip>>`. Receiving a new clip is a *move*
//! (popped out of a ring — no refcount change). Replacing the old clip yields
//! the displaced `Arc`, which the caller must NOT drop here (dropping the last
//! `Arc` frees the buffer, and freeing on the audio thread is forbidden). So
//! [`WavPlayer::set_clip`] *returns* the old `Arc` and the callback ships it back
//! to the control thread to be dropped. See [`crate::audio`].

use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::clip::AudioClip;
use crate::commands::Command;

/// Realtime playback of a single [`AudioClip`], rendered as planar stereo.
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
    /// When true, wrap back to the start at the clip end instead of stopping.
    looping: bool,
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
            looping: false,
        }
    }

    /// Swap in a new clip, returning the one it replaced (if any) **without
    /// dropping it** — the caller disposes of it off the audio thread. Loading
    /// rewinds to the start and leaves playback stopped, so a freshly loaded file
    /// doesn't blast out immediately.
    ///
    /// Realtime-safe: `Option::replace` is a move; no allocation, no drop.
    #[inline]
    pub fn set_clip(&mut self, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.pos = 0.0;
        self.playing = false;
        self.clip.replace(clip)
    }

    /// Drop the current clip (without freeing it — handed back for the control
    /// thread to dispose of, same contract as [`Self::set_clip`]), reset
    /// position/playing/looping, and return to a freshly-constructed state.
    /// Used by [`crate::strip::Strip::clear`] when a track is removed.
    #[inline]
    pub fn clear(&mut self) -> Option<Arc<AudioClip>> {
        self.pos = 0.0;
        self.playing = false;
        self.looping = false;
        self.clip.take()
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
            Command::SetLooping(on) => self.looping = on,
            // Gain/pan/EQ commands (per-track or master), plus `ClearTrack`,
            // are not the player's concern; the mixer (or, for `ClearTrack`,
            // the audio callback) routes them. Ignored here.
            Command::ClearTrack(_)
            | Command::SetTrackGainDb(..)
            | Command::SetTrackGainLinear(..)
            | Command::SetTrackPan(..)
            | Command::SetTrackEqBandKind(..)
            | Command::SetTrackEqBandFreq(..)
            | Command::SetTrackEqBandQ(..)
            | Command::SetTrackEqBandGainDb(..)
            | Command::SetTrackEqBandEnabled(..)
            | Command::SetMasterGainDb(_)
            | Command::SetMasterGainLinear(_)
            | Command::SetMasterPan(_) => {}
        }
    }

    /// Current playhead position in clip frames, rounded down. Realtime-safe.
    #[inline]
    pub fn pos_frames(&self) -> i64 {
        self.pos as i64
    }

    /// Whether playback is currently advancing. Realtime-safe.
    #[inline]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Render one block of planar stereo into `left`/`right` (which must be the
    /// same length — the block's frame count). Fills with silence when stopped,
    /// paused, or past the clip end.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free:
    /// indexing is bounds-checked but always in range by construction.
    #[inline]
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());

        // Steps per output frame through the clip. >1 when downsampling, <1 when
        // upsampling. 0.0 means "emit silence" (no clip, or paused/stopped).
        let step = match &self.clip {
            Some(clip) if self.playing => clip.sample_rate as f64 / self.device_rate as f64,
            _ => 0.0,
        };

        for f in 0..frames {
            if step <= 0.0 {
                // No clip, or paused/stopped.
                left[f] = 0.0;
                right[f] = 0.0;
                continue;
            }
            // Safe: `step > 0.0` only when `self.clip` is `Some` and playing.
            let clip = match &self.clip {
                Some(c) => c,
                None => unreachable!(),
            };
            let last = clip.frames as f64 - 1.0;

            if self.pos >= last {
                if self.looping && clip.frames > 1 {
                    // Wrap back toward the start, keeping the fractional
                    // overshoot so the resampler stays continuous across the
                    // loop seam. Loop length is (frames-1) so the wrapped
                    // position lands back in the interpolatable range.
                    self.pos -= last;
                    if self.pos >= last {
                        // Pathologically short clip vs. a big step: never leave
                        // `pos` out of range (read_stereo reads pos and pos+1).
                        self.pos = 0.0;
                    }
                } else {
                    // Reached the end: stop, hold the playhead, emit silence for
                    // the rest of this block (and every later one until replayed).
                    self.playing = false;
                    left[f] = 0.0;
                    right[f] = 0.0;
                    continue;
                }
            }

            let (l, r) = Self::read_stereo(clip, self.pos);
            left[f] = l;
            right[f] = r;
            self.pos += step;
        }
    }

    /// Read the clip at fractional position `pos` as a stereo pair, linearly
    /// interpolating between neighbouring stored samples. Mono clips broadcast to
    /// both sides; multichannel clips use the first two channels. Realtime-safe.
    ///
    /// Caller guarantees `pos < clip.frames - 1`, so `i + 1` is in range.
    #[inline]
    fn read_stereo(clip: &AudioClip, pos: f64) -> (f32, f32) {
        let i = pos as usize; // floor
        let frac = (pos - i as f64) as f32;
        let lerp = |ch: &[f32]| ch[i] + (ch[i + 1] - ch[i]) * frac;

        if clip.channels == 1 {
            let m = lerp(&clip.data[0]);
            (m, m)
        } else {
            (lerp(&clip.data[0]), lerp(&clip.data[1]))
        }
    }
}

/// Capacity of the clip hand-off and retirement rings, in clips. Loading is a
/// rare, human-driven action, so a handful of slots is plenty; the control
/// thread drains the retirement ring far faster than clips can pile up.
const CLIP_RING_CAPACITY: usize = 16;

/// A clip plus the track it is destined for. Crosses the control→audio boundary
/// through the hand-off ring; the audio callback uses `track` to pick which
/// strip's player gets the clip. Cheap to move (`u8` + `Arc`); the heavy
/// `AudioClip` itself lives behind the `Arc` and is not copied.
pub struct TrackedClip {
    pub track: u8,
    pub clip: Arc<AudioClip>,
}

/// Create the control→audio clip hand-off ring. The control thread pushes a
/// freshly-decoded clip + its target track; the audio callback pops it and
/// swaps the clip into the addressed strip.
pub fn clip_channel() -> (Producer<TrackedClip>, Consumer<TrackedClip>) {
    RingBuffer::new(CLIP_RING_CAPACITY)
}

/// Create the audio→control retirement ring. The audio callback pushes clips it
/// has displaced (so it never drops them); the control thread drains and drops
/// them, which is where the actual deallocation safely happens. Track index is
/// not retained here — drop order doesn't depend on it.
pub fn retire_channel() -> (Producer<Arc<AudioClip>>, Consumer<Arc<AudioClip>>) {
    RingBuffer::new(CLIP_RING_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 1, 1000));
        // Loaded but not playing -> silence.
        let mut l = [1.0_f32; 256];
        let mut r = [1.0_f32; 256];
        player.render(&mut l, &mut r);
        assert!(
            l.iter().chain(r.iter()).all(|&s| s == 0.0),
            "loaded-but-paused must be silent"
        );
    }

    #[test]
    fn play_advances_and_emits_audio() {
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 2, 10_000));
        player.handle(Command::Play);

        let mut l = [0.0_f32; 256];
        let mut r = [0.0_f32; 256];
        player.render(&mut l, &mut r);

        assert!(player.is_playing());
        assert_eq!(
            player.pos_frames(),
            256,
            "rate-matched playback advances 1:1"
        );
        assert!(
            l.iter().chain(r.iter()).any(|&s| s != 0.0),
            "playing should emit audio"
        );
    }

    #[test]
    fn mono_clip_broadcasts_to_both_channels() {
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 1, 10_000));
        player.handle(Command::Play);

        let mut l = [0.0_f32; 128];
        let mut r = [0.0_f32; 128];
        player.render(&mut l, &mut r);
        // A mono source must produce identical left/right (centered).
        for (a, b) in l.iter().zip(r.iter()) {
            assert!((a - b).abs() < 1e-7, "mono should be identical L/R");
        }
    }

    #[test]
    fn resamples_when_rates_differ() {
        // 44.1 kHz clip on a 48 kHz device: position advances by 44100/48000 per
        // output frame, i.e. slower than 1:1.
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(44_100.0, 1, 100_000));
        player.handle(Command::Play);

        let mut l = [0.0_f32; 480];
        let mut r = [0.0_f32; 480];
        player.render(&mut l, &mut r);

        let expected = (480.0 * 44_100.0 / 48_000.0) as i64; // ~441
        assert!(
            (player.pos_frames() - expected).abs() <= 1,
            "resampled position should be ~{expected}, got {}",
            player.pos_frames()
        );
    }

    #[test]
    fn stops_and_holds_at_end_of_clip() {
        let mut player = WavPlayer::new(48_000.0);
        let frames = 300;
        player.set_clip(test_clip(48_000.0, 1, frames));
        player.handle(Command::Play);

        // Render more frames than the clip has.
        let mut l = [0.0_f32; 512];
        let mut r = [0.0_f32; 512];
        player.render(&mut l, &mut r);

        assert!(!player.is_playing(), "playback should stop at the clip end");
        assert!(player.pos_frames() <= frames as i64);
    }

    #[test]
    fn loops_at_end_instead_of_stopping() {
        let mut player = WavPlayer::new(48_000.0);
        let frames = 300;
        player.set_clip(test_clip(48_000.0, 1, frames));
        player.handle(Command::SetLooping(true));
        player.handle(Command::Play);

        // Render well past the clip end (3× its length).
        let mut l = [0.0_f32; 1000];
        let mut r = [0.0_f32; 1000];
        player.render(&mut l, &mut r);

        assert!(
            player.is_playing(),
            "looping playback must not stop at the end"
        );
        // Position wrapped back into the clip rather than parking at the end.
        assert!(
            player.pos_frames() < frames as i64,
            "position should have wrapped, got {}",
            player.pos_frames()
        );
        // No long run of silence at the tail (it kept producing audio).
        assert!(
            l[l.len() - 1] != 0.0 || r[r.len() - 1] != 0.0 || l[l.len() - 2] != 0.0,
            "looping should keep emitting audio past the end"
        );
    }

    #[test]
    fn seek_and_stop_move_the_playhead() {
        let mut player = WavPlayer::new(48_000.0);
        player.set_clip(test_clip(48_000.0, 1, 48_000)); // exactly 1 second

        player.handle(Command::Seek(0.5)); // half a second in
        assert!((player.pos_frames() - 24_000).abs() <= 1);

        // Seeking past the end clamps to the last frame.
        player.handle(Command::Seek(999.0));
        assert!(player.pos_frames() <= 48_000);

        player.handle(Command::Stop);
        assert_eq!(player.pos_frames(), 0, "stop rewinds to the start");
    }

    #[test]
    fn playback_path_does_not_allocate() {
        // The realtime-safety contract from TESTING.md: exercise command
        // handling, clip swap via the rings, and planar rendering under
        // assert_no_alloc.
        let (mut clip_tx, mut clip_rx) = clip_channel();
        let (mut retire_tx, mut retire_rx) = retire_channel();
        let mut player = WavPlayer::new(48_000.0);
        let mut l = [0.0_f32; 512];
        let mut r = [0.0_f32; 512];

        // Pre-build clips on the control side (allocating here is fine).
        let clips: Vec<Arc<AudioClip>> = (0..4).map(|_| test_clip(44_100.0, 2, 20_000)).collect();
        let mut clip_iter = clips.iter().cloned();

        assert_no_alloc(|| {
            for i in 0..1000 {
                if i % 250 == 0 {
                    if let Some(c) = clip_iter.next() {
                        let _ = clip_tx.push(TrackedClip { track: 0, clip: c });
                    }
                }
                while let Ok(t) = clip_rx.pop() {
                    if let Some(old) = player.set_clip(t.clip) {
                        if let Err(rtrb::PushError::Full(old)) = retire_tx.push(old) {
                            std::mem::forget(old);
                        }
                    }
                }
                player.handle(Command::Play);
                player.render(&mut l, &mut r);
            }
        });

        // Draining retired clips (and thus dropping them) happens off the audio
        // thread — here, after the guarded section.
        while retire_rx.pop().is_ok() {}
    }
}
