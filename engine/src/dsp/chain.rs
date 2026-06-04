//! [`Chain`] — the per-channel processing chain: EQ → gain → pan.
//!
//! This is the reusable processing unit that sits on *every* mixer node — each
//! track has one (between its source and the mix bus) and so does the master
//! bus (between the summed mix and the output). Pulling it out of `Strip` lets
//! the master reuse the exact same code path as a track strip, which keeps the
//! signal flow predictable: a parameter that smooths cleanly on a track also
//! smooths cleanly on the master.
//!
//! The chain owns no buffers of its own — it processes whatever planar block it
//! is handed in place, like any other [`Process`] unit. The caller (a [`Strip`]
//! or the [`Mixer`]) lends it the scratch buffers each block, which is what
//! keeps the audio thread allocation-free.
//!
//! [`Strip`]: crate::strip::Strip
//! [`Mixer`]: crate::mixer::Mixer

use super::{Eq, FilterKind, Gain, Pan, Process};

/// EQ → gain → pan, in that order. The EQ is pre-fader (shape, then level), and
/// pan sits after the gain so a fade-down doesn't unbalance the field.
pub struct Chain {
    /// The stream rate this chain was built for. Stored so [`Chain::reset`] can
    /// rebuild its child units without the caller having to pass it again.
    sample_rate: f32,
    pub(crate) eq: Eq,
    pub(crate) gain: Gain,
    pub(crate) pan: Pan,
}

impl Chain {
    /// Build a chain for a stream at `sample_rate` Hz. All units start at their
    /// transparent defaults: flat EQ, unity gain, centered pan.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            eq: Eq::new(sample_rate),
            gain: Gain::new(sample_rate, 0.0),
            pan: Pan::new(sample_rate, 0.0),
        }
    }

    /// Snap the whole chain back to its constructor defaults — flat EQ, unity
    /// gain, centered pan — *and* clear filter state so a freshly re-used strip
    /// can't click from stale memory. Used by [`crate::mixer::Mixer::clear_track`]
    /// when the control side asks to remove (and later possibly reuse) a track.
    ///
    /// Realtime-safe: rebuilds child units in place from already-known state, no
    /// allocation — the same pattern as `*self = Self::new(...)` but written out
    /// so it's obvious nothing surprising happens.
    #[inline]
    pub fn reset(&mut self) {
        self.eq = Eq::new(self.sample_rate);
        self.gain = Gain::new(self.sample_rate, 0.0);
        self.pan = Pan::new(self.sample_rate, 0.0);
    }

    /// Set the gain target in decibels. Smoothed on the audio thread.
    #[inline]
    pub fn set_gain_db(&mut self, db: f32) {
        self.gain.set_db(db);
    }

    /// Set the gain target as a raw linear multiplier. Smoothed on the audio thread.
    #[inline]
    pub fn set_gain_linear(&mut self, linear: f32) {
        self.gain.set_linear(linear);
    }

    /// Set the pan target in `[-1, 1]`. Smoothed on the audio thread.
    #[inline]
    pub fn set_pan(&mut self, pan: f32) {
        self.pan.set_pos(pan);
    }

    /// Set EQ band `n`'s filter kind, by integer code.
    #[inline]
    pub fn set_eq_band_kind(&mut self, n: u8, code: u8) {
        self.eq.set_band_kind(n as usize, FilterKind::from_code(code));
    }

    /// Set EQ band `n`'s center/corner frequency, Hz.
    #[inline]
    pub fn set_eq_band_freq(&mut self, n: u8, hz: f32) {
        self.eq.set_band_freq(n as usize, hz);
    }

    /// Set EQ band `n`'s Q.
    #[inline]
    pub fn set_eq_band_q(&mut self, n: u8, q: f32) {
        self.eq.set_band_q(n as usize, q);
    }

    /// Set EQ band `n`'s gain in dB.
    #[inline]
    pub fn set_eq_band_gain_db(&mut self, n: u8, db: f32) {
        self.eq.set_band_gain_db(n as usize, db);
    }

    /// Enable/disable EQ band `n` (true bypass when off).
    #[inline]
    pub fn set_eq_band_enabled(&mut self, n: u8, on: bool) {
        self.eq.set_band_enabled(n as usize, on);
    }
}

impl Process for Chain {
    #[inline]
    fn process(&mut self, block: &mut [&mut [f32]]) {
        self.eq.process(block);
        self.gain.process(block);
        self.pan.process(block);
    }
}
