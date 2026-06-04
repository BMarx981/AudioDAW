//! [`Sampler`] — a multi-clip realtime source.
//!
//! A track's audio source as of Milestone 6: instead of one [`AudioClip`]
//! streaming linearly (the old [`crate::player::WavPlayer`]), a track holds a
//! **fixed-capacity pool of [`TimelineClip`] slots**. Each slot places a clip on
//! the global timeline at a `start_frame` (in **device frames**), runs for at
//! most `length_frames`, and reads from the source starting at
//! `source_offset_frames`. Every callback the sampler is told the global
//! playhead, computes which clips overlap the requested block, and sums their
//! contributions into the output.
//!
//! ## Why "global frame" instead of per-source position
//!
//! With one source you can carry the playhead inside it (`pos` in
//! [`crate::player::WavPlayer`]). With many sources you can't: clips can start
//! anywhere, overlap each other, share an underlying [`AudioClip`] across
//! different placements, and the *same* source might appear twice on a track.
//! The natural reference frame becomes the timeline itself — measured in device
//! frames since the project's zero point — and every clip's read position is
//! derived from it.
//!
//! ## Realtime safety
//!
//! Everything in [`Sampler::render`] is allocation-free, lock-free, panic-free:
//! - slots live in a pre-allocated `Vec`, mutated only by `Option::take` /
//!   `Option::replace` (moves, no drops on this thread);
//! - per-output-frame work is stack arithmetic + bounds-checked indexing into a
//!   buffer that's at least `frames + 1` long by construction (the inner
//!   `pos < source_last` guard);
//! - displaced clips returned by setters cross back to the control thread for
//!   disposal (the same pattern as
//!   [`crate::player::WavPlayer::set_clip`]).
//!
//! ## What this module does *not* do
//!
//! It's a source. No EQ/gain/pan, no meters, no transport state — those still
//! live on the [`crate::strip::Strip`] and the [`crate::mixer::Mixer`]. The
//! sampler is told a global frame each render and trusts the caller for
//! everything else.
//!
//! ## Out-of-scope (Milestone 6 deliberately defers)
//!
//! - Per-clip loop / fade-in / fade-out: source ends → silence; no crossfading.
//! - Per-clip gain / pitch: the chain does track-level gain; pitch is a later
//!   milestone.
//! - Dynamic pool resize: the pool is sized once. If a project ever needs more
//!   than [`MAX_CLIPS_PER_TRACK`], a future milestone can grow the cap by
//!   reallocating during a stream-stop window (off the audio thread).

use std::sync::Arc;

use crate::clip::AudioClip;

/// Maximum [`TimelineClip`]s a single [`Sampler`] can hold. 64 covers typical
/// arrangements (chopped drum loops, layered backings); a future milestone that
/// needs more can grow this — every slot is just an `Option<TimelineClip>` and
/// costs nothing when empty.
pub const MAX_CLIPS_PER_TRACK: usize = 64;

/// A single scheduled appearance of an [`AudioClip`] on a track's timeline.
///
/// All time fields are in **device frames** (the units the audio callback
/// runs in), not the underlying source's native rate — keeping the timeline in
/// one unit makes scheduling math straightforward and unambiguous.
///
/// `clip` is shared by `Arc`: many `TimelineClip`s can refer to the same source
/// (loops, duplications), and the audio thread never drops an `Arc` — displaced
/// clips are handed back to the control thread.
#[derive(Clone)]
pub struct TimelineClip {
    /// The decoded source audio.
    pub clip: Arc<AudioClip>,
    /// Position on the project timeline (device frames) where this placement
    /// starts. Signed so that future "negative" placements (a clip whose source
    /// starts before the project zero) are representable without an API change.
    pub start_frame: i64,
    /// Number of device frames the placement covers. Once the playhead passes
    /// `start_frame + length_frames`, the sampler emits silence for this slot
    /// even if the source has more audio.
    pub length_frames: u32,
    /// Where in the source to begin reading (source-rate frames). Lets a
    /// trimmed clip skip silence at the head of the WAV without re-encoding.
    pub source_offset_frames: u32,
}

