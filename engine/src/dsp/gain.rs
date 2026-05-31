//! [`Gain`] — a smoothed level control.
//!
//! The control value crosses the bridge in **decibels** (fader-natural for the
//! UI; see CLAUDE.md's "design the API for Dart first"). We convert dB → a
//! linear multiplier here and smooth the *linear* value, so a drag from -20 dB
//! to 0 dB is a continuous fade with no zipper noise.
//!
//! Range: `MIN_DB`..`MAX_DB`. At or below `MIN_DB` the gain is exactly `0.0`
//! (true mute) rather than a tiny-but-audible -60 dB residue.

use super::{Process, SmoothedParam};

/// Anything at or below this is treated as a hard mute (linear 0.0).
pub const MIN_DB: f32 = -60.0;
/// Headroom ceiling for a single track. Currently +12 dB so the gain-fader
/// comparison UI (which offers a −∞…+12 dB option) can drive the signal across
/// its full range; tighten back to +6 once the fader representation is chosen.
pub const MAX_DB: f32 = 12.0;
/// Glide time for level changes. ~8 ms is fast enough to feel instant on a drag
/// yet slow enough to stay click-free across a full-scale jump.
const SMOOTH_SECS: f32 = 0.008;

/// A smoothed linear gain applied equally to every channel.
pub struct Gain {
    /// Smoothed **linear** multiplier (not dB). Smoothing the linear value keeps
    /// the per-sample math to a single multiply.
    gain: SmoothedParam,
}

impl Gain {
    /// Create a gain unit starting at `initial_db`, sitting exactly there (no
    /// glide on the first block).
    pub fn new(sample_rate: f32, initial_db: f32) -> Self {
        Self {
            gain: SmoothedParam::new(sample_rate, SMOOTH_SECS, Self::db_to_linear(initial_db)),
        }
    }

    /// Aim the gain at `db`, clamped to `[MIN_DB, MAX_DB]`. Realtime-safe; the
    /// fade happens per-sample in [`Process::process`].
    #[inline]
    pub fn set_db(&mut self, db: f32) {
        self.gain.set_target(Self::db_to_linear(db));
    }

    /// Aim the gain at a raw linear multiplier, clamped to `[0, MAX]` (where MAX
    /// is the linear equivalent of [`MAX_DB`]). This is the most direct setter —
    /// the unit smooths the linear value internally, so there's no dB round-trip
    /// — and is what the "linear 0…2" fader drives. Realtime-safe.
    #[inline]
    pub fn set_linear(&mut self, linear: f32) {
        self.gain
            .set_target(linear.clamp(0.0, Self::db_to_linear(MAX_DB)));
    }

    /// dB → linear amplitude. `<= MIN_DB` maps to a hard 0.0 (mute); otherwise
    /// `10^(dB/20)`. Clamped at the top to `MAX_DB`.
    #[inline]
    fn db_to_linear(db: f32) -> f32 {
        if db <= MIN_DB {
            return 0.0;
        }
        let db = db.min(MAX_DB);
        10.0_f32.powf(db / 20.0)
    }
}

impl Process for Gain {
    #[inline]
    fn process(&mut self, block: &mut [&mut [f32]]) {
        let frames = block.first().map_or(0, |c| c.len());
        // Frame-outer, channel-inner: advance the smoother exactly once per
        // frame and apply that one value to every channel, so the channels stay
        // sample-locked (a per-channel advance would desync them).
        for f in 0..frames {
            let g = self.gain.next();
            for ch in block.iter_mut() {
                ch[f] *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    /// Run a constant DC block through a gain already settled at `db` (we settle
    /// it by processing a warm-up block first), and return the steady output.
    fn steady_output_at(db: f32) -> f32 {
        let mut g = Gain::new(SR, db);
        let mut l = vec![1.0_f32; 4096];
        let mut r = vec![1.0_f32; 4096];
        // Constructed at `db`, so it's already settled; one block is plenty.
        let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
        g.process(&mut block);
        // Tail of the block is fully settled.
        l[l.len() - 1]
    }

    #[test]
    fn minus_six_db_halves_amplitude() {
        // -6.02 dB is exactly 0.5x; -6 dB is ~0.501x. Assert close to half.
        let out = steady_output_at(-6.0206);
        assert!(
            (out - 0.5).abs() < 1e-3,
            "-6 dB should halve amplitude, got {out}"
        );
    }

    #[test]
    fn zero_db_is_unity() {
        let out = steady_output_at(0.0);
        assert!((out - 1.0).abs() < 1e-4, "0 dB should be unity, got {out}");
    }

    #[test]
    fn floor_is_a_hard_mute() {
        let out = steady_output_at(MIN_DB - 10.0);
        assert_eq!(out, 0.0, "at/below the floor, gain must be exactly 0");
    }

    #[test]
    fn set_linear_drives_and_clamps() {
        // A linear setter should pass small values straight through (no log
        // round-trip) and clamp above the MAX_DB-equivalent ceiling.
        let mut g = Gain::new(SR, 0.0);
        g.set_linear(0.5);
        let mut l = vec![1.0_f32; 4096];
        let mut r = vec![1.0_f32; 4096];
        let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
        g.process(&mut block);
        assert!(
            (l[l.len() - 1] - 0.5).abs() < 1e-3,
            "linear 0.5 should pass through"
        );

        // Way above the ceiling clamps to the +MAX_DB linear value (~3.98 at +12).
        let ceiling = 10.0_f32.powf(MAX_DB / 20.0);
        let mut g = Gain::new(SR, MAX_DB); // start settled at the ceiling
        g.set_linear(100.0);
        let mut l = vec![1.0_f32; 4096];
        let mut r = vec![1.0_f32; 4096];
        let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
        g.process(&mut block);
        assert!(
            (l[l.len() - 1] - ceiling).abs() < 1e-2,
            "linear gain should clamp to the {MAX_DB} dB ceiling ({ceiling})"
        );
    }

    #[test]
    fn change_is_smoothed_not_stepped() {
        // Settled at 0 dB (unity), then aim for mute. Across one block the output
        // must descend gradually — consecutive samples never jump by a large
        // amount (no zipper). Use a constant input so any step is the gain's.
        let mut g = Gain::new(SR, 0.0);
        g.set_db(MIN_DB); // -> mute target
        let mut l = vec![1.0_f32; 2048];
        let mut r = vec![1.0_f32; 2048];
        let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
        g.process(&mut block);

        let max_step = l
            .windows(2)
            .map(|w| (w[0] - w[1]).abs())
            .fold(0.0, f32::max);
        assert!(
            max_step < 0.02,
            "gain change not smooth (max step {max_step})"
        );
        // And it actually moved toward mute.
        assert!(l[l.len() - 1] < 0.5, "should be fading toward mute");
    }

    #[test]
    fn gain_does_not_allocate() {
        let mut g = Gain::new(SR, 0.0);
        let mut l = [0.5_f32; 512];
        let mut r = [0.5_f32; 512];
        assert_no_alloc(|| {
            for i in 0..1000 {
                g.set_db(-(i % 24) as f32);
                let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
                g.process(&mut block);
            }
        });
    }
}
