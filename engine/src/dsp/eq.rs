//! [`Eq`] — a 4-band parametric equalizer built from [`Biquad`]s.
//!
//! Four independent bands in series, each a biquad whose kind, frequency, Q, and
//! gain are set from the UI. The default layout is the classic console EQ:
//! low-shelf, two sweepable bells, high-shelf — all starting flat (0 dB), so a
//! fresh EQ is transparent.
//!
//! ## The two problems this module solves (the milestone's real lessons)
//!
//! **1. Coefficients are expensive; parameters change continuously.**
//! Recomputing a biquad's coefficients means `sin`/`cos`/`powf` — too costly to
//! do every sample for four bands. But if we only recomputed them once per audio
//! buffer, a fast knob drag would step the coefficients in big jumps and zipper.
//! The middle path: smooth each parameter ([`SmoothedParam`]) and recompute
//! coefficients once per **[`CONTROL_BLOCK`]-sample sub-block**. That caps the
//! trig cost (one recompute per 16 samples per band) while keeping the
//! coefficient trajectory smooth enough to be click-free. This is also why the
//! UI value and the audio-thread value are *different things*: the UI sets a
//! target; the audio thread chases it.
//!
//! **2. Don't pay for bands that aren't moving.** Each band carries a
//! `needs_coeffs` flag. While a parameter is gliding we recompute every
//! sub-block; once the smoothers have settled onto their targets we stop
//! recomputing entirely (the coefficients are already correct) until the next
//! change. A static EQ costs essentially nothing beyond the per-sample filter.
//!
//! ## Realtime safety
//!
//! All state (the biquads, the smoothers) is fixed-size and lives inside `Eq`;
//! [`Eq::process`] and every setter are allocation-free, lock-free, panic-free.

use super::biquad::{Biquad, BiquadCoeffs, FilterKind};
use super::{Process, SmoothedParam};

/// Number of EQ bands. Four is the classic parametric layout.
pub const NUM_BANDS: usize = 4;

/// How many samples share one coefficient recompute. At 48 kHz, 16 samples is a
/// ~3 kHz control rate — far finer than any audible zipper, and 16× cheaper than
/// recomputing per sample. DSP correctness is independent of this value; it only
/// trades coefficient-update granularity against CPU.
const CONTROL_BLOCK: usize = 16;

/// Parameter glide time. ~20 ms is quick enough that a sweep feels immediate but
/// slow enough that even an extreme jump stays click-free.
const SMOOTH_SECS: f32 = 0.02;

/// One EQ band: a biquad plus the smoothed parameters that feed it.
///
/// `Copy` because every field is (`SmoothedParam` and `Biquad` are both `Copy`),
/// which keeps `Eq` simple to construct and move.
#[derive(Clone, Copy)]
struct Band {
    kind: FilterKind,
    enabled: bool,
    /// Smoothed parameters, advanced at the *control* rate (once per sub-block).
    freq: SmoothedParam,
    q: SmoothedParam,
    gain_db: SmoothedParam,
    filter: Biquad,
    /// While true, recompute coefficients each sub-block. Cleared once the
    /// smoothers settle, set again by any setter.
    needs_coeffs: bool,
}

impl Band {
    fn new(
        sample_rate: f32,
        control_rate: f32,
        kind: FilterKind,
        freq_hz: f32,
        q: f32,
        gain_db: f32,
    ) -> Self {
        Self {
            kind,
            enabled: true,
            // The smoothers run at the control rate (one step per sub-block), so
            // their time constant is expressed against that rate.
            freq: SmoothedParam::new(control_rate, SMOOTH_SECS, freq_hz),
            q: SmoothedParam::new(control_rate, SMOOTH_SECS, q),
            gain_db: SmoothedParam::new(control_rate, SMOOTH_SECS, gain_db),
            filter: Biquad::new(BiquadCoeffs::new(kind, sample_rate, freq_hz, q, gain_db)),
            needs_coeffs: false,
        }
    }

