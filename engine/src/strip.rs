//! [`Strip`] — a channel strip: source → EQ → gain → pan → output, plus a meter.
//!
//! This is the seed of the eventual audio graph. It owns the realtime source
//! ([`WavPlayer`]), the in-order DSP units ([`Eq`], then [`Gain`], then [`Pan`]),
//! the
//! pre-allocated planar scratch buffers the chain processes in, and the
//! post-fader peak meter. The audio callback owns one `Strip` and calls
//! [`Strip::process`] per buffer.
//!
//! ## Why scratch buffers + chunking (Rust/realtime note)
//!
//! cpal hands us an interleaved device buffer of *some* frame count that can
//! vary between callbacks. Our DSP works on planar buffers, so we need somewhere
//! to deinterleave into — but we cannot allocate on the audio thread. So the
//! strip owns two `Vec<f32>` (left/right) sized to [`MAX_BLOCK`] frames, filled
//! once at construction (before any callback runs) and only ever *sliced* after
//! that. If a callback's block is larger than `MAX_BLOCK`, we process it in
//! `MAX_BLOCK`-frame chunks — still zero allocation, correct for any size.
//!
//! ## Signal flow each chunk
//!
//! 1. [`WavPlayer::render`] fills the planar scratch (`left`, `right`).
//! 2. [`Eq`], then [`Gain`], then [`Pan`] process the scratch in place
//!    (per-sample / per-sub-block smoothed).
//! 3. We interleave the scratch onto the device buffer, and on the way:
//!    - update the post-fader peak meter (one running peak-hold per channel),
//!    - tap the post-fader mono mix into the oscilloscope ring.
//!
//! The meter is **post-fader** (standard): it reflects what you hear after gain
//! and pan, so muting drops the meter to silence.

use std::sync::Arc;

use rtrb::Producer;

use crate::clip::AudioClip;
use crate::commands::Command;
use crate::dsp::{Eq, FilterKind, Gain, Pan, Process};

/// Largest block (in frames) the strip processes in one pass. Device buffers
/// are almost always well under this (128–2048 typical); anything larger is
/// split into chunks. Sizing the scratch to this is a few tens of KB — trivial,
/// and it means the audio thread never needs a bigger buffer than it has.
const MAX_BLOCK: usize = 4096;

/// Default starting level/pan for a fresh strip: unity gain, centered.
const INITIAL_GAIN_DB: f32 = 0.0;
const INITIAL_PAN: f32 = 0.0;

/// Meter release time: how fast the displayed peak falls after a transient.
/// Attack is instantaneous (the meter jumps up immediately on a louder sample);
/// release is a ~300 ms exponential decay so peaks are readable, not flickery.
const METER_RELEASE_SECS: f32 = 0.3;