impl TimelineClip {
    /// Convenience: a clip that plays the source from frame 0 for its full
    /// length. Useful in tests and as the default when the UI just "drops a
    /// WAV onto the timeline at position `start_frame`".
    pub fn whole(clip: Arc<AudioClip>, start_frame: i64) -> Self {
        let length_frames = clip.frames as u32;
        Self {
            clip,
            start_frame,
            length_frames,
            source_offset_frames: 0,
        }
    }
}

/// A track's multi-clip source. Owns a fixed pool of slots; renders the sum of
/// the slots' contributions over a requested device-frame range.
pub struct Sampler {
    /// Output device sample rate (Hz). Fixed for the life of the stream.
    device_rate: f32,
    /// Slot pool, allocated once at construction. `None` slots are inactive and
    /// cost nothing to skip; `Some` slots are rendered.
    slots: Vec<Option<TimelineClip>>,
}

impl Sampler {
    /// Build a sampler for an output device running at `device_rate` Hz, with
    /// [`MAX_CLIPS_PER_TRACK`] empty slots. Allocates the slot pool now, on the
    /// spawning thread, before any callback runs.
    pub fn new(device_rate: f32) -> Self {
        let mut slots = Vec::with_capacity(MAX_CLIPS_PER_TRACK);
        for _ in 0..MAX_CLIPS_PER_TRACK {
            slots.push(None);
        }
        Self { device_rate, slots }
    }

    /// Capacity of the slot pool. Stable for the life of the sampler.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Number of currently occupied slots. Cheap (O(capacity)); fine for the
    /// control side, never called from the audio thread.
    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Install `tc` into `slot`, returning whatever was there before — **without
    /// dropping it**. The caller (the audio callback) ships the displaced clip
    /// back to the control thread for safe disposal, same contract as
    /// [`crate::player::WavPlayer::set_clip`]. Realtime-safe. Out-of-range
    /// `slot` is a no-op (returns `None`).
    #[inline]
    pub fn set_clip(&mut self, slot: usize, tc: TimelineClip) -> Option<TimelineClip> {
        self.slots.get_mut(slot)?.replace(tc)
    }

    /// Empty `slot`, returning the displaced clip (undropped). Out-of-range
    /// slots and already-empty slots return `None`. Realtime-safe.
    #[inline]
    pub fn clear_clip(&mut self, slot: usize) -> Option<TimelineClip> {
        self.slots.get_mut(slot)?.take()
    }

    /// Move the placement: change `start_frame` for `slot`. Realtime-safe.
    /// Out-of-range or empty slots are no-ops.
    #[inline]
    pub fn set_clip_start(&mut self, slot: usize, start_frame: i64) {
        if let Some(Some(tc)) = self.slots.get_mut(slot) {
            tc.start_frame = start_frame;
        }
    }

    /// Resize the placement on the timeline. Realtime-safe.
    #[inline]
    pub fn set_clip_length(&mut self, slot: usize, length_frames: u32) {
        if let Some(Some(tc)) = self.slots.get_mut(slot) {
            tc.length_frames = length_frames;
        }
    }

    /// Shift where in the source the placement starts reading. Realtime-safe.
    #[inline]
    pub fn set_clip_source_offset(&mut self, slot: usize, source_offset_frames: u32) {
        if let Some(Some(tc)) = self.slots.get_mut(slot) {
            tc.source_offset_frames = source_offset_frames;
        }
    }

    /// Read-only access to a slot, for inspection in tests and on the control
    /// side. Not realtime-relevant.
    #[inline]
    pub fn slot(&self, slot: usize) -> Option<&TimelineClip> {
        self.slots.get(slot).and_then(|s| s.as_ref())
    }