    /// Steady-state coefficients from the *target* (UI) values — what the curve
    /// represents, ignoring any in-flight smoothing.
    fn target_coeffs(&self, sample_rate: f32) -> BiquadCoeffs {
        BiquadCoeffs::new(
            self.kind,
            sample_rate,
            self.freq.target(),
            self.q.target(),
            self.gain_db.target(),
        )
    }
}

/// A 4-band parametric EQ as a single [`Process`] unit.
pub struct Eq {
    sample_rate: f32,
    // Literal `4` (== NUM_BANDS) rather than the const: flutter_rust_bridge's
    // parser can't evaluate a const-named array length when it scans this crate.
    bands: [Band; 4],
}

impl Eq {
    /// Build a flat 4-band EQ for a stream at `sample_rate` Hz. Default layout:
    /// low-shelf @ 120 Hz, bell @ 500 Hz, bell @ 3 kHz, high-shelf @ 8 kHz, all
    /// at 0 dB (transparent).
    pub fn new(sample_rate: f32) -> Self {
        let cr = control_rate(sample_rate);
        Self {
            sample_rate,
            bands: [
                Band::new(sample_rate, cr, FilterKind::LowShelf, 120.0, 0.707, 0.0),
                Band::new(sample_rate, cr, FilterKind::Peak, 500.0, 1.0, 0.0),
                Band::new(sample_rate, cr, FilterKind::Peak, 3_000.0, 1.0, 0.0),
                Band::new(sample_rate, cr, FilterKind::HighShelf, 8_000.0, 0.707, 0.0),
            ],
        }
    }

    /// Set band `i`'s filter kind. No-op if `i` is out of range. Realtime-safe.
    /// A kind change is discrete (it can't be smoothed), so it may click on a
    /// band carrying signal — fine for an occasional user action.
    #[inline]
    pub fn set_band_kind(&mut self, i: usize, kind: FilterKind) {
        if let Some(b) = self.bands.get_mut(i) {
            b.kind = kind;
            b.needs_coeffs = true;
        }
    }

    /// Set band `i`'s center/corner frequency in Hz. Smoothed; realtime-safe.
    #[inline]
    pub fn set_band_freq(&mut self, i: usize, hz: f32) {
        if let Some(b) = self.bands.get_mut(i) {
            b.freq.set_target(hz);
            b.needs_coeffs = true;
        }
    }

    /// Set band `i`'s Q (bandwidth). Smoothed; realtime-safe.
    #[inline]
    pub fn set_band_q(&mut self, i: usize, q: f32) {
        if let Some(b) = self.bands.get_mut(i) {
            b.q.set_target(q);
            b.needs_coeffs = true;
        }
    }

    /// Set band `i`'s gain in dB (peak/shelf only). Smoothed; realtime-safe.
    #[inline]
    pub fn set_band_gain_db(&mut self, i: usize, db: f32) {
        if let Some(b) = self.bands.get_mut(i) {
            b.gain_db.set_target(db);
            b.needs_coeffs = true;
        }
    }

    /// Enable/disable band `i`. A disabled band is skipped entirely (true
    /// bypass). Re-enabling clears the filter memory so stale state can't click.
    #[inline]
    pub fn set_band_enabled(&mut self, i: usize, on: bool) {
        if let Some(b) = self.bands.get_mut(i) {
            if on && !b.enabled {
                b.filter.reset();
                b.needs_coeffs = true;
            }
            b.enabled = on;
        }
    }

    /// Combined magnitude response of all enabled bands at `freq_hz`, in dB,
    /// computed from the target (UI) parameters. This is the authoritative curve
    /// the UI draws (the Dart side mirrors this math). Not on the audio path.
    pub fn response_db(&self, freq_hz: f32) -> f32 {
        self.bands
            .iter()
            .filter(|b| b.enabled)
            .map(|b| {
                b.target_coeffs(self.sample_rate)
                    .magnitude_db(self.sample_rate, freq_hz)
            })
            .sum()
    }
}

