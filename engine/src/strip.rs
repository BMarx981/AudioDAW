//! [`Strip`] — one track in the mixer: [`Sampler`] source → [`Chain`]
//! (EQ → gain → pan) → meter, summed into a shared mix bus.
//!
//! ## What changed in Milestone 6
//!
//! Through Milestone 5 each strip held a [`crate::player::WavPlayer`] — one
//! clip per track, playing linearly from its own per-source playhead. M6
//! generalizes the source to a [`Sampler`]: a track now holds multiple
//! [`crate::sampler::TimelineClip`]s placed at arbitrary positions on a
//! *global* timeline, and rendering means "give me the sum of every clip's
//! contribution to this device-frame range." The playhead is no longer a
//! per-strip thing — it's a global counter on the [`Mixer`](crate::mixer::Mixer)
//! that's threaded into every strip's render. Transport (Play/Pause/Stop/Seek)
//! also leaves the strip; it's a property of the global timeline now, not of
//! any individual source.
//!
//! ## What changed in Milestone 4 (still true)
//!
//! [`Strip::render_into`] renders into the caller's planar accumulator and
//! **adds** to it, leaving the device-buffer interleave to the
//! [`Mixer`](crate::mixer::Mixer).
//!
//! ## Why scratch buffers (Rust/realtime note)
//!
//! The DSP units work on planar buffers, so we need somewhere to write the
//! pre-chain signal — but we cannot allocate on the audio thread. So the strip
//! owns two `Vec<f32>` (left/right) sized to [`MAX_BLOCK`] frames, filled once
//! at construction (before any callback runs) and only ever *sliced* after
//! that. The mixer guarantees `render_into` is only ever handed an accumulator
//! slice of at most [`MAX_BLOCK`] frames, so the strip never needs a bigger
//! buffer.
//!
//! ## Signal flow per block
//!
//! 1. If `advancing`, [`Sampler::render`] writes the planar scratch
//!    (`left`, `right`) from whichever clips overlap the requested global
//!    range. Otherwise the scratch is zeroed (paused/stopped → silent).
//! 2. The [`Chain`] processes the scratch in place (EQ → gain → pan, all
//!    per-sample smoothed). Smoothers keep running even when paused so a
//!    pending parameter change finishes resolving across pauses.
//! 3. We add the scratch into the caller's accumulator, updating the post-fader
//!    peak meter as we go. The meter is **post-fader** (standard): it reflects
//!    what you hear after gain and pan, so muting drops the meter to silence,
//!    and the meter decays normally during a pause because we still walk the
//!    (silent) scratch.

use std::sync::Arc;

use crate::clip::AudioClip;
use crate::dsp::{Chain, Process};
use crate::sampler::{Sampler, TimelineClip};

/// Largest accumulator block (in frames) the strip will ever be asked to add
/// into. The mixer chunks bigger device buffers into pieces of this size, so the
/// scratch never has to be bigger.
pub const MAX_BLOCK: usize = 4096;

/// Meter release time: how fast the displayed peak falls after a transient.
/// Attack is instantaneous (the meter jumps up immediately on a louder sample);
/// release is a ~300 ms exponential decay so peaks are readable, not flickery.
const METER_RELEASE_SECS: f32 = 0.3;

/// A single channel strip: multi-clip source, processing chain, planar scratch,
/// and a post-fader peak meter.
pub struct Strip {
    sampler: Sampler,
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

impl Strip {
    /// Build a strip for an output device at `device_rate` Hz. Allocates its
    /// scratch buffers and the sampler's clip-slot pool now, on the spawning
    /// thread, before any callback runs.
    pub fn new(device_rate: f32) -> Self {
        Self {
            sampler: Sampler::new(device_rate),
            chain: Chain::new(device_rate),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            meter_release: (-1.0 / (METER_RELEASE_SECS * device_rate)).exp(),
            peak_l: 0.0,
            peak_r: 0.0,
        }
    }