    /// Drop every active clip in the pool (returning the displaced `Arc`s via
    /// `out` so the caller can ship them to retirement). Used when a track is
    /// removed. Realtime-safe; `out` is a buffer the caller pre-allocates.
    ///
    /// Stops early if `out` fills, so the caller must size it to at least
    /// [`MAX_CLIPS_PER_TRACK`] for a guaranteed-complete clear.
    pub fn clear_all_into(&mut self, out: &mut [Option<TimelineClip>]) {
        let mut i = 0;
        for slot in self.slots.iter_mut() {
            if i >= out.len() {
                break;
            }
            if let Some(tc) = slot.take() {
                out[i] = Some(tc);
                i += 1;
            }
        }
    }

    /// Render `frames` device-rate planar stereo samples into `left`/`right`,
    /// where the first output frame corresponds to global timeline frame
    /// `global_start_frame`. **Writes** (does not accumulate from caller); the
    /// strip's chain then processes these scratch buffers in place.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free.
    ///
    /// Per-clip cost is proportional to the *overlap* of the clip's placement
    /// with the requested block — clips that don't fall in this block contribute
    /// only a couple of comparisons.
    #[inline]
    pub fn render(&mut self, global_start_frame: i64, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());

        // 1. Zero the output. Sampler is a writing source.
        for i in 0..frames {
            left[i] = 0.0;
            right[i] = 0.0;
        }
        if frames == 0 {
            return;
        }

        let global_end = global_start_frame.saturating_add(frames as i64);

        // 2. For each placed clip: clip its [start, start+length) span against
        //    [global_start, global_end), then add its samples (resampled from
        //    its native rate) into the output range.
        for slot in self.slots.iter() {
            let Some(tc) = slot.as_ref() else {
                continue;
            };

            // The placement's timeline span. Both ends in device frames.
            let clip_timeline_start = tc.start_frame;
            let clip_timeline_end =
                tc.start_frame.saturating_add(tc.length_frames as i64);

            // Intersection with the block we're rendering.
            let overlap_start = clip_timeline_start.max(global_start_frame);
            let overlap_end = clip_timeline_end.min(global_end);
            if overlap_start >= overlap_end {
                continue;
            }

            // Output indices [out_start, out_end) within left/right.
            let out_start = (overlap_start - global_start_frame) as usize;
            let out_end = (overlap_end - global_start_frame) as usize;

            // Source read position at the first output sample of the overlap.
            // `dt_device` is how many device frames into the placement we are
            // (>= 0 because overlap_start >= clip_timeline_start); convert to
            // source frames via the rate ratio and add the user-set offset.
            let source = &tc.clip;
            let device_into_clip = (overlap_start - clip_timeline_start) as f64;
            let step = if self.device_rate > 0.0 {
                source.sample_rate as f64 / self.device_rate as f64
            } else {
                0.0
            };
            if step <= 0.0 {
                // Degenerate device or clip rate; can't safely resample.
                continue;
            }
            let mut pos = tc.source_offset_frames as f64 + device_into_clip * step;

            // Skip clips too short to interpolate at all — `read_stereo` reads
            // `i` and `i+1`, so it needs at least 2 source frames. With the
            // `min` clamp on the next-sample index below, frames == 1 would
            // technically read the same sample twice; bail anyway, since one
            // sample of audio is below any audible threshold.
            if source.frames < 2 {
                continue;
            }
            // Strict upper bound on `pos`. We render every position in
            // `[source_offset, source.frames)`, including the last integer
            // frame (where `pos as usize == frames - 1` and frac == 0). The
            // older `pos < frames - 1` cutoff dropped that final sample, which
            // showed up as an off-by-one at every clip's trailing edge.
            let source_max_pos = source.frames as f64;

            for f in out_start..out_end {
                if pos >= source_max_pos {
                    break;
                }
                let (l, r) = read_stereo(source, pos);
                left[f] += l;
                right[f] += r;
                pos += step;
            }
        }
    }
}

