//! [`Strip`] — one track in the mixer: WAV source → [`Chain`] (EQ → gain → pan)
//! → meter, summed into a shared mix bus.
//!
//! ## What changed in Milestone 4
//!
//! Through Milestone 3 the strip wrote directly to the interleaved device
//! buffer. With multitrack, that no longer makes sense: every strip's output has
//! to be *summed* with every other strip's, and only the final master sum gets
//! interleaved onto the device. So [`Strip::render_into`] now renders into the
//! caller's planar accumulator and **adds** to it, leaving the device-buffer
//! interleave to the [`Mixer`](crate::mixer::Mixer).
//!
//! ## Why scratch buffers (Rust/realtime note)
//!
//! The DSP units work on planar buffers, so we need somewhere to deinterleave
//! into — but we cannot allocate on the audio thread. So the strip owns two
//! `Vec<f32>` (left/right) sized to [`MAX_BLOCK`] frames, filled once at
//! construction (before any callback runs) and only ever *sliced* after that.
//! The mixer guarantees `render_into` is only ever handed an accumulator slice
//! of at most [`MAX_BLOCK`] frames, so the strip never needs a bigger buffer.
//!
//! ## Signal flow per block
//!
//! 1. [`WavPlayer::render`] fills the planar scratch (`left`, `right`).
//! 2. The [`Chain`] processes the scratch in place (EQ → gain → pan, all
//!    per-sample smoothed).
//! 3. We add the scratch into the caller's accumulator, updating the post-fader
//!    peak meter as we go.
//!
//! The meter is **post-fader** (standard): it reflects what you hear after gain
//! and pan, so muting drops the meter to silence.

use std::sync::Arc;

use crate::clip::AudioClip;
use crate::commands::Command;
use crate::dsp::{Chain, Process};

/// Largest accumulator block (in frames) the strip will ever be asked to add
/// into. The mixer chunks bigger device buffers into pieces of this size, so the
/// scratch never has to be bigger.
pub const MAX_BLOCK: usize = 4096;

/// Meter release time: how fast the displayed peak falls after a transient.
/// Attack is instantaneous (the meter jumps up immediately on a louder sample);
/// release is a ~300 ms exponential decay so peaks are readable, not flickery.
const METER_RELEASE_SECS: f32 = 0.3;

/// A single channel strip: WAV source, processing chain, planar scratch, and a
/// post-fader peak meter.
pub struct Strip {
    player: WavPlayerSource,
    pub(crate) chain: Chain,
    /// Planar scratch, pre-allocated to [`MAX_BLOCK`] and only ever sliced.
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    /// Per-sample release coefficient for the meter envelope (precomputed).
    meter_release: f32,
    /// Running post-fader peak per channel (instant attack, exp release).
    peak_l: f32,
    peak_r: f32,
}

/// Alias so the module reads as "the strip owns its source" without leaking the
/// concrete player type into every signature here.
type WavPlayerSource = crate::player::WavPlayer;

impl Strip {
    /// Build a strip for an output device at `device_rate` Hz. Allocates its
    /// scratch buffers now, on the spawning thread, before any callback runs.
    pub fn new(device_rate: f32) -> Self {
        Self {
            player: WavPlayerSource::new(device_rate),
            chain: Chain::new(device_rate),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            meter_release: (-1.0 / (METER_RELEASE_SECS * device_rate)).exp(),
            peak_l: 0.0,
            peak_r: 0.0,
        }
    }

    /// Apply a transport command (Play/Pause/Stop/Seek/SetLooping) to this
    /// strip's player. Parameter changes are routed by the [`Mixer`] directly to
    /// the chain, so they never come through here. Realtime-safe.
    #[inline]
    pub fn handle_transport(&mut self, cmd: Command) {
        self.player.handle(cmd);
    }

    /// Swap in a new clip, returning the displaced one (undropped — see
    /// [`WavPlayer::set_clip`](crate::player::WavPlayer::set_clip)).
    #[inline]
    pub fn set_clip(&mut self, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.player.set_clip(clip)
    }

    /// Fully clear this strip — drop the clip (returning it undropped for the
    /// control thread to dispose of), reset the processing chain to its
    /// defaults, and zero the meter. Used by [`crate::mixer::Mixer::clear_track`]
    /// when a track is removed from the project so the strip is fresh if it's
    /// reused later. Realtime-safe.
    #[inline]
    pub fn clear(&mut self) -> Option<Arc<AudioClip>> {
        let displaced = self.player.clear();
        self.chain.reset();
        self.peak_l = 0.0;
        self.peak_r = 0.0;
        displaced
    }

    /// Current playhead position in clip frames. Realtime-safe.
    #[inline]
    pub fn pos_frames(&self) -> i64 {
        self.player.pos_frames()
    }

