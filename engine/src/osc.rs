//! A minimal sine oscillator — the dumbest possible synth.
//!
//! The point of the starter is to prove the realtime path end-to-end, not to
//! make a nice sound. The real engine will have wavetable / PolyBLEP / etc.;
//! none of that matters until the plumbing works.
//!
//! Realtime-safety note (for the Rust beginner): every method here is
//! allocation-free, lock-free, and panic-free. `next_sample` only does
//! arithmetic on stack data, so it is safe to call from inside the audio
//! callback. There is nothing here that can block, lock, or grow a heap buffer.

use std::f64::consts::TAU;

/// Sine oscillator with a one-pole-smoothed frequency.
///
/// We keep the phase in `f64` because at 48 kHz the per-sample phase increment
/// is tiny, and an `f32` accumulator drifts audibly over seconds. The output is
/// `f32` (our internal sample format).
///
/// Frequency is *smoothed*: `set_target_hz` only moves a target, and each
/// sample `current_hz` glides toward it. That glide is what turns a slider drag
/// into a smooth pitch bend instead of a click/zipper. This is the same
/// parameter-smoothing pattern the real engine will use for every parameter
/// (gains, cutoffs, …), so it's worth understanding here where it's simplest.
pub struct SineOsc {
    sample_rate: f32,
    /// Phase accumulator in radians, wrapped to `[0, TAU)`.
    phase: f64,
    /// The frequency actually being played *this* sample.
    current_hz: f32,
    /// Where `current_hz` is gliding toward.
    target_hz: f32,
    /// One-pole smoothing coefficient per sample (0..1). Higher = faster glide.
    smoothing: f32,
}

impl SineOsc {
    /// Create an oscillator at `sample_rate`, already sitting at `initial_hz`
    /// (no glide on the very first frequency — `current` and `target` match).
    pub fn new(sample_rate: f32, initial_hz: f32) -> Self {
        Self {
            sample_rate,
            phase: 0.0,
            current_hz: initial_hz,
            target_hz: initial_hz,
            smoothing: Self::smoothing_for(sample_rate),
        }
    }

    /// One-pole coefficient giving a ~5 ms time constant, computed from the
    /// sample rate so the glide *time* stays constant across sample rates.
    /// `1 - e^(-1 / (t * fs))` is the standard one-pole-from-time-constant form.
    fn smoothing_for(sample_rate: f32) -> f32 {
        const GLIDE_SECONDS: f32 = 0.005;
        1.0 - (-1.0 / (GLIDE_SECONDS * sample_rate)).exp()
    }

    /// Set the frequency the oscillator should glide toward. Cheap and
    /// realtime-safe — the actual ramp happens per-sample in `next_sample`.
    #[inline]
    pub fn set_target_hz(&mut self, hz: f32) {
        self.target_hz = hz;
    }

    /// Render exactly one mono sample and advance the phase. Allocation-free,
    /// branch-light, safe on the audio thread.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        // 1. Glide the live frequency toward the target (click-free).
        self.current_hz += (self.target_hz - self.current_hz) * self.smoothing;

        // 2. Output sin(phase). (Compute before advancing so a freshly
        //    constructed osc starts at sin(0) = 0.)
        let out = self.phase.sin() as f32;

        // 3. Advance phase, wrapping at TAU to keep `f64` precision high.
        self.phase += self.current_hz as f64 * (TAU / self.sample_rate as f64);
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        out
    }

    /// Render a block of mono samples into `out`. Convenience for tests and
    /// block-based callers; just loops `next_sample`. Allocation-free.
    ///
    /// Currently exercised only by the unit tests — the audio callback renders
    /// per-frame so it can fan out to N channels. Kept because block rendering
    /// is the natural shape for later milestones (and offline render). The crate
    /// is a `cdylib`, so `pub` alone doesn't mark it reachable to the dead-code
    /// lint; the explicit allow documents that this is intentional.
    #[allow(dead_code)]
    pub fn process(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            *s = self.next_sample();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    #[test]
    fn produces_correct_frequency_and_amplitude() {
        // 100 ms of 440 Hz at 48 kHz. Constructed at 440 so there's no glide —
        // the output is a clean 440 Hz sine from sample 0.
        let mut osc = SineOsc::new(SR, 440.0);
        let len = (SR * 0.1) as usize; // 4800 samples
        let mut buf = vec![0.0_f32; len];
        osc.process(&mut buf);

        // Peak should reach ~±1.0 (a full-scale sine).
        let peak = buf.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!((peak - 1.0).abs() < 0.01, "expected peak ~1.0, got {peak}");

        // Zero-crossing count: 440 Hz over 0.1 s = 44 cycles = 88 crossings.
        // Allow ±1 for boundary effects at the buffer edges.
        let crossings = buf
            .windows(2)
            .filter(|w| (w[0] <= 0.0 && w[1] > 0.0) || (w[0] >= 0.0 && w[1] < 0.0))
            .count();
        assert!(
            (crossings as i32 - 88).abs() <= 1,
            "expected ~88 zero crossings for 440 Hz/0.1 s, got {crossings}"
        );
    }

    #[test]
    fn frequency_glide_is_smooth_no_jumps() {
        // Start at 100 Hz, jump the target to 1000 Hz, and confirm the live
        // frequency ramps rather than snapping — i.e. consecutive samples never
        // differ by more than a small amount (no zipper/click).
        let mut osc = SineOsc::new(SR, 100.0);
        osc.set_target_hz(1000.0);
        let mut buf = vec![0.0_f32; 4800];
        osc.process(&mut buf);
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0_f32, f32::max);
        // At 1 kHz/48 kHz the largest legitimate sample-to-sample delta is well
        // under 0.2; a discontinuity from an un-smoothed jump would exceed this.
        assert!(
            max_step < 0.2,
            "sample step too large ({max_step}); glide not smooth"
        );
    }

    #[test]
    fn rendering_does_not_allocate() {
        // The realtime-safety contract: rendering must never touch the heap.
        // `assert_no_alloc` aborts if anything allocates inside the closure
        // (the cfg(test) global allocator in lib.rs is what arms this check).
        let mut osc = SineOsc::new(SR, 440.0);
        let mut buf = [0.0_f32; 512]; // pre-allocated, like the real callback
        assert_no_alloc(|| {
            // Stress it: 1000 buffer renders, with a frequency change each time
            // (the path the audio callback actually exercises).
            for i in 0..1000 {
                osc.set_target_hz(200.0 + (i % 800) as f32);
                osc.process(&mut buf);
            }
        });
    }
}