impl Process for Eq {
    #[inline]
    fn process(&mut self, block: &mut [&mut [f32]]) {
        let frames = block.first().map_or(0, |c| c.len());
        let mut off = 0;
        // Walk the block in control-rate sub-blocks. Within each, coefficients
        // are fixed (computed once); across them they glide toward their targets.
        while off < frames {
            let n = (frames - off).min(CONTROL_BLOCK);

            // 1. Advance smoothers + recompute coefficients once for this
            //    sub-block, but only for bands that are still moving.
            for band in &mut self.bands {
                if band.enabled && band.needs_coeffs {
                    let f = band.freq.next();
                    let q = band.q.next();
                    let g = band.gain_db.next();
                    band.filter
                        .set_coeffs(BiquadCoeffs::new(band.kind, self.sample_rate, f, q, g));
                    if band.freq.settled() && band.q.settled() && band.gain_db.settled() {
                        // Snap to the exact target and stop recomputing until the
                        // next parameter change.
                        band.freq.snap();
                        band.q.snap();
                        band.gain_db.snap();
                        band.needs_coeffs = false;
                    }
                }
            }

            // 2. Filter the sub-block, channel by channel, each band in series.
            //    Coefficients are constant here; only the per-channel filter
            //    memory advances.
            for (ch, samples) in block.iter_mut().enumerate().take(2) {
                let seg = &mut samples[off..off + n];
                for band in &mut self.bands {
                    if band.enabled {
                        for x in seg.iter_mut() {
                            *x = band.filter.process_sample(ch, *x);
                        }
                    }
                }
            }

            off += n;
        }
    }
}

