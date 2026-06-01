//! [`Biquad`] — a second-order IIR filter, plus the RBJ-cookbook coefficient
//! math that drives it. This is the DSP primitive the 4-band EQ ([`super::eq`])
//! is built from.
//!
//! ## What a biquad is (the one DSP idea to internalize here)
//!
//! A biquad is the workhorse of audio filtering: a "bi-quadratic" transfer
//! function — two poles, two zeros — that, with five coefficients, can be a
//! low-pass, high-pass, band-pass, notch, peaking bell, or shelf. The
//! difference equation (Direct Form I) is:
//!
//! ```text
//! y[n] = b0·x[n] + b1·x[n-1] + b2·x[n-2] − a1·y[n-1] − a2·y[n-2]
//! ```
//!
//! The `b` coefficients form the feed-forward (zeros) part, the `a` the
//! feedback (poles) part. We always store coefficients **normalized** so that
//! `a0 = 1` (every coefficient pre-divided by the raw `a0`), which removes a
//! per-sample divide.
//!
//! ## Two types, split on purpose (Rust note)
//!
//! - [`BiquadCoeffs`] is *just the numbers* — `Copy`, no state. Computing it
//!   involves `sin`/`cos`/`powf`, which are relatively expensive, so we compute
//!   it rarely (when a parameter changes) and reuse it for many samples. This is
//!   the "coefficients are expensive, so cache them" point from the milestone.
//! - [`Biquad`] is *coeffs + per-channel filter memory*. The memory (the
//!   `x[n-1]`, `y[n-1]`, … history) must persist across blocks and must be
//!   **separate per channel**, or the left channel's history would corrupt the
//!   right. It implements [`Process`] so it can sit in a chain like any other
//!   unit.
//!
//! We use the **transposed Direct Form II** structure internally (two state
//! words per channel instead of four) because it has good numerical behavior in
//! `f32` and is the standard choice for audio biquads.
//!
//! ## Denormals (a realtime-safety gotcha specific to IIR filters)
//!
//! A filter with feedback can ring down toward zero forever. As the state words
//! get astronomically small they enter the *subnormal* float range, and on x86
//! arithmetic on subnormals can be 10–100× slower — a silent CPU spike that
//! shows up as audio dropouts long after the sound has faded. We guard against
//! it by flushing the tiny state words to exactly zero each sample (see
//! [`flush_denorm`]). CLAUDE.md calls this out as required for the audio thread.

use std::f32::consts::PI;

use super::Process;

/// The biquad filter shapes the EQ offers. Field-less so it crosses the
/// flutter_rust_bridge boundary as a plain Dart enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    /// Low-pass: passes lows, rolls off highs above the cutoff (12 dB/oct).
    Lowpass,
    /// High-pass: passes highs, rolls off lows below the cutoff (12 dB/oct).
    Highpass,
    /// Band-pass: passes a band around the center, 0 dB peak gain.
    Bandpass,
    /// Notch: rejects a narrow band around the center, flat elsewhere.
    Notch,
    /// Peaking ("bell"): boosts or cuts a band around the center by `gain_db`.
    Peak,
    /// Low shelf: boosts or cuts everything below the corner by `gain_db`.
    LowShelf,
    /// High shelf: boosts or cuts everything above the corner by `gain_db`.
    HighShelf,
}

impl FilterKind {
    /// Map a small integer code to a kind: 0=peak, 1=low-shelf, 2=high-shelf,
    /// 3=low-pass, 4=high-pass, 5=band-pass, 6=notch. Unknown codes fall back to
    /// a transparent peaking bell. The kind crosses the bridge and the command
    /// ring as this code (never the enum) so flutter_rust_bridge never has to see
    /// a `dsp` type — must stay in sync with the Dart `EqFilterKind.code` values.
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => FilterKind::LowShelf,
            2 => FilterKind::HighShelf,
            3 => FilterKind::Lowpass,
            4 => FilterKind::Highpass,
            5 => FilterKind::Bandpass,
            6 => FilterKind::Notch,
            _ => FilterKind::Peak,
        }
    }
}

/// Number of channels of filter memory a single [`Biquad`] carries. The engine
/// bus is stereo; sizing the state to a small fixed array keeps `Biquad` `Copy`
/// and allocation-free.
const MAX_CHANNELS: usize = 2;

/// Smallest positive *normal* `f32`. Anything with magnitude below this is a
/// subnormal; we flush it to zero to dodge the denormal CPU penalty.
const TINY: f32 = f32::MIN_POSITIVE;

