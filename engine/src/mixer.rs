//! [`Mixer`] — the multitrack summing bus.
//!
//! Owns a fixed-size pool of [`Strip`]s and a master [`Chain`]. Every callback:
//! every strip renders into a shared planar accumulator, the master chain
//! processes that sum in place, and the result is interleaved onto the device
//! buffer (with the master peak meter and the oscilloscope tap riding along).
//!
//! ## Track pool sizing — fixed at startup
//!
//! Milestone 4's stop signal is two tracks summed to a master. The pool is sized
//! to [`MAX_TRACKS`] strips, allocated once on the spawning thread, and that's
//! it: adding a track at runtime is just "start using strip slot `n`", never
//! "allocate". This is the realtime-safety rule from CLAUDE.md ("tracks are
//! pre-allocated in a pool, not created on the audio thread") satisfied early —
//! Milestone 5 will expose runtime track management on top of the same pool.
//!
//! Strips with no clip loaded render silence (their player has nothing to
//! source), so an inactive slot costs only the per-sample meter update — cheap.
//!
//! ## Why two scratch layers
//!
//! Each [`Strip`] owns its own L/R scratch (so its chain has somewhere to write
//! the post-EQ/gain/pan signal *before* it's added into the sum). The mixer owns
//! a second L/R scratch — the *master accumulator* — that every strip adds into,
//! then the master chain reads and writes. Both layers are pre-allocated to
//! [`MAX_BLOCK`] frames and never resized.
//!
//! ## Realtime safety
//!
//! Everything in [`Mixer::process`] is allocation-free, lock-free, panic-free:
//! - command routing dispatches to small inline setters
//! - per-strip render writes only to pre-allocated scratch
//! - accumulator zeroing and per-strip add are tight stack loops
//! - master chain processes in place
//! - meter publish + scope tap are wait-free
//!
//! Out-of-range track indices are silently ignored — the audio thread must
//! never panic on malformed input from the control side.

use std::sync::Arc;

use rtrb::Producer;

use crate::clip::AudioClip;
use crate::commands::Command;
use crate::dsp::{Chain, Process};
use crate::strip::{Strip, MAX_BLOCK};

/// Capacity of the strip pool. Two are active for Milestone 4 (one mix, one
/// master); the remaining slots cost only their per-sample meter loop while
/// they hold no clip, and Milestone 5 will expose them. Eight is a small,
/// memory-cheap pool (~64 KB of scratch total) that comfortably covers most
/// "small project" use without forcing a redesign later.
pub const MAX_TRACKS: usize = 8;

/// Same meter time constant the strip uses, so the master meter feels like the
/// track meters.
const METER_RELEASE_SECS: f32 = 0.3;

/// The summing mixer: a fixed pool of strips, a master chain, and the master
/// meter atomics' worth of state. Owned by the audio callback.
pub struct Mixer {
    pub(crate) strips: Vec<Strip>,
    /// The master processing chain (EQ → gain → pan). EQ isn't exposed by the
    /// Milestone 4 UI yet, but the master is a full chain so a future "master
    /// EQ" comes free.
    pub(crate) master: Chain,
    /// Planar L/R accumulator for the sum of every strip — pre-allocated to
    /// [`MAX_BLOCK`] and only ever sliced.
    master_scratch_l: Vec<f32>,
    master_scratch_r: Vec<f32>,
    /// Per-sample release coefficient for the master meter envelope.
    master_meter_release: f32,
    /// Running post-master-fader peak per channel.
    master_peak_l: f32,
    master_peak_r: f32,
}

impl Mixer {
    /// Build a mixer for an output device at `device_rate` Hz. Allocates the
    /// strip pool and the master accumulator now, on the spawning thread, before
    /// any callback runs.
    pub fn new(device_rate: f32) -> Self {
        Self {
            strips: (0..MAX_TRACKS).map(|_| Strip::new(device_rate)).collect(),
            master: Chain::new(device_rate),
            master_scratch_l: vec![0.0; MAX_BLOCK],
            master_scratch_r: vec![0.0; MAX_BLOCK],
            master_meter_release: (-1.0 / (METER_RELEASE_SECS * device_rate)).exp(),
            master_peak_l: 0.0,
            master_peak_r: 0.0,
        }
    }

    /// The mixer's track-pool capacity. Stable for the life of the engine.
    #[inline]
    pub fn track_count(&self) -> usize {
        self.strips.len()
    }