/// Control (sub-block) rate for a given audio sample rate.
fn control_rate(sample_rate: f32) -> f32 {
    sample_rate / CONTROL_BLOCK as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;
    use std::f32::consts::PI;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| (2.0 * PI * freq * n as f32 / SR).sin())
            .collect()
    }

    /// Level in dB of a buffer's steady-state tail, referenced so a
    /// unit-amplitude (peak = 1.0) sine reads 0 dB. The +3.01 correction comes
    /// from `RMS = peak/√2` for a sine: without it a unit sine would read
    /// −3.01 dB and the "passthrough → 0 dB" assertions below would all be off
    /// by that amount.
    fn rms_db_tail(buf: &[f32]) -> f32 {
        let tail = &buf[buf.len() / 2..];
        let ms = tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32;
        10.0 * ms.max(1e-30).log10() + 10.0 * 2.0_f32.log10()
    }

    /// Push a mono signal through the EQ (as a 1-channel block) and return it.
    fn run_mono(eq: &mut Eq, input: &[f32]) -> Vec<f32> {
        let mut buf = input.to_vec();
        let mut block: [&mut [f32]; 1] = [&mut buf];
        eq.process(&mut block);
        buf
    }

    #[test]
    fn flat_eq_is_transparent() {
        // Default layout is all 0 dB shelves/bells == identity. A tone should
        // pass essentially unchanged at several frequencies.
        let mut eq = Eq::new(SR);
        for &f in &[100.0, 1_000.0, 8_000.0] {
            let out = run_mono(&mut eq, &sine(f, 16384));
            assert!(
                rms_db_tail(&out).abs() < 0.3,
                "flat EQ should pass {f} Hz untouched, got {} dB",
                rms_db_tail(&out)
            );
        }
    }

    #[test]
    fn a_bell_boost_lifts_its_band_in_audio_and_curve() {
        let mut eq = Eq::new(SR);
        // Band 2 is a bell defaulting to 3 kHz; boost it +12 dB and let it settle.
        eq.set_band_gain_db(2, 12.0);
        // Warm up so the smoother reaches the target before we measure.
        let _ = run_mono(&mut eq, &vec![0.0; 8192]);

        let out = rms_db_tail(&run_mono(&mut eq, &sine(3_000.0, 16384)));
        assert!(
            (out - 12.0).abs() < 1.5,
            "3 kHz should be lifted ~12 dB, got {out} dB"
        );

        // The drawn curve must agree with the audio.
        let curve = eq.response_db(3_000.0);
        assert!(
            (curve - 12.0).abs() < 1.0,
            "curve should read ~+12 dB at 3 kHz, got {curve}"
        );
        // A distant frequency stays ~flat.
        assert!(eq.response_db(100.0).abs() < 1.0, "100 Hz should stay flat");
    }

    #[test]
    fn response_sums_across_bands() {
        // Two bells boosting near the same place should sum in dB.
        let mut eq = Eq::new(SR);
        eq.set_band_freq(1, 2_000.0);
        eq.set_band_gain_db(1, 6.0);
        eq.set_band_freq(2, 2_000.0);
        eq.set_band_gain_db(2, 6.0);
        let at_2k = eq.response_db(2_000.0);
        assert!(
            at_2k > 10.0,
            "two +6 dB bells at 2 kHz should sum to ~+12 dB, got {at_2k}"
        );
    }

    #[test]
    fn sweeping_a_cutoff_does_not_click() {
        // The headline stop-signal: sweep a filter during playback with zero
        // clicks. Feed a steady tone, turn band 0 into a low-pass, and sweep its
        // cutoff hard each block. A coefficient discontinuity would show up as a
        // large sample-to-sample jump in the output; smoothing must prevent it.
        let mut eq = Eq::new(SR);
        eq.set_band_kind(0, FilterKind::Lowpass);
        eq.set_band_freq(0, 5_000.0);

        let block = 64;
        let mut prev = 0.0_f32;
        let mut max_jump = 0.0_f32;
        // Sweep cutoff 5 kHz -> 300 Hz over ~1.3 s, measuring output continuity.
        for k in 0..1000 {
            let cutoff = 5_000.0 - (k as f32 / 1000.0) * 4_700.0;
            eq.set_band_freq(0, cutoff);
            let out = run_mono(&mut eq, &sine(440.0, block));
            for &s in &out {
                max_jump = max_jump.max((s - prev).abs());
                prev = s;
            }
        }
        // Adjacent samples of a 440 Hz sine move by at most ~0.06 anyway; allow
        // generous headroom but well below the ~O(1) spike a click would cause.
        assert!(
            max_jump < 0.2,
            "cutoff sweep should be click-free, max jump {max_jump}"
        );
        assert!(
            prev.is_finite(),
            "output must stay finite through the sweep"
        );
    }

    #[test]
    fn disabled_band_is_a_true_bypass() {
        let mut eq = Eq::new(SR);
        eq.set_band_gain_db(2, 12.0);
        eq.set_band_enabled(2, false);
        let _ = run_mono(&mut eq, &vec![0.0; 4096]);
        let out = rms_db_tail(&run_mono(&mut eq, &sine(3_000.0, 16384)));
        assert!(
            out.abs() < 0.3,
            "disabled boost band should not affect audio, got {out} dB"
        );
        assert!(
            eq.response_db(3_000.0).abs() < 0.3,
            "disabled band drops out of the curve"
        );
    }

    #[test]
    fn eq_process_does_not_allocate() {
        let mut eq = Eq::new(SR);
        let mut l = [0.2_f32; 512];
        let mut r = [0.2_f32; 512];
        assert_no_alloc(|| {
            for i in 0..1000 {
                // Churn every parameter as a UI drag would.
                eq.set_band_freq(0, 200.0 + (i % 400) as f32);
                eq.set_band_q(1, 0.5 + (i % 8) as f32 * 0.5);
                eq.set_band_gain_db(2, -12.0 + (i % 24) as f32);
                eq.set_band_enabled(3, i % 2 == 0);
                eq.set_band_kind(
                    1,
                    if i % 3 == 0 {
                        FilterKind::Notch
                    } else {
                        FilterKind::Peak
                    },
                );
                let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
                eq.process(&mut block);
            }
        });
    }
}