/// Flush a subnormal value to exactly zero. A normal value passes through
/// unchanged. Branch-predicts trivially (the input is almost always normal) and
/// never allocates.
#[inline(always)]
fn flush_denorm(x: f32) -> f32 {
    if x.abs() < TINY {
        0.0
    } else {
        x
    }
}

/// The five normalized coefficients of a biquad (`a0` is folded to 1.0). `Copy`
/// and cheap to pass around; expensive only to *create*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    /// A pass-through (identity) filter: output equals input. Used as the
    /// starting/disabled state of a band.
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// Compute coefficients for `kind` at `freq_hz` / `q` / `gain_db`, for a
    /// stream running at `sample_rate` Hz. These are the standard
    /// Robert Bristow-Johnson "Audio EQ Cookbook" formulas.
    ///
    /// Inputs are clamped to a safe range: the frequency to just under Nyquist
    /// (above it the filter is undefined and can blow up), `q` to a sane
    /// positive window, and `gain_db` to ±24 dB. `gain_db` is ignored by the
    /// kinds that don't use it (LP/HP/BP/notch).
    pub fn new(kind: FilterKind, sample_rate: f32, freq_hz: f32, q: f32, gain_db: f32) -> Self {
        if sample_rate <= 0.0 {
            return Self::identity();
        }
        // Clamp to keep the math well-defined: never at/above Nyquist, never a
        // zero/negative Q (division), gain in a musical range.
        let nyquist = sample_rate * 0.5;
        let f0 = freq_hz.clamp(1.0, nyquist * 0.99);
        let q = q.clamp(0.05, 40.0);
        let gain_db = gain_db.clamp(-24.0, 24.0);

        // Shared intermediates.
        let a = 10.0_f32.powf(gain_db / 40.0); // amplitude for shelf/peak (sqrt of linear gain)
        let w0 = 2.0 * PI * f0 / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q);

        // Raw (un-normalized) coefficients per kind, then divide through by a0.
        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterKind::Lowpass => {
                let b1 = 1.0 - cos_w0;
                (
                    b1 * 0.5,
                    b1,
                    b1 * 0.5,
                    1.0 + alpha,
                    -2.0 * cos_w0,
                    1.0 - alpha,
                )
            }
            FilterKind::Highpass => {
                let b1 = -(1.0 + cos_w0);
                (
                    (1.0 + cos_w0) * 0.5,
                    b1,
                    (1.0 + cos_w0) * 0.5,
                    1.0 + alpha,
                    -2.0 * cos_w0,
                    1.0 - alpha,
                )
            }
            FilterKind::Bandpass => {
                // Constant 0 dB peak-gain band-pass.
                (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
            }
            FilterKind::Notch => (
                1.0,
                -2.0 * cos_w0,
                1.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            FilterKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos_w0,
                1.0 - alpha / a,
            ),
            FilterKind::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let ap1 = a + 1.0;
                let am1 = a - 1.0;
                (
                    a * (ap1 - am1 * cos_w0 + two_sqrt_a_alpha),
                    2.0 * a * (am1 - ap1 * cos_w0),
                    a * (ap1 - am1 * cos_w0 - two_sqrt_a_alpha),
                    ap1 + am1 * cos_w0 + two_sqrt_a_alpha,
                    -2.0 * (am1 + ap1 * cos_w0),
                    ap1 + am1 * cos_w0 - two_sqrt_a_alpha,
                )
            }
            FilterKind::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let ap1 = a + 1.0;
                let am1 = a - 1.0;
                (
                    a * (ap1 + am1 * cos_w0 + two_sqrt_a_alpha),
                    -2.0 * a * (am1 + ap1 * cos_w0),
                    a * (ap1 + am1 * cos_w0 - two_sqrt_a_alpha),
                    ap1 - am1 * cos_w0 + two_sqrt_a_alpha,
                    2.0 * (am1 - ap1 * cos_w0),
                    ap1 - am1 * cos_w0 - two_sqrt_a_alpha,
                )
            }
        };

        // Normalize so a0 == 1. `a0` is always > 0 for these forms, but guard
        // anyway rather than risk a NaN reaching the audio thread.
        if a0.abs() < TINY {
            return Self::identity();
        }
        let inv = 1.0 / a0;
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
        }
    }

    /// Magnitude response of this filter at `freq_hz`, in decibels. This is the
    /// exact gain (in dB) the filter applies to a steady sine at that frequency
    /// — the curve the UI draws. Evaluates `|H(e^jω)|` analytically from the
    /// coefficients; not on the audio path (used by tests and offline tools).
    ///
    /// `H(z) = (b0 + b1 z⁻¹ + b2 z⁻²) / (1 + a1 z⁻¹ + a2 z⁻²)`, evaluated on the
    /// unit circle `z = e^jω`.
    pub fn magnitude_db(&self, sample_rate: f32, freq_hz: f32) -> f32 {
        let w = 2.0 * PI * freq_hz / sample_rate;
        let (cos1, sin1) = (w.cos(), w.sin());
        let (cos2, sin2) = ((2.0 * w).cos(), (2.0 * w).sin());

        // e^{-jω} = cos − j·sin, so the imaginary parts pick up a minus sign.
        let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
        let num_im = -(self.b1 * sin1 + self.b2 * sin2);
        let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
        let den_im = -(self.a1 * sin1 + self.a2 * sin2);

        let num_mag2 = num_re * num_re + num_im * num_im;
        let den_mag2 = den_re * den_re + den_im * den_im;
        if den_mag2 < TINY {
            return 0.0;
        }
        // 10·log10 of the *squared* magnitude == 20·log10 of the magnitude.
        10.0 * (num_mag2 / den_mag2).log10()
    }
}

