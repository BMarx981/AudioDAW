//! Decoded audio held in memory: [`AudioClip`].
//!
//! ## Why this type is immutable and shared by `Arc`
//!
//! A clip can be several megabytes (a few minutes of stereo `f32`). We decode it
//! once on the control thread and then need the *audio thread* to read from it —
//! but the audio thread must never allocate, free, or lock. The pattern that
//! makes that safe is: build the clip once, wrap it in an [`std::sync::Arc`], and
//! only ever *share* it (clone the `Arc`, which is a cheap atomic refcount bump
//! on the control thread). The audio thread holds an `Arc<AudioClip>` and only
//! reads `&self` — no mutation, no allocation. See [`crate::player`] for how the
//! `Arc` is handed across the realtime boundary without ever being *dropped* on
//! the audio thread.
//!
//! Samples are stored **planar** (one `Vec<f32>` per channel) and in `f32`, which
//! is the engine's internal format (see CLAUDE.md). Decoding converts to this at
//! the I/O boundary.

/// Immutable, fully-decoded audio sitting in memory.
///
/// Construct via [`crate::decode`]. Never mutated after construction — that's
/// what lets many threads share one behind an `Arc` with no locking.
#[derive(Debug, Clone)]
pub struct AudioClip {
    /// Sample rate of the *file*, in Hz. May differ from the audio device's
    /// rate; [`crate::player`] resamples on the fly to bridge the difference.
    pub sample_rate: f32,
    /// Number of channels (1 = mono, 2 = stereo). `data.len() == channels`.
    pub channels: usize,
    /// Length in frames (samples per channel). Every inner `Vec` has this len.
    pub frames: usize,
    /// Planar sample data: `data[channel][frame]`, each value in roughly
    /// `[-1.0, 1.0]`.
    pub data: Vec<Vec<f32>>,
}

impl AudioClip {
    /// Build a clip from planar channel data. Returns an error if the channels
    /// are ragged (different lengths) or there are no channels — both would make
    /// the realtime reader's indexing assumptions false.
    pub fn new(sample_rate: f32, data: Vec<Vec<f32>>) -> Result<Self, String> {
        let channels = data.len();
        if channels == 0 {
            return Err("audio clip has no channels".to_string());
        }
        let frames = data[0].len();
        if data.iter().any(|ch| ch.len() != frames) {
            return Err("audio clip channels have mismatched lengths".to_string());
        }
        Ok(Self {
            sample_rate,
            channels,
            frames,
            data,
        })
    }

    /// Clip length in seconds.
    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate <= 0.0 {
            return 0.0;
        }
        self.frames as f64 / self.sample_rate as f64
    }

    /// Compute a min/max waveform summary for display, downsampled to at most
    /// `buckets` columns. Each bucket holds the minimum and maximum of the
    /// mono-mixed signal across the frames it covers — the standard "filled
    /// waveform" the UI draws as a vertical bar per column.
    ///
    /// Runs on the control thread (it's called once at load and allocates); it is
    /// not part of the realtime path.
    pub fn waveform(&self, buckets: usize) -> Waveform {
        // Degenerate inputs: no frames or no buckets requested.
        if self.frames == 0 || buckets == 0 {
            return Waveform {
                min: Vec::new(),
                max: Vec::new(),
            };
        }

        // One bucket can't be wider than the clip; cap so we don't emit empty
        // trailing buckets for very short clips.
        let buckets = buckets.min(self.frames);
        let mut min = Vec::with_capacity(buckets);
        let mut max = Vec::with_capacity(buckets);

        let inv_channels = 1.0 / self.channels as f32;
        for b in 0..buckets {
            // Frame range [start, end) for this bucket. Using a 64-bit product
            // keeps the arithmetic exact for long clips.
            let start = (b as u64 * self.frames as u64 / buckets as u64) as usize;
            let end = ((b as u64 + 1) * self.frames as u64 / buckets as u64) as usize;
            let end = end.max(start + 1); // never an empty bucket

            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for f in start..end {
                // Mono mix = average of channels. Cheap and good enough for a
                // display summary.
                let mut acc = 0.0;
                for ch in &self.data {
                    acc += ch[f];
                }
                let m = acc * inv_channels;
                lo = lo.min(m);
                hi = hi.max(m);
            }
            min.push(lo);
            max.push(hi);
        }

        Waveform { min, max }
    }
}

/// A downsampled waveform summary: `min[i]`/`max[i]` are the extremes of the
/// mono signal in display column `i`. Both vectors have the same length.
pub struct Waveform {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ragged_channels() {
        let err = AudioClip::new(48_000.0, vec![vec![0.0; 10], vec![0.0; 9]]);
        assert!(err.is_err(), "ragged channels should be rejected");
    }

    #[test]
    fn rejects_no_channels() {
        assert!(AudioClip::new(48_000.0, Vec::new()).is_err());
    }

    #[test]
    fn duration_is_frames_over_rate() {
        let clip = AudioClip::new(48_000.0, vec![vec![0.0; 24_000]]).unwrap();
        assert!((clip.duration_secs() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn waveform_brackets_the_signal() {
        // A full-scale ramp from -1 to 1 over one channel. Each bucket's min/max
        // must bracket the samples it covers, and the global extremes must reach
        // the signal extremes.
        let n = 1000;
        let ramp: Vec<f32> = (0..n)
            .map(|i| (i as f32 / (n - 1) as f32) * 2.0 - 1.0)
            .collect();
        let clip = AudioClip::new(48_000.0, vec![ramp]).unwrap();

        let wf = clip.waveform(50);
        assert_eq!(wf.min.len(), 50);
        assert_eq!(wf.max.len(), 50);
        for i in 0..50 {
            assert!(
                wf.min[i] <= wf.max[i],
                "min must not exceed max in a bucket"
            );
        }
        // First bucket starts near -1, last bucket ends near +1.
        assert!(wf.min[0] < -0.9, "first bucket should reach near -1");
        assert!(wf.max[49] > 0.9, "last bucket should reach near +1");
    }

    #[test]
    fn waveform_buckets_capped_to_frame_count() {
        // Asking for more buckets than frames must not produce empty buckets.
        let clip = AudioClip::new(48_000.0, vec![vec![0.25; 4]]).unwrap();
        let wf = clip.waveform(100);
        assert_eq!(wf.min.len(), 4, "buckets capped at frame count");
    }
}