    /// "Just load this clip as the whole track" — the M5-compatible shortcut
    /// used by the existing bridge. Places `clip` in **slot 0** at start_frame
    /// 0 for its full length, displacing whatever was there. Returns the
    /// displaced source `Arc` (without dropping it — same retirement contract
    /// as Milestones 1–5; the audio callback ships it to the retirement ring).
    /// Realtime-safe.
    ///
    /// Slot-granular clip placement (the actual M6 timeline) goes through
    /// [`Self::place_clip`] et al — wired up in the next step alongside the
    /// new bridge commands.
    #[inline]
    pub fn set_clip(&mut self, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.sampler
            .set_clip(0, TimelineClip::whole(clip, 0))
            .map(|old| old.clip)
    }

    /// Place a [`TimelineClip`] in `slot`, returning the displaced source `Arc`
    /// (undropped) if any. Realtime-safe.
    #[inline]
    pub fn place_clip(
        &mut self,
        slot: usize,
        tc: TimelineClip,
    ) -> Option<Arc<AudioClip>> {
        self.sampler.set_clip(slot, tc).map(|old| old.clip)
    }

    /// Move a placed clip to a new timeline position. Realtime-safe.
    #[inline]
    pub fn move_clip(&mut self, slot: usize, start_frame: i64) {
        self.sampler.set_clip_start(slot, start_frame);
    }

    /// Resize a placed clip on the timeline. Realtime-safe.
    #[inline]
    pub fn resize_clip(&mut self, slot: usize, length_frames: u32) {
        self.sampler.set_clip_length(slot, length_frames);
    }

    /// Shift where in the source a placed clip starts reading. Realtime-safe.
    #[inline]
    pub fn set_clip_source_offset(&mut self, slot: usize, offset_frames: u32) {
        self.sampler.set_clip_source_offset(slot, offset_frames);
    }

    /// Clear one clip slot, returning the displaced source `Arc` (undropped) if
    /// any. Realtime-safe.
    #[inline]
    pub fn clear_clip(&mut self, slot: usize) -> Option<Arc<AudioClip>> {
        self.sampler.clear_clip(slot).map(|old| old.clip)
    }

    /// Clear **slot 0** (the M5-compatible single-clip slot), reset the
    /// processing chain to its defaults, and zero the meter. Returns the
    /// displaced source `Arc` (undropped). This is what the M5-era
    /// `ClearTrack` command resolves to today; once step 3 ships the
    /// slot-granular clip commands, removing a track will drain every slot.
    /// Realtime-safe.
    #[inline]
    pub fn clear(&mut self) -> Option<Arc<AudioClip>> {
        let displaced = self.sampler.clear_clip(0).map(|old| old.clip);
        self.chain.reset();
        self.peak_l = 0.0;
        self.peak_r = 0.0;
        displaced
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
    /// the caller's planar accumulator. `global_frame` is the device-frame
    /// position on the project timeline of the first output frame.
    /// `advancing == false` (paused/stopped) renders silence into the scratch
    /// before the chain runs, so smoothers stay current and the meter decays
    /// normally. The accumulator slices must be the same length and at most
    /// [`MAX_BLOCK`] frames — the mixer enforces that with its own chunking.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free.
    #[inline]
    pub fn render_into(
        &mut self,
        global_frame: i64,
        advancing: bool,
        mix_l: &mut [f32],
        mix_r: &mut [f32],
    ) {
        let frames = mix_l.len().min(mix_r.len());
        debug_assert!(frames <= MAX_BLOCK, "strip block exceeds MAX_BLOCK");
        let left = &mut self.scratch_l[..frames];
        let right = &mut self.scratch_r[..frames];

        // 1. Source -> planar scratch (or silence when paused/stopped).
        if advancing {
            self.sampler.render(global_frame, left, right);
        } else {
            for x in left.iter_mut() {
                *x = 0.0;
            }
            for x in right.iter_mut() {
                *x = 0.0;
            }
        }

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

    /// Drive `strip` for one block at the given global frame. Wraps the new
    /// `render_into(global_frame, advancing, ...)` signature so the test body
    /// reads as it did in M5.
    fn render_block(
        strip: &mut Strip,
        global: &mut i64,
        advancing: bool,
        mix_l: &mut [f32],
        mix_r: &mut [f32],
    ) {
        for x in mix_l.iter_mut() {
            *x = 0.0;
        }
        for x in mix_r.iter_mut() {
            *x = 0.0;
        }
        strip.render_into(*global, advancing, mix_l, mix_r);
        if advancing {
            *global += mix_l.len() as i64;
        }
    }

    #[test]
    fn meter_responds_to_audio_then_decays() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        let mut global = 0_i64;

        // Advancing render of a block of full-ish signal: the meter rises.
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];
        render_block(&mut strip, &mut global, true, &mut mix_l, &mut mix_r);
        assert!(
            strip.peak_left() > 0.5 && strip.peak_right() > 0.5,
            "meter should rise with audio, got L={} R={}",
            strip.peak_left(),
            strip.peak_right()
        );

        // "Stopped": render with advancing=false for ~2 s. Scratch is zeroed, so
        // the chain processes silence and the meter must decay toward zero.
        for _ in 0..200 {
            render_block(&mut strip, &mut global, false, &mut mix_l, &mut mix_r);
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
        strip.chain.set_gain_db(-120.0); // hard mute
        let mut global = 0_i64;

        // Advancing render for ~2 s. Gain smoother mutes within ~10 ms; after
        // that the meter only releases (300 ms time constant) so the held peak
        // decays well below the threshold.
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];
        for _ in 0..200 {
            render_block(&mut strip, &mut global, true, &mut mix_l, &mut mix_r);
        }
        assert!(
            strip.peak_left() < 0.01,
            "muted strip should meter ~0, got {}",
            strip.peak_left()
        );
        assert!(mix_l[mix_l.len() - 1].abs() < 1e-3);
    }