/// One biquad filter with per-channel state. Swap its coefficients with
/// [`Biquad::set_coeffs`] (state is preserved, so a coefficient change mid-stream
/// doesn't reset the ringing); process audio with [`Process::process`].
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    coeffs: BiquadCoeffs,
    /// Transposed-DF2 state: two words per channel (`z1`, `z2`). The length is a
    /// literal `2` rather than `MAX_CHANNELS` because flutter_rust_bridge's source
    /// parser can't evaluate a const-named array length (it bails with "Cannot
    /// parse array length" and then panics) when it scans this crate. Keep it ==
    /// `MAX_CHANNELS`.
    z: [[f32; 2]; 2],
}

impl Biquad {
    /// A biquad with the given coefficients and zeroed state.
    pub fn new(coeffs: BiquadCoeffs) -> Self {
        Self {
            coeffs,
            z: [[0.0; 2]; MAX_CHANNELS],
        }
    }

    /// A pass-through biquad (identity coefficients).
    pub fn identity() -> Self {
        Self::new(BiquadCoeffs::identity())
    }

    /// Replace the coefficients, keeping the filter memory. Cheap (a struct
    /// copy); realtime-safe. Smooth coefficient changes are the caller's job —
    /// jumping coefficients far in one step can click on a resonant filter.
    #[inline]
    pub fn set_coeffs(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    /// The current coefficients (for inspection/tests).
    #[inline]
    pub fn coeffs(&self) -> BiquadCoeffs {
        self.coeffs
    }

    /// Filter one sample on channel `ch`. Transposed Direct Form II. Inlined and
    /// allocation-free — this is the per-sample hot path.
    #[inline(always)]
    pub fn process_sample(&mut self, ch: usize, x: f32) -> f32 {
        let c = &self.coeffs;
        let z = &mut self.z[ch];
        // y = b0·x + z1
        // z1 = b1·x − a1·y + z2
        // z2 = b2·x − a2·y
        let y = c.b0 * x + z[0];
        z[0] = flush_denorm(c.b1 * x - c.a1 * y + z[1]);
        z[1] = flush_denorm(c.b2 * x - c.a2 * y);
        y
    }

    /// Zero the filter memory (e.g. before reusing on a different signal).
    #[inline]
    pub fn reset(&mut self) {
        self.z = [[0.0; 2]; MAX_CHANNELS];
    }
}

impl Process for Biquad {
    #[inline]
    fn process(&mut self, block: &mut [&mut [f32]]) {
        // Filter each channel independently, using that channel's own state.
        // Channels beyond MAX_CHANNELS are passed through untouched.
        for (ch, samples) in block.iter_mut().enumerate().take(MAX_CHANNELS) {
            for x in samples.iter_mut() {
                *x = self.process_sample(ch, *x);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    /// `len` samples of a unit-amplitude sine at `freq` Hz.
    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| (2.0 * PI * freq * n as f32 / SR).sin())
            .collect()
    }

    /// RMS in dB of a buffer's steady-state tail (skips the filter's startup
    /// transient so we measure the settled response).
    fn rms_db_tail(buf: &[f32]) -> f32 {
        let tail = &buf[buf.len() / 2..];
        let ms = tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32;
        10.0 * ms.max(1e-30).log10()
    }

    /// Run a sine through a single-channel biquad and return the output.
    fn run(coeffs: BiquadCoeffs, freq: f32, len: usize) -> Vec<f32> {
        let mut bq = Biquad::new(coeffs);
        sine(freq, len)
            .iter()
            .map(|&x| bq.process_sample(0, x))
            .collect()
    }

    #[test]
    fn lowpass_passes_lows_and_attenuates_highs() {
        let lp = BiquadCoeffs::new(FilterKind::Lowpass, SR, 1_000.0, 0.707, 0.0);
        // A 100 Hz tone (well below cutoff) passes ~unchanged.
        let low = run(lp, 100.0, 8192);
        assert!(
            rms_db_tail(&low) > -1.0,
            "100 Hz should pass a 1 kHz LP nearly untouched, got {} dB",
            rms_db_tail(&low)
        );
        // A 10 kHz tone (a decade above cutoff) is hammered (~−40 dB at 12/oct).
        let high = run(lp, 10_000.0, 8192);
        assert!(
            rms_db_tail(&high) < -20.0,
            "10 kHz should be attenuated >20 dB by a 1 kHz LP, got {} dB",
            rms_db_tail(&high)
        );
    }

    #[test]
    fn highpass_passes_highs_and_attenuates_lows() {
        let hp = BiquadCoeffs::new(FilterKind::Highpass, SR, 1_000.0, 0.707, 0.0);
        let high = run(hp, 10_000.0, 8192);
        assert!(rms_db_tail(&high) > -1.0, "10 kHz should pass a 1 kHz HP");
        let low = run(hp, 100.0, 8192);
        assert!(
            rms_db_tail(&low) < -20.0,
            "100 Hz should be attenuated >20 dB by a 1 kHz HP, got {} dB",
            rms_db_tail(&low)
        );
    }

    #[test]
    fn bandpass_peaks_at_center_and_rejects_far_tones() {
        let bp = BiquadCoeffs::new(FilterKind::Bandpass, SR, 1_000.0, 2.0, 0.0);
        let center = rms_db_tail(&run(bp, 1_000.0, 8192));
        let below = rms_db_tail(&run(bp, 100.0, 8192));
        let above = rms_db_tail(&run(bp, 10_000.0, 8192));
        assert!(
            center > below + 15.0 && center > above + 15.0,
            "band-pass should favor center: center {center}, below {below}, above {above}"
        );
    }

    #[test]
    fn peak_boost_lifts_the_center_by_the_set_gain() {
        // A +12 dB bell at 1 kHz should raise a 1 kHz tone by ~12 dB and leave a
        // distant tone (100 Hz) essentially flat.
        let bell = BiquadCoeffs::new(FilterKind::Peak, SR, 1_000.0, 1.0, 12.0);
        let at_center = rms_db_tail(&run(bell, 1_000.0, 16384));
        let far = rms_db_tail(&run(bell, 100.0, 16384));
        assert!(
            (at_center - 12.0).abs() < 1.0,
            "peak should boost center by ~12 dB, got {at_center} dB"
        );
        assert!(
            far.abs() < 1.0,
            "peak should leave 100 Hz flat, got {far} dB"
        );
    }

    #[test]
    fn shelves_boost_their_side_and_leave_the_other_flat() {
        // Low shelf +12 dB lifts the lows, leaves the far highs alone.
        let ls = BiquadCoeffs::new(FilterKind::LowShelf, SR, 1_000.0, 0.707, 12.0);
        assert!(
            (rms_db_tail(&run(ls, 80.0, 16384)) - 12.0).abs() < 1.5,
            "low shelf should lift lows ~12 dB"
        );
        assert!(
            rms_db_tail(&run(ls, 16_000.0, 16384)).abs() < 1.5,
            "low shelf should leave highs flat"
        );
        // High shelf is the mirror image.
        let hs = BiquadCoeffs::new(FilterKind::HighShelf, SR, 1_000.0, 0.707, 12.0);
        assert!(
            (rms_db_tail(&run(hs, 16_000.0, 16384)) - 12.0).abs() < 1.5,
            "high shelf should lift highs ~12 dB"
        );
        assert!(
            rms_db_tail(&run(hs, 80.0, 16384)).abs() < 1.5,
            "high shelf should leave lows flat"
        );
    }

    #[test]
    fn measured_response_matches_the_analytic_magnitude() {
        // The analytic `magnitude_db` is what the UI draws; the time-domain
        // filter is what you hear. They must agree, or the curve lies. Cross-
        // check several kinds/frequencies: process a tone, measure its RMS gain,
        // and compare to magnitude_db at that frequency.
        let cases = [
            (FilterKind::Lowpass, 1_000.0, 0.707, 0.0, 500.0),
            (FilterKind::Lowpass, 1_000.0, 0.707, 0.0, 3_000.0),
            (FilterKind::Peak, 2_000.0, 1.5, 9.0, 2_000.0),
            (FilterKind::Peak, 2_000.0, 1.5, -9.0, 2_000.0),
            (FilterKind::HighShelf, 4_000.0, 0.707, 6.0, 12_000.0),
        ];
        for (kind, f0, q, gain, probe) in cases {
            let c = BiquadCoeffs::new(kind, SR, f0, q, gain);
            let measured = rms_db_tail(&run(c, probe, 32768));
            let analytic = c.magnitude_db(SR, probe);
            assert!(
                (measured - analytic).abs() < 0.6,
                "{kind:?} @ {probe} Hz: measured {measured:.2} dB vs analytic {analytic:.2} dB"
            );
        }
    }

    #[test]
    fn identity_is_a_pass_through() {
        let input = sine(1_000.0, 1024);
        let out = run(BiquadCoeffs::identity(), 1_000.0, 1024);
        for (i, o) in input.iter().zip(out.iter()) {
            assert!(
                (i - o).abs() < 1e-6,
                "identity must pass the signal through"
            );
        }
    }

    #[test]
    fn coefficients_are_stable_poles_inside_unit_circle() {
        // For a stable biquad the poles lie inside the unit circle, which (for
        // real coeffs) means |a2| < 1 and |a1| < 1 + a2. Check across extremes.
        for &f0 in &[20.0, 200.0, 2_000.0, 20_000.0] {
            for &q in &[0.1, 0.707, 8.0, 40.0] {
                for kind in [
                    FilterKind::Lowpass,
                    FilterKind::Highpass,
                    FilterKind::Bandpass,
                    FilterKind::Notch,
                    FilterKind::Peak,
                    FilterKind::LowShelf,
                    FilterKind::HighShelf,
                ] {
                    let c = BiquadCoeffs::new(kind, SR, f0, q, 18.0);
                    assert!(
                        c.a2.abs() < 1.0,
                        "{kind:?} f0={f0} q={q}: |a2| {} >= 1",
                        c.a2
                    );
                    assert!(
                        c.a1.abs() < 1.0 + c.a2 + 1e-4,
                        "{kind:?} f0={f0} q={q}: pole outside unit circle"
                    );
                    assert!(
                        c.b0.is_finite() && c.a1.is_finite(),
                        "coeffs must be finite"
                    );
                }
            }
        }
    }

    #[test]
    fn ringing_state_flushes_to_zero_no_denormals_linger() {
        // Excite a resonant filter, then feed silence. The state must decay to
        // *exactly* zero (denormal flush), not leave subnormal residue that
        // would cost CPU forever.
        let mut bq = Biquad::new(BiquadCoeffs::new(
            FilterKind::Bandpass,
            SR,
            1_000.0,
            20.0,
            0.0,
        ));
        for &x in &sine(1_000.0, 2048) {
            bq.process_sample(0, x);
        }
        for _ in 0..200_000 {
            bq.process_sample(0, 0.0);
        }
        assert_eq!(bq.z[0][0], 0.0, "z1 must flush to exactly 0");
        assert_eq!(bq.z[0][1], 0.0, "z2 must flush to exactly 0");
    }

    #[test]
    fn biquad_process_does_not_allocate() {
        let mut bq = Biquad::new(BiquadCoeffs::new(FilterKind::Peak, SR, 1_000.0, 1.0, 6.0));
        let mut l = [0.3_f32; 512];
        let mut r = [0.3_f32; 512];
        assert_no_alloc(|| {
            for i in 0..1000 {
                // Coefficient swaps happen on the audio thread too — must be alloc-free.
                bq.set_coeffs(BiquadCoeffs::new(
                    FilterKind::Peak,
                    SR,
                    1_000.0 + (i % 100) as f32,
                    1.0,
                    6.0,
                ));
                let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
                bq.process(&mut block);
            }
        });
    }
}