/// Linearly interpolate one stereo sample from `source` at fractional position
/// `pos`. Mono sources broadcast to both sides; sources with more than two
/// channels use the first two. Realtime-safe.
///
/// Caller guarantees `source.frames >= 2` and `pos < source.frames`. The
/// `i + 1` interpolation index is clamped to the last frame so the final
/// integer position (where `i == frames - 1` and `frac == 0`) reads
/// `source[frames - 1]` exactly rather than out-of-bounds — slightly different
/// from [`crate::player::WavPlayer`]'s contract, which uses
/// `pos < frames - 1` and so drops the final sample (the player is single-clip
/// streaming where one missed sample at the tail is invisible; the sampler's
/// per-clip overlap arithmetic exposes it as a missing sample at every clip
/// boundary).
#[inline]
fn read_stereo(source: &AudioClip, pos: f64) -> (f32, f32) {
    let i = pos as usize; // floor
    // Clamp the next-sample read so `pos = frames - 1` (frac = 0) is in range.
    // `source.frames >= 2` by caller, so `frames - 1 >= 1`, no underflow.
    let i_next = (i + 1).min(source.frames - 1);
    let frac = (pos - i as f64) as f32;
    let lerp = |ch: &[f32]| ch[i] + (ch[i_next] - ch[i]) * frac;

    if source.channels == 1 {
        let m = lerp(&source.data[0]);
        (m, m)
    } else {
        (lerp(&source.data[0]), lerp(&source.data[1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    /// A clip whose samples are simply `value` everywhere — easy to reason
    /// about when summing several clips or checking placement boundaries.
    fn const_clip(rate: f32, channels: usize, frames: usize, value: f32) -> Arc<AudioClip> {
        let data = (0..channels)
            .map(|_| vec![value; frames])
            .collect();
        Arc::new(AudioClip::new(rate, data).unwrap())
    }

    /// A clip whose left channel is a per-frame counter (0, 1, 2, …) and right
    /// is its negative, so we can check both position and channel identity.
    fn ramp_clip(rate: f32, frames: usize) -> Arc<AudioClip> {
        let l: Vec<f32> = (0..frames).map(|i| i as f32).collect();
        let r: Vec<f32> = (0..frames).map(|i| -(i as f32)).collect();
        Arc::new(AudioClip::new(rate, vec![l, r]).unwrap())
    }

    /// A sampler with nothing placed must produce silence at every position.
    #[test]
    fn empty_pool_renders_silence() {
        let mut s = Sampler::new(SR);
        let mut l = [0.5_f32; 64];
        let mut r = [0.5_f32; 64];
        s.render(0, &mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|&x| x == 0.0));

        // Far into the timeline, still silence.
        s.render(1_000_000, &mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|&x| x == 0.0));
    }

    /// One rate-matched clip at start_frame = 0 reads from its own frame N at
    /// global frame N — the simplest placement, used to anchor every later
    /// expectation about the math.
    #[test]
    fn single_clip_at_origin_reads_one_to_one() {
        let mut s = Sampler::new(SR);
        let clip = ramp_clip(SR, 1000);
        s.set_clip(0, TimelineClip::whole(clip, 0));

        let mut l = [0.0_f32; 32];
        let mut r = [0.0_f32; 32];
        s.render(0, &mut l, &mut r);
        for i in 0..32 {
            assert!((l[i] - i as f32).abs() < 1e-3, "L[{i}] = {}", l[i]);
            assert!((r[i] - -(i as f32)).abs() < 1e-3, "R[{i}] = {}", r[i]);
        }
    }

    /// A clip placed at start_frame = K must produce silence before K and the
    /// clip's samples after K. This is the headline sample-accurate placement
    /// guarantee from the milestone's stop signal.
    #[test]
    fn placement_offset_delays_audio_by_exactly_start_frame() {
        let mut s = Sampler::new(SR);
        let clip = ramp_clip(SR, 200);
        s.set_clip(0, TimelineClip::whole(clip, 100));

        let mut l = [0.0_f32; 150];
        let mut r = [0.0_f32; 150];
        s.render(0, &mut l, &mut r);

        // Frames [0, 100) — before the clip starts — must be exactly silent.
        for i in 0..100 {
            assert_eq!(l[i], 0.0, "expected silence at frame {i}");
            assert_eq!(r[i], 0.0);
        }
        // Frames [100, 150) — first 50 of the clip.
        for i in 100..150 {
            let src_i = (i - 100) as f32;
            assert!((l[i] - src_i).abs() < 1e-3);
            assert!((r[i] - -src_i).abs() < 1e-3);
        }
    }

    /// Two overlapping constant-amplitude clips sum sample-by-sample over their
    /// overlap, and outside the overlap each contributes alone.
    #[test]
    fn overlapping_clips_sum_in_their_overlap() {
        let mut s = Sampler::new(SR);
        // Clip A: amplitude 0.2, span [0, 100).
        s.set_clip(
            0,
            TimelineClip {
                clip: const_clip(SR, 2, 100, 0.2),
                start_frame: 0,
                length_frames: 100,
                source_offset_frames: 0,
            },
        );
        // Clip B: amplitude 0.5, span [50, 150).
        s.set_clip(
            1,
            TimelineClip {
                clip: const_clip(SR, 2, 100, 0.5),
                start_frame: 50,
                length_frames: 100,
                source_offset_frames: 0,
            },
        );

        let mut l = [0.0_f32; 160];
        let mut r = [0.0_f32; 160];
        s.render(0, &mut l, &mut r);

        // [0, 50): only A → 0.2.
        for i in 0..50 {
            assert!((l[i] - 0.2).abs() < 1e-4, "A-only at {i}: {}", l[i]);
        }
        // [50, 100): A + B → 0.7.
        for i in 50..100 {
            assert!((l[i] - 0.7).abs() < 1e-4, "A+B at {i}: {}", l[i]);
        }
        // [100, 150): only B → 0.5.
        for i in 100..150 {
            assert!((l[i] - 0.5).abs() < 1e-4, "B-only at {i}: {}", l[i]);
        }
        // [150, 160): past both → silence.
        for i in 150..160 {
            assert_eq!(l[i], 0.0);
        }
    }

    /// `length_frames` caps the placement on the timeline regardless of source
    /// length — past it, the slot must be silent even though more source audio
    /// exists. This is what lets the UI shorten a clip without resampling it.
    #[test]
    fn length_frames_truncates_the_placement() {
        let mut s = Sampler::new(SR);
        let clip = const_clip(SR, 2, 1000, 0.4); // plenty of source
        s.set_clip(
            0,
            TimelineClip {
                clip,
                start_frame: 0,
                length_frames: 30, // but cap at 30 frames on the timeline
                source_offset_frames: 0,
            },
        );

        let mut l = [0.0_f32; 64];
        let mut r = [0.0_f32; 64];
        s.render(0, &mut l, &mut r);

        for i in 0..30 {
            assert!((l[i] - 0.4).abs() < 1e-4);
        }
        for i in 30..64 {
            assert_eq!(l[i], 0.0, "past length must be silent at {i}");
        }
    }

    /// `source_offset_frames` skips into the source: the placement's first
    /// output sample comes from `source[source_offset_frames]`, not `source[0]`.
    /// That's how the UI trims silence off the head of a WAV without re-encoding.
    #[test]
    fn source_offset_skips_into_the_source() {
        let mut s = Sampler::new(SR);
        let clip = ramp_clip(SR, 1000);
        s.set_clip(
            0,
            TimelineClip {
                clip,
                start_frame: 0,
                length_frames: 50,
                source_offset_frames: 200, // start at source frame 200
            },
        );

        let mut l = [0.0_f32; 50];
        let mut r = [0.0_f32; 50];
        s.render(0, &mut l, &mut r);
        for i in 0..50 {
            // Output frame i reads source frame 200 + i (rate-matched).
            assert!(
                (l[i] - (200 + i) as f32).abs() < 1e-3,
                "L[{i}] = {}, expected {}",
                l[i],
                200 + i
            );
        }
    }

    /// Reading a 44.1 kHz source on a 48 kHz device must advance the source
    /// position by 44100/48000 per output frame — the sampler resamples on the
    /// fly, same contract as the old WavPlayer.
    #[test]
    fn rate_mismatch_resamples_linearly() {
        let mut s = Sampler::new(SR); // 48 kHz device
        let clip = ramp_clip(44_100.0, 1000);
        s.set_clip(0, TimelineClip::whole(clip, 0));

        let mut l = [0.0_f32; 100];
        let mut r = [0.0_f32; 100];
        s.render(0, &mut l, &mut r);

        // Output frame 50 corresponds to source pos = 50 * 44100/48000 ≈ 45.9375.
        // Linearly interpolating a ramp gives back that fractional value.
        let expected = 50.0 * 44_100.0 / 48_000.0;
        assert!(
            (l[50] - expected).abs() < 1e-3,
            "expected ~{expected}, got {}",
            l[50]
        );
    }

    /// Rendering across a block boundary must produce identical samples to
    /// rendering the same span as a single big call — i.e., no per-block reset
    /// or drift. This is the property M6's "drag clips, hit play, hear the
    /// arrangement" relies on under cpal's variable buffer sizes.
    #[test]
    fn split_render_matches_single_render() {
        let make = || {
            let mut s = Sampler::new(SR);
            s.set_clip(0, TimelineClip::whole(ramp_clip(44_100.0, 5000), 17));
            s.set_clip(
                1,
                TimelineClip {
                    clip: const_clip(SR, 2, 500, 0.3),
                    start_frame: 200,
                    length_frames: 500,
                    source_offset_frames: 50,
                },
            );
            s
        };

        // Single 300-frame render.
        let mut s_big = make();
        let mut big_l = vec![0.0; 300];
        let mut big_r = vec![0.0; 300];
        s_big.render(0, &mut big_l, &mut big_r);

        // Three 100-frame renders advancing the global frame between them.
        let mut s_split = make();
        let mut split_l = vec![0.0; 300];
        let mut split_r = vec![0.0; 300];
        for chunk in 0..3 {
            let start = chunk * 100;
            s_split.render(
                start as i64,
                &mut split_l[start..start + 100],
                &mut split_r[start..start + 100],
            );
        }

        for i in 0..300 {
            assert!(
                (big_l[i] - split_l[i]).abs() < 1e-5,
                "L[{i}] differs: {} vs {}",
                big_l[i],
                split_l[i]
            );
            assert!((big_r[i] - split_r[i]).abs() < 1e-5);
        }
    }

    /// The golden M6 case: place clips with known shape at known offsets, run a
    /// long render, and check the *whole* mix against a manually computed
    /// expected buffer. If the scheduling math ever drifts, this catches it.
    #[test]
    fn golden_render_three_clips_at_known_offsets() {
        const TOTAL: usize = 400;
        let mut s = Sampler::new(SR);
        // Clip A: amp 0.1, [0, 200), rate-matched.
        s.set_clip(
            0,
            TimelineClip {
                clip: const_clip(SR, 2, 200, 0.1),
                start_frame: 0,
                length_frames: 200,
                source_offset_frames: 0,
            },
        );
        // Clip B: amp 0.2, [100, 300).
        s.set_clip(
            1,
            TimelineClip {
                clip: const_clip(SR, 2, 200, 0.2),
                start_frame: 100,
                length_frames: 200,
                source_offset_frames: 0,
            },
        );
        // Clip C: amp 0.4, [250, 400).
        s.set_clip(
            2,
            TimelineClip {
                clip: const_clip(SR, 2, 150, 0.4),
                start_frame: 250,
                length_frames: 150,
                source_offset_frames: 0,
            },
        );

        let mut got_l = vec![0.0_f32; TOTAL];
        let mut got_r = vec![0.0_f32; TOTAL];
        s.render(0, &mut got_l, &mut got_r);

        // Expected: per-frame sum of whichever clips are live.
        let mut want_l = vec![0.0_f32; TOTAL];
        for i in 0..TOTAL {
            let mut v = 0.0;
            if (0..200).contains(&i) {
                v += 0.1;
            }
            if (100..300).contains(&i) {
                v += 0.2;
            }
            if (250..400).contains(&i) {
                v += 0.4;
            }
            want_l[i] = v;
        }

        for i in 0..TOTAL {
            assert!(
                (got_l[i] - want_l[i]).abs() < 1e-4,
                "frame {i}: got {} want {}",
                got_l[i],
                want_l[i]
            );
            // Symmetric stereo since every source is `const_clip` (L == R).
            assert!((got_r[i] - want_l[i]).abs() < 1e-4);
        }
    }

    /// Render starting from a non-zero global frame must place each clip
    /// correctly relative to that frame — not relative to 0. Concretely, a
    /// clip at start_frame = 1000 rendered from global = 950 must put its
    /// first sample at output index 50.
    #[test]
    fn render_starting_partway_into_the_timeline() {
        let mut s = Sampler::new(SR);
        s.set_clip(0, TimelineClip::whole(ramp_clip(SR, 500), 1000));

        let mut l = [0.0_f32; 100];
        let mut r = [0.0_f32; 100];
        s.render(950, &mut l, &mut r);

        for i in 0..50 {
            assert_eq!(l[i], 0.0, "before clip start at {i}");
        }
        for i in 50..100 {
            let src_i = (i - 50) as f32;
            assert!((l[i] - src_i).abs() < 1e-3, "L[{i}] = {}", l[i]);
        }
    }

    /// Out-of-range slot indices in setters must be silent no-ops — never
    /// panic, never grow the pool. The audio thread mustn't crash on a
    /// bad slot from the control side.
    #[test]
    fn out_of_range_setters_are_no_ops() {
        let mut s = Sampler::new(SR);
        // 999 is well outside MAX_CLIPS_PER_TRACK.
        assert!(s
            .set_clip(999, TimelineClip::whole(ramp_clip(SR, 10), 0))
            .is_none());
        assert!(s.clear_clip(999).is_none());
        s.set_clip_start(999, 12345); // must not panic
        s.set_clip_length(999, 42);
        s.set_clip_source_offset(999, 7);
        assert_eq!(s.active_count(), 0);
    }

    /// Renders + setter churn under assert_no_alloc — the realtime-safety
    /// guarantee from CLAUDE.md and TESTING.md. This is the corollary of the
    /// dynamic-add/remove test the mixer has at the strip-pool level.
    #[test]
    fn render_and_setter_churn_does_not_allocate() {
        let mut s = Sampler::new(SR);
        let mut l = vec![0.0_f32; 512];
        let mut r = vec![0.0_f32; 512];

        // Pre-build clips on the control side so the guarded section only does
        // moves (Option::replace / Option::take), never an Arc clone or alloc.
        let clips: Vec<TimelineClip> = (0..8)
            .map(|i| TimelineClip {
                clip: const_clip(SR, 2, 4_000, 0.1 * (i as f32 + 1.0)),
                start_frame: (i as i64) * 500,
                length_frames: 4_000,
                source_offset_frames: 0,
            })
            .collect();
        let mut clip_iter = clips.into_iter();
        // Pre-allocated retirement scratch (so the test simulates the audio
        // callback's "displaced clip is moved off-thread for drop" pattern).
        let mut retired: Option<TimelineClip> = None;

        assert_no_alloc(|| {
            let mut global = 0_i64;
            for tick in 0..1000 {
                // Periodically place a fresh clip in a rotating slot.
                if tick % 100 == 0 {
                    if let Some(c) = clip_iter.next() {
                        let slot = (tick / 100) % MAX_CLIPS_PER_TRACK;
                        if let Some(old) = s.set_clip(slot, c) {
                            // Hold the displaced clip in scratch — must not drop
                            // it on this thread (drop frees the Arc's buffer).
                            retired = Some(old);
                        }
                    }
                }
                // Periodically move a clip's start frame (a UI drag).
                s.set_clip_start(tick % MAX_CLIPS_PER_TRACK, (tick as i64) * 7);

                s.render(global, &mut l, &mut r);
                global += l.len() as i64;
            }
        });

        // Drain retired clips off the guarded section (here = control thread).
        drop(retired);
    }
}