    /// Whether this strip's player is currently advancing. Realtime-safe.
    #[inline]
    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
    }

    /// Latest post-fader peak for the left channel, linear (0..≈1, can exceed 1
    /// if boosted). Realtime-safe.
    #[inline]
    pub fn peak_left(&self) -> f32 {
        self.peak_l
    }

    /// Latest post-fader peak for the right channel, linear. Realtime-safe.
    #[inline]
    pub fn peak_right(&self) -> f32 {
        self.peak_r
    }

    /// Render `mix_l.len()` frames of this strip's output and **add** them into
    /// the caller's planar accumulator. The accumulator slices must be the same
    /// length and at most [`MAX_BLOCK`] frames — the mixer enforces that with
    /// its own chunking.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free.
    #[inline]
    pub fn render_into(&mut self, mix_l: &mut [f32], mix_r: &mut [f32]) {
        let frames = mix_l.len().min(mix_r.len());
        debug_assert!(frames <= MAX_BLOCK, "strip block exceeds MAX_BLOCK");
        let left = &mut self.scratch_l[..frames];
        let right = &mut self.scratch_r[..frames];

        // 1. Source -> planar scratch.
        self.player.render(left, right);

        // 2. EQ -> gain -> pan, in place. The borrow checker guarantees the
        //    chain can't alias the scratch slices it processes.
        let mut block: [&mut [f32]; 2] = [left, right];
        self.chain.process(&mut block);

        // 3. Add the scratch into the caller's accumulator and update the
        //    post-fader peak meter sample-by-sample. We re-borrow the scratch
        //    immutably now the chain is done.
        let left = &self.scratch_l[..frames];
        let right = &self.scratch_r[..frames];
        for f in 0..frames {
            let l = left[f];
            let r = right[f];
            // Instant attack to a louder sample, exponential release otherwise.
            self.peak_l = (self.peak_l * self.meter_release).max(l.abs());
            self.peak_r = (self.peak_r * self.meter_release).max(r.abs());
            mix_l[f] += l;
            mix_r[f] += r;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    /// A constant-amplitude stereo clip — easy to reason about through gain/pan.
    fn const_clip(amp: f32, frames: usize) -> Arc<AudioClip> {
        Arc::new(AudioClip::new(SR, vec![vec![amp; frames], vec![amp; frames]]).unwrap())
    }

    #[test]
    fn meter_responds_to_audio_then_decays() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle_transport(Command::Play);

        // Render a block of full-ish signal: the meter should read clearly > 0.
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];
        strip.render_into(&mut mix_l, &mut mix_r);
        assert!(
            strip.peak_left() > 0.5 && strip.peak_right() > 0.5,
            "meter should rise with audio, got L={} R={}",
            strip.peak_left(),
            strip.peak_right()
        );

        // Stop and render silence for ~2 s; the meter must decay toward zero.
        strip.handle_transport(Command::Stop);
        for _ in 0..200 {
            for x in mix_l.iter_mut() {
                *x = 0.0;
            }
            for x in mix_r.iter_mut() {
                *x = 0.0;
            }
            strip.render_into(&mut mix_l, &mut mix_r);
        }
        assert!(
            strip.peak_left() < 0.01 && strip.peak_right() < 0.01,
            "meter should decay to ~0 after silence, got L={} R={}",
            strip.peak_left(),
            strip.peak_right()
        );
    }

    #[test]
    fn mute_drops_the_meter() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle_transport(Command::Play);
        strip.chain.set_gain_db(-120.0); // hard mute

        // The gain smoother mutes the signal within ~10 ms; after that the meter
        // only releases (300 ms time constant). Run ~2 s of audio so the held
        // peak has decayed well below the threshold.
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];
        for _ in 0..200 {
            for x in mix_l.iter_mut() {
                *x = 0.0;
            }
            for x in mix_r.iter_mut() {
                *x = 0.0;
            }
            strip.render_into(&mut mix_l, &mut mix_r);
        }
        assert!(
            strip.peak_left() < 0.01,
            "muted strip should meter ~0, got {}",
            strip.peak_left()
        );
        // And the strip's added contribution is effectively silence at the tail.
        assert!(mix_l[mix_l.len() - 1].abs() < 1e-3);
    }

    #[test]
    fn hard_left_pan_silences_right_contribution() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle_transport(Command::Play);
        strip.chain.set_pan(-1.0);

        let mut mix_l = vec![0.0_f32; 1024];
        let mut mix_r = vec![0.0_f32; 1024];
        for _ in 0..50 {
            for x in mix_l.iter_mut() {
                *x = 0.0;
            }
            for x in mix_r.iter_mut() {
                *x = 0.0;
            }
            strip.render_into(&mut mix_l, &mut mix_r);
        }
        // Right contribution should be ~silent; left should be audible.
        assert!(
            mix_l[mix_l.len() - 1].abs() > 0.3,
            "left should carry the signal, got {}",
            mix_l[mix_l.len() - 1]
        );
        assert!(
            mix_r[mix_r.len() - 1].abs() < 0.01,
            "hard-left pan should silence right, got {}",
            mix_r[mix_r.len() - 1]
        );
    }

    #[test]
    fn strip_render_does_not_allocate() {
        // The full strip path — transport, planar render, chain, meter, sum into
        // the accumulator — under assert_no_alloc, with parameter changes and a
        // clip swap, as the mixer exercises it each callback.
        let mut strip = Strip::new(SR);
        let clip = const_clip(0.7, 50_000);
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];

        assert_no_alloc(|| {
            strip.set_clip(clip); // move in; no alloc (Option::replace)
            strip.handle_transport(Command::Play);
            for i in 0..1000 {
                strip.chain.set_gain_db(-(i % 24) as f32);
                strip.chain.set_pan(((i % 200) as f32 / 100.0) - 1.0);
                for x in mix_l.iter_mut() {
                    *x = 0.0;
                }
                for x in mix_r.iter_mut() {
                    *x = 0.0;
                }
                strip.render_into(&mut mix_l, &mut mix_r);
            }
        });
    }
}