/// A single channel strip: WAV source, EQ, gain, pan, and a post-fader peak
/// meter.
pub struct Strip {
    player: WavPlayerSource,
    /// 4-band parametric EQ, pre-fader (the standard place: shape the tone, then
    /// set the level).
    eq: Eq,
    gain: Gain,
    pan: Pan,
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
            eq: Eq::new(device_rate),
            gain: Gain::new(device_rate, INITIAL_GAIN_DB),
            pan: Pan::new(device_rate, INITIAL_PAN),
            scratch_l: vec![0.0; MAX_BLOCK],
            scratch_r: vec![0.0; MAX_BLOCK],
            meter_release: (-1.0 / (METER_RELEASE_SECS * device_rate)).exp(),
            peak_l: 0.0,
            peak_r: 0.0,
        }
    }

    /// Route one command to the right place: transport to the player, parameter
    /// changes to the DSP units. Realtime-safe.
    #[inline]
    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play
            | Command::Pause
            | Command::Stop
            | Command::Seek(_)
            | Command::SetLooping(_) => self.player.handle(cmd),
            Command::SetGainDb(db) => self.gain.set_db(db),
            Command::SetGainLinear(lin) => self.gain.set_linear(lin),
            Command::SetPan(p) => self.pan.set_pos(p),
            Command::SetEqBandKind(n, code) => {
                self.eq.set_band_kind(n as usize, FilterKind::from_code(code))
            }
            Command::SetEqBandFreq(n, hz) => self.eq.set_band_freq(n as usize, hz),
            Command::SetEqBandQ(n, q) => self.eq.set_band_q(n as usize, q),
            Command::SetEqBandGainDb(n, db) => self.eq.set_band_gain_db(n as usize, db),
            Command::SetEqBandEnabled(n, on) => self.eq.set_band_enabled(n as usize, on),
        }
    }

    /// Swap in a new clip, returning the displaced one (undropped — see
    /// [`WavPlayer::set_clip`](crate::player::WavPlayer::set_clip)).
    #[inline]
    pub fn set_clip(&mut self, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.player.set_clip(clip)
    }

    /// Current playhead position in clip frames. Realtime-safe.
    #[inline]
    pub fn pos_frames(&self) -> i64 {
        self.player.pos_frames()
    }

    /// Whether playback is currently advancing. Realtime-safe.
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

    /// Render the strip into the interleaved device buffer `data` (`channels`
    /// interleaved samples per frame), tapping the post-fader mono mix into the
    /// scope ring. Splits oversized buffers into [`MAX_BLOCK`]-frame chunks.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free.
    #[inline]
    pub fn process(&mut self, data: &mut [f32], channels: usize, scope_tx: &mut Producer<f32>) {
        if channels == 0 {
            return;
        }
        // Walk the device buffer one chunk (≤ MAX_BLOCK frames) at a time.
        for chunk in data.chunks_mut(MAX_BLOCK * channels) {
            let frames = chunk.len() / channels;
            self.process_chunk(chunk, frames, channels, scope_tx);
        }
    }

    /// Process exactly `frames` frames (guaranteed ≤ [`MAX_BLOCK`]) into one
    /// slice of the device buffer.
    #[inline]
    fn process_chunk(
        &mut self,
        chunk: &mut [f32],
        frames: usize,
        channels: usize,
        scope_tx: &mut Producer<f32>,
    ) {
        let left = &mut self.scratch_l[..frames];
        let right = &mut self.scratch_r[..frames];

        // 1. Source -> planar scratch.
        self.player.render(left, right);

        // 2. EQ, then gain, then pan, in place. `block` borrows the two scratch
        //    slices; the borrow checker guarantees the units can't alias them.
        let mut block: [&mut [f32]; 2] = [left, right];
        self.eq.process(&mut block);
        self.gain.process(&mut block);
        self.pan.process(&mut block);

        // 3. Interleave onto the device buffer, updating the meter and scope as
        //    we go. Re-borrow the scratch immutably now the DSP is done.
        let left = &self.scratch_l[..frames];
        let right = &self.scratch_r[..frames];
        for (f, frame) in chunk.chunks_mut(channels).enumerate() {
            let l = left[f];
            let r = right[f];

            // Post-fader peak meter: instant attack (jump to a louder sample),
            // exponential release otherwise.
            self.peak_l = (self.peak_l * self.meter_release).max(l.abs());
            self.peak_r = (self.peak_r * self.meter_release).max(r.abs());

            // Spread stereo onto the device's channel layout: mono device gets
            // the downmix; stereo maps 1:1; extra channels get silence.
            if channels == 1 {
                frame[0] = (l + r) * 0.5;
            } else {
                for (c, out) in frame.iter_mut().enumerate() {
                    *out = match c {
                        0 => l,
                        1 => r,
                        _ => 0.0,
                    };
                }
            }

            // Post-fader mono mix to the scope (wait-free; full ring just drops).
            let _ = scope_tx.push((l + r) * 0.5);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::scope_channel;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    /// A constant-amplitude stereo clip — easy to reason about through gain/pan.
    fn const_clip(amp: f32, frames: usize) -> Arc<AudioClip> {
        Arc::new(AudioClip::new(SR, vec![vec![amp; frames], vec![amp; frames]]).unwrap())
    }

    #[test]
    fn meter_responds_to_audio_then_decays() {
        let (mut scope_tx, _r) = scope_channel();
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle(Command::Play);

        // Render a block of full-ish signal: the meter should read clearly > 0.
        let mut data = [0.0_f32; 1024]; // 512 stereo frames
        strip.process(&mut data, 2, &mut scope_tx);
        assert!(
            strip.peak_left() > 0.5 && strip.peak_right() > 0.5,
            "meter should rise with audio, got L={} R={}",
            strip.peak_left(),
            strip.peak_right()
        );

        // Stop and render silence for ~2 s; the meter must decay toward zero.
        strip.handle(Command::Stop);
        for _ in 0..200 {
            strip.process(&mut data, 2, &mut scope_tx);
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
        let (mut scope_tx, _r) = scope_channel();
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle(Command::Play);
        strip.handle(Command::SetGainDb(-120.0)); // hard mute

        // The gain smoother mutes the signal within ~10 ms; after that the meter
        // only releases (300 ms time constant). Run ~2 s of audio so the held
        // peak has decayed well below the threshold.
        let mut data = [0.0_f32; 1024];
        for _ in 0..200 {
            strip.process(&mut data, 2, &mut scope_tx);
        }
        assert!(
            strip.peak_left() < 0.01,
            "muted strip should meter ~0, got {}",
            strip.peak_left()
        );
        // And the device buffer is actually silent at the tail.
        assert!(data[data.len() - 1].abs() < 1e-3);
    }

    #[test]
    fn hard_left_pan_silences_right_output() {
        let (mut scope_tx, _r) = scope_channel();
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.8, 96_000));
        strip.handle(Command::Play);
        strip.handle(Command::SetPan(-1.0));

        let mut data = [0.0_f32; 2048];
        for _ in 0..50 {
            strip.process(&mut data, 2, &mut scope_tx);
        }
        // Odd (right) interleaved samples should be ~silent; left ones audible.
        let last_l = data[data.len() - 2];
        let last_r = data[data.len() - 1];
        assert!(
            last_l.abs() > 0.3,
            "left should carry the signal, got {last_l}"
        );
        assert!(
            last_r.abs() < 0.01,
            "hard-left pan should silence right, got {last_r}"
        );
    }

    #[test]
    fn handles_blocks_larger_than_max_block_in_chunks() {
        // A device buffer bigger than MAX_BLOCK must still render fully (chunked)
        // without panicking or going out of bounds.
        let (mut scope_tx, _r) = scope_channel();
        let mut strip = Strip::new(SR);
        strip.set_clip(const_clip(0.5, MAX_BLOCK * 4));
        strip.handle(Command::Play);

        let frames = MAX_BLOCK + 777; // not a multiple of MAX_BLOCK
        let mut data = vec![0.0_f32; frames * 2];
        strip.process(&mut data, 2, &mut scope_tx);
        assert!(
            data.iter().any(|&s| s != 0.0),
            "oversized block should render"
        );
        assert_eq!(strip.pos_frames(), frames as i64);
    }

    #[test]
    fn strip_process_does_not_allocate() {
        // The full strip path — command routing, planar render, gain, pan, meter,
        // interleave, scope tap — under assert_no_alloc, with parameter changes
        // and a clip swap, as the real callback exercises it.
        let (mut scope_tx, _r) = scope_channel();
        let mut strip = Strip::new(SR);
        let clip = const_clip(0.7, 50_000);
        let mut data = [0.0_f32; 1024];

        assert_no_alloc(|| {
            strip.set_clip(clip); // move in; no alloc (Option::replace)
            strip.handle(Command::Play);
            for i in 0..1000 {
                strip.handle(Command::SetGainDb(-(i % 24) as f32));
                strip.handle(Command::SetPan(((i % 200) as f32 / 100.0) - 1.0));
                strip.process(&mut data, 2, &mut scope_tx);
            }
        });
    }
}