    /// Route one command to the right place — transport broadcasts to every
    /// strip, per-track commands hit the addressed strip, master commands hit
    /// the master chain. Realtime-safe; out-of-range indices are no-ops.
    ///
    /// **Note:** `Command::ClearTrack` is **not** handled here — the audio
    /// callback special-cases it so it can ship the displaced clip to the
    /// retirement ring (which this `Mixer` has no handle on). Anything that
    /// ends up here is treated as a no-op so dispatching is forgiving.
    #[inline]
    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            // Transport is global: one playhead, every player advances in lockstep.
            Command::Play | Command::Pause | Command::Stop | Command::Seek(_) | Command::SetLooping(_) => {
                for s in self.strips.iter_mut() {
                    s.handle_transport(cmd);
                }
            }
            // Handled by the audio callback, not here — see the method doc.
            Command::ClearTrack(_) => {}
            Command::SetTrackGainDb(t, db) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_gain_db(db);
                }
            }
            Command::SetTrackGainLinear(t, lin) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_gain_linear(lin);
                }
            }
            Command::SetTrackPan(t, p) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_pan(p);
                }
            }
            Command::SetTrackEqBandKind(t, b, k) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_eq_band_kind(b, k);
                }
            }
            Command::SetTrackEqBandFreq(t, b, hz) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_eq_band_freq(b, hz);
                }
            }
            Command::SetTrackEqBandQ(t, b, q) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_eq_band_q(b, q);
                }
            }
            Command::SetTrackEqBandGainDb(t, b, db) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_eq_band_gain_db(b, db);
                }
            }
            Command::SetTrackEqBandEnabled(t, b, on) => {
                if let Some(s) = self.strips.get_mut(t as usize) {
                    s.chain.set_eq_band_enabled(b, on);
                }
            }
            Command::SetMasterGainDb(db) => self.master.set_gain_db(db),
            Command::SetMasterGainLinear(lin) => self.master.set_gain_linear(lin),
            Command::SetMasterPan(p) => self.master.set_pan(p),
        }
    }

    /// Swap a new clip into track `t`. Returns the displaced clip (undropped —
    /// the caller must dispose of it off the audio thread) or `None` if the
    /// slot was empty or `t` is out of range.
    #[inline]
    pub fn set_clip(&mut self, t: u8, clip: Arc<AudioClip>) -> Option<Arc<AudioClip>> {
        self.strips.get_mut(t as usize).and_then(|s| s.set_clip(clip))
    }

    /// Clear track `t`: drop its clip, reset its chain to defaults, zero its
    /// meter. Returns the displaced clip (undropped) so the audio callback can
    /// ship it to the retirement ring. Out-of-range indices are no-ops.
    /// Realtime-safe.
    #[inline]
    pub fn clear_track(&mut self, t: u8) -> Option<Arc<AudioClip>> {
        self.strips.get_mut(t as usize).and_then(|s| s.clear())
    }

    /// Current playhead position in frames of the **first** strip — used by the
    /// transport status stream. Every strip advances together (transport is
    /// global), so any strip's position is representative; track 0 is the
    /// canonical readout.
    #[inline]
    pub fn pos_frames(&self) -> i64 {
        self.strips.first().map_or(0, |s| s.pos_frames())
    }

    /// Whether playback is advancing — true if any track is playing. (With a
    /// global transport every loaded track plays together, but this is the
    /// honest "is sound coming out?" answer if some tracks have no clip.)
    #[inline]
    pub fn is_playing(&self) -> bool {
        self.strips.iter().any(|s| s.is_playing())
    }

    /// Latest post-fader peak for the left channel of strip `t`, linear. Returns
    /// `0.0` if `t` is out of range. Realtime-safe.
    #[inline]
    pub fn track_peak_left(&self, t: usize) -> f32 {
        self.strips.get(t).map_or(0.0, |s| s.peak_left())
    }

    /// Latest post-fader peak for the right channel of strip `t`, linear.
    #[inline]
    pub fn track_peak_right(&self, t: usize) -> f32 {
        self.strips.get(t).map_or(0.0, |s| s.peak_right())
    }

    /// Latest master-bus peak for the left channel, linear.
    #[inline]
    pub fn master_peak_left(&self) -> f32 {
        self.master_peak_l
    }

    /// Latest master-bus peak for the right channel, linear.
    #[inline]
    pub fn master_peak_right(&self) -> f32 {
        self.master_peak_r
    }

    /// Render the mix into the interleaved device buffer `data` (`channels`
    /// interleaved samples per frame), tapping the post-master mono mix into the
    /// scope ring. Splits oversized buffers into [`MAX_BLOCK`]-frame chunks.
    ///
    /// **THE REALTIME HOT PATH.** Allocation-free, lock-free, panic-free.
    #[inline]
    pub fn process(&mut self, data: &mut [f32], channels: usize, scope_tx: &mut Producer<f32>) {
        if channels == 0 {
            return;
        }
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
        // 1. Zero the master accumulator — strips ADD into it.
        let mix_l = &mut self.master_scratch_l[..frames];
        let mix_r = &mut self.master_scratch_r[..frames];
        for x in mix_l.iter_mut() {
            *x = 0.0;
        }
        for x in mix_r.iter_mut() {
            *x = 0.0;
        }

        // 2. Sum every strip into the accumulator. Each strip updates its own
        //    post-fader meter as it goes; an empty strip just renders silence.
        for strip in self.strips.iter_mut() {
            strip.render_into(mix_l, mix_r);
        }

        // 3. Master chain processes the sum in place (EQ → gain → pan).
        let mut block: [&mut [f32]; 2] = [mix_l, mix_r];
        self.master.process(&mut block);

        // 4. Interleave onto the device buffer, updating the master meter and
        //    scope tap on the way. Re-borrow the scratch immutably now the chain
        //    is done.
        let mix_l = &self.master_scratch_l[..frames];
        let mix_r = &self.master_scratch_r[..frames];
        for (f, frame) in chunk.chunks_mut(channels).enumerate() {
            let l = mix_l[f];
            let r = mix_r[f];

            // Master post-fader peak: instant attack, exponential release.
            self.master_peak_l = (self.master_peak_l * self.master_meter_release).max(l.abs());
            self.master_peak_r = (self.master_peak_r * self.master_meter_release).max(r.abs());

            // Spread the master mix onto the device's channel layout.
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

            // Post-master mono mix to the scope (wait-free; full ring just drops).
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

    fn const_clip(amp: f32, frames: usize) -> Arc<AudioClip> {
        Arc::new(AudioClip::new(SR, vec![vec![amp; frames], vec![amp; frames]]).unwrap())
    }

    /// Two tracks playing the same clip must sum to noticeably more than one
    /// track playing it — the core mixing-math claim. Comparing two-track output
    /// to one-track output keeps the assertion robust to the chain's per-unit
    /// attenuation (default-pan center is −3 dB, etc.).
    #[test]
    fn two_tracks_sum_into_master() {
        let (mut scope_tx, _r) = scope_channel();
        // Settle for ~85 ms so every smoother in the chain (gain ~8 ms, pan
        // ~8 ms, EQ ~20 ms at control rate) is fully on its target before we
        // measure. One block = 512 frames ≈ 10.7 ms at 48 kHz.
        let settle_blocks = 8;
        let mut data = vec![0.0_f32; 1024];

        // Baseline: one track at amplitude A.
        let mut solo = Mixer::new(SR);
        solo.set_clip(0, const_clip(0.4, 96_000));
        solo.handle(Command::Play);
        for _ in 0..settle_blocks {
            solo.process(&mut data, 2, &mut scope_tx);
        }
        let solo_master = solo.master_peak_left();

        // Now two tracks of the same clip.
        let mut duo = Mixer::new(SR);
        duo.set_clip(0, const_clip(0.4, 96_000));
        duo.set_clip(1, const_clip(0.4, 96_000));
        duo.handle(Command::Play);
        for _ in 0..settle_blocks {
            duo.process(&mut data, 2, &mut scope_tx);
        }
        let duo_master = duo.master_peak_left();

        // Two identical sources should sum to ~2× one (within whatever further
        // attenuation the master chain applies — same factor either way). We
        // give it a generous floor of 1.5× to stay robust to settle timing.
        assert!(
            duo_master >= solo_master * 1.5,
            "two-track master {duo_master} should be >= 1.5x solo {solo_master}"
        );
        // Both per-track meters should be reading on the duo.
        assert!(duo.track_peak_left(0) > 0.05);
        assert!(duo.track_peak_left(1) > 0.05);
        // The duo's master meter should also be reading clearly.
        assert!(duo.master_peak_left() > 0.1);
    }

    /// Muting one track must drop *only* that track's meter — the other strip
    /// and the master keep reading. Confirms the per-track meter wiring is
    /// independent.
    #[test]
    fn muting_one_track_does_not_silence_the_other() {
        let (mut scope_tx, _r) = scope_channel();
        let mut mixer = Mixer::new(SR);
        mixer.set_clip(0, const_clip(0.4, 96_000));
        mixer.set_clip(1, const_clip(0.4, 96_000));
        mixer.handle(Command::Play);
        mixer.handle(Command::SetTrackGainDb(1, -120.0)); // mute track 1

        let mut data = vec![0.0_f32; 1024];
        for _ in 0..200 {
            mixer.process(&mut data, 2, &mut scope_tx);
        }

        assert!(
            mixer.track_peak_left(0) > 0.05,
            "track 0 should still meter, got {}",
            mixer.track_peak_left(0)
        );
        assert!(
            mixer.track_peak_left(1) < 0.01,
            "track 1 should be muted, got {}",
            mixer.track_peak_left(1)
        );
        assert!(
            mixer.master_peak_left() > 0.05,
            "master keeps reading from the un-muted track"
        );
    }

    /// The master gain attenuates the summed signal independent of any track.
    #[test]
    fn master_gain_attenuates_the_sum() {
        let (mut scope_tx, _r) = scope_channel();
        let mut mixer = Mixer::new(SR);
        mixer.set_clip(0, const_clip(0.5, 96_000));
        mixer.handle(Command::Play);
        mixer.handle(Command::SetMasterGainDb(-120.0)); // master mute

        let mut data = vec![0.0_f32; 1024];
        for _ in 0..200 {
            mixer.process(&mut data, 2, &mut scope_tx);
        }
        assert!(
            mixer.master_peak_left() < 0.01,
            "master mute should silence output, got {}",
            mixer.master_peak_left()
        );
        // But the per-track meter, which is *pre-master*, still reads.
        assert!(
            mixer.track_peak_left(0) > 0.05,
            "track meter is pre-master and should still read"
        );
    }

    #[test]
    fn handles_blocks_larger_than_max_block_in_chunks() {
        // A device buffer bigger than MAX_BLOCK must still render fully
        // (chunked), without panicking or going out of bounds.
        let (mut scope_tx, _r) = scope_channel();
        let mut mixer = Mixer::new(SR);
        mixer.set_clip(0, const_clip(0.5, MAX_BLOCK * 4));
        mixer.handle(Command::Play);

        let frames = MAX_BLOCK + 777; // not a multiple of MAX_BLOCK
        let mut data = vec![0.0_f32; frames * 2];
        mixer.process(&mut data, 2, &mut scope_tx);
        assert!(
            data.iter().any(|&s| s != 0.0),
            "oversized block should render"
        );
    }

    /// Out-of-range track indices in commands and clip loads must be no-ops,
    /// not panics — the audio thread must never panic on UI mistakes.
    #[test]
    fn out_of_range_indices_are_no_ops() {
        let mut mixer = Mixer::new(SR);
        // None of these may panic, and none must touch a real strip.
        mixer.handle(Command::SetTrackGainDb(99, -6.0));
        mixer.handle(Command::SetTrackPan(99, 1.0));
        mixer.handle(Command::SetTrackEqBandFreq(99, 0, 1000.0));
        assert!(mixer.set_clip(99, const_clip(0.1, 100)).is_none());
    }

    /// The M5 dynamic-add/remove contract: calling `clear_track` repeatedly while
    /// the mixer is rendering must not allocate on what would be the audio
    /// thread. The strip's clip Arc is `take`n (handed back for retirement on the
    /// control side); the chain rebuilds in place; the meter zeros. None of that
    /// should touch the heap.
    #[test]
    fn clear_track_does_not_allocate() {
        let (mut scope_tx, _r) = scope_channel();
        let mut mixer = Mixer::new(SR);
        let clip = const_clip(0.4, 50_000);
        let mut data = vec![0.0_f32; 1024];

        // Hand the strip a clip so `clear_track` has work to do. Setting and the
        // first render happen inside the guard so the displaced Arc is observed
        // as a `take`, not a drop.
        let mut clip_slot = Some(clip);

        assert_no_alloc(|| {
            for _ in 0..200 {
                // Reload-then-clear cycle. Each iteration: install a clip, play
                // some audio, clear the slot (returns the displaced Arc which we
                // hold for the next iteration so its memory is never freed inside
                // the guard).
                if let Some(c) = clip_slot.take() {
                    mixer.set_clip(0, c);
                }
                mixer.handle(Command::Play);
                mixer.process(&mut data, 2, &mut scope_tx);
                clip_slot = mixer.clear_track(0);
            }
        });
    }

    #[test]
    fn mixer_process_does_not_allocate() {
        // The full mixer path under assert_no_alloc — command routing, two
        // strips rendering and summing, master chain, master meter, interleave,
        // scope tap — driven by parameter changes and a clip swap.
        let (mut scope_tx, _r) = scope_channel();
        let mut mixer = Mixer::new(SR);
        let clip0 = const_clip(0.3, 50_000);
        let clip1 = const_clip(0.3, 50_000);
        let mut data = vec![0.0_f32; 1024];

        assert_no_alloc(|| {
            mixer.set_clip(0, clip0); // moves the Arc in; no alloc
            mixer.set_clip(1, clip1);
            mixer.handle(Command::Play);
            for i in 0..1000 {
                mixer.handle(Command::SetTrackGainDb(0, -(i % 24) as f32));
                mixer.handle(Command::SetTrackPan(1, ((i % 200) as f32 / 100.0) - 1.0));
                mixer.handle(Command::SetMasterGainDb(-(i % 12) as f32));
                mixer.process(&mut data, 2, &mut scope_tx);
            }
        });
    }
}