    #[test]
    fn hard_left_pan_silences_right_contribution() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.chain.set_pan(-1.0);
        let mut global = 0_i64;

        let mut mix_l = vec![0.0_f32; 1024];
        let mut mix_r = vec![0.0_f32; 1024];
        for _ in 0..50 {
            render_block(&mut strip, &mut global, true, &mut mix_l, &mut mix_r);
        }
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

    /// The new M6 behaviour: when `advancing == false` the strip must emit
    /// silence into the accumulator regardless of where the global playhead
    /// would otherwise be reading. This is what makes "paused" actually silent
    /// in a multi-clip world (the sampler is frozen, not re-reading one sample
    /// forever).
    #[test]
    fn not_advancing_emits_silence_even_over_an_active_clip() {
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));

        let mut mix_l = vec![0.0_f32; 256];
        let mut mix_r = vec![0.0_f32; 256];
        // Render at global frame 1000 (well inside the clip) but not advancing.
        strip.render_into(1000, false, &mut mix_l, &mut mix_r);
        assert!(
            mix_l.iter().chain(mix_r.iter()).all(|&s| s.abs() < 1e-6),
            "paused render must be silent"
        );
    }

    #[test]
    fn strip_render_does_not_allocate() {
        // The full strip path — sampler render, chain, meter, sum into the
        // accumulator — under assert_no_alloc, with parameter changes and a
        // clip swap, as the mixer exercises it each callback.
        let mut strip = Strip::new(SR);
        let clip = const_clip(0.7, 50_000);
        let mut mix_l = vec![0.0_f32; 512];
        let mut mix_r = vec![0.0_f32; 512];

        assert_no_alloc(|| {
            strip.set_clip(clip); // move in; no alloc (Option::replace via sampler)
            let mut global = 0_i64;
            for i in 0..1000 {
                strip.chain.set_gain_db(-(i % 24) as f32);
                strip.chain.set_pan(((i % 200) as f32 / 100.0) - 1.0);
                for x in mix_l.iter_mut() {
                    *x = 0.0;
                }
                for x in mix_r.iter_mut() {
                    *x = 0.0;
                }
                strip.render_into(global, true, &mut mix_l, &mut mix_r);
                global += mix_l.len() as i64;
            }
        });
    }
}
