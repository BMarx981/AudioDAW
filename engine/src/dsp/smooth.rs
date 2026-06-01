//! [`SmoothedParam`] — a one-pole-smoothed parameter value.
//!
//! This generalizes the frequency glide in [`crate::osc::SineOsc`] into a
//! reusable helper. The pattern: a parameter has a *target* (set cheaply from a
//! control message) and a *current* value that glides toward it one sample at a
//! time. Reading [`SmoothedParam::next`] once per output frame yields a smooth
//! ramp instead of a step, which is what keeps parameter changes click-free.
//!
//! ## Why one-pole
//!
//! Each sample we move a fixed *fraction* of the remaining distance:
//! `current += (target - current) * coeff`. That's a one-pole low-pass on the
//! control signal. It approaches the target asymptotically (never exactly equal,
//! but within any epsilon after a few time constants) and has no overshoot. The
//! coefficient is derived from a time constant in seconds so the glide *duration*
//! is independent of sample rate — same feel at 44.1 and 96 kHz.

/// A parameter value that glides toward its target with a one-pole filter.
///
/// All methods are allocation-free and safe to call on the audio thread.
#[derive(Clone, Copy, Debug)]
pub struct SmoothedParam {
    /// Value emitted *this* sample.
    current: f32,
    /// Where `current` is gliding toward.
    target: f32,
    /// One-pole coefficient in `(0, 1]`; higher = faster glide.
    coeff: f32,
}

impl SmoothedParam {
    /// Create a parameter sitting exactly at `initial` (no glide on the first
    /// value — `current` and `target` match), smoothing with the given time
    /// constant. `time_secs` is roughly the time to cover ~63% of a jump; the
    /// value is within ~1% of a new target after about 5× that.
    pub fn new(sample_rate: f32, time_secs: f32, initial: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            coeff: Self::coeff_for(sample_rate, time_secs),
        }
    }

    /// Standard one-pole-from-time-constant coefficient: `1 - e^(-1/(t·fs))`.
    /// Guarded so a zero/negative time constant means "no smoothing" (snap).
    fn coeff_for(sample_rate: f32, time_secs: f32) -> f32 {
        if time_secs <= 0.0 || sample_rate <= 0.0 {
            return 1.0;
        }
        1.0 - (-1.0 / (time_secs * sample_rate)).exp()
    }

    /// Aim the parameter at a new value. Cheap and realtime-safe — the actual
    /// glide happens per-sample in [`SmoothedParam::next`].
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Advance one sample and return the new current value. Call once per output
    /// frame. Allocation-free, branch-free.
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.current += (self.target - self.current) * self.coeff;
        self.current
    }

    /// The value emitted on the last [`next`](Self::next) (the audio-rate value).
    #[inline]
    pub fn current(&self) -> f32 {
        self.current
    }

    /// The value being glided toward (the UI-rate target).
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Whether `current` has effectively reached `target`. Uses a small relative
    /// tolerance so it works across scales (Hz in the thousands vs. a Q near 1).
    /// Lets a caller stop recomputing expensive derived values once a glide ends.
    #[inline]
    pub fn settled(&self) -> bool {
        (self.current - self.target).abs() <= 1e-4 * self.target.abs().max(1.0)
    }

    /// Snap `current` exactly onto `target` (no glide). Handy once a parameter
    /// has settled, to avoid an asymptotic value that never quite arrives.
    #[inline]
    pub fn snap(&mut self) {
        self.current = self.target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    #[test]
    fn first_value_has_no_glide() {
        // Constructed at a value, target matches -> next() returns it unchanged.
        let mut p = SmoothedParam::new(SR, 0.008, 0.5);
        assert!(
            (p.next() - 0.5).abs() < 1e-9,
            "no glide before a new target"
        );
    }

    #[test]
    fn ramps_toward_target_within_a_bounded_number_of_samples() {
        // Jump the target and confirm we converge to within tolerance inside a
        // bounded window. An 8 ms time constant settles to within ~0.06% of the
        // target after ~7.5 time constants (60 ms ≈ 2880 samples at 48 kHz);
        // assert it's within 0.2% by then — comfortably true, and a step that
        // failed to ramp would never get there.
        let mut p = SmoothedParam::new(SR, 0.008, 0.0);
        p.set_target(1.0);

        let bound = (SR * 0.06) as usize; // 60 ms
        let mut settled_at = None;
        let mut last = 0.0_f32;
        for i in 0..bound {
            last = p.next();
            if (last - 1.0).abs() < 2e-3 {
                settled_at = Some(i);
                break;
            }
        }
        assert!(
            settled_at.is_some(),
            "should settle within {bound} samples; got to {last}"
        );
    }

    #[test]
    fn glide_is_monotonic_and_has_no_discontinuities() {
        // The ramp must move toward the target every sample and never jump by
        // more than it did on the first (largest) step — i.e. no discontinuity.
        let mut p = SmoothedParam::new(SR, 0.008, 0.0);
        p.set_target(1.0);

        let mut prev = 0.0_f32;
        let first_step = {
            let v = p.next();
            let s = v - prev;
            prev = v;
            s
        };
        for _ in 0..2000 {
            let v = p.next();
            let step = v - prev;
            assert!(step >= -1e-7, "must not overshoot/reverse (step {step})");
            assert!(
                step <= first_step + 1e-6,
                "no step may exceed the first (step {step} > {first_step})"
            );
            prev = v;
        }
    }

    #[test]
    fn smoothing_does_not_allocate() {
        // Realtime-safety contract (TESTING.md): the per-sample glide is on the
        // audio path, so it must never touch the heap.
        let mut p = SmoothedParam::new(SR, 0.008, 0.0);
        assert_no_alloc(|| {
            for i in 0..10_000 {
                p.set_target((i % 2) as f32);
                let _ = p.next();
            }
        });
    }
}
