//! Built-in DSP units and the trait that lets them compose.
//!
//! This module is the seed of the eventual audio graph. Every processing unit
//! (here: [`Gain`] and [`Pan`]; later: filters, dynamics, …) implements one
//! small trait, [`Process`], so the engine can chain them without knowing what
//! each one does.
//!
//! ## Buffer ownership (read this — it's the core Rust idea here)
//!
//! A [`Process`] unit **borrows** the audio it works on; it never **owns** it.
//! The signature is `process(&mut self, block: &mut [&mut [f32]])`:
//!
//! - The outer `&mut [_]` is the list of channels (planar / deinterleaved: one
//!   slice per channel, e.g. `[L, R]` for stereo — see CLAUDE.md's "f32,
//!   deinterleaved internally" rule).
//! - Each inner `&mut [f32]` is one channel's samples for *this block*.
//! - Processing is **in place**: the unit reads and overwrites the same slices.
//!
//! Why borrow rather than own? The buffers are pre-allocated once (at stream
//! start) and reused every block. If a unit owned a `Vec`, it would have to
//! allocate — forbidden on the audio thread. Borrowing means the caller keeps
//! ownership of the scratch buffers and just lends them out each block; nothing
//! is allocated or freed inside `process`. The borrow checker also guarantees no
//! two units hold the same buffer at once, which is exactly the aliasing rule a
//! serial effects chain wants.
//!
//! ## Why smoothing is per-sample, not per-block
//!
//! A parameter (gain, pan position) that jumps to a new value at a block
//! boundary produces a step discontinuity in the signal — an audible click, and
//! on a stream of small steps, "zipper noise." So a [`Gain`]/[`Pan`] doesn't
//! apply the raw target; it holds a [`SmoothedParam`] that glides toward the
//! target *one sample at a time*. Each frame the unit advances the smoother once
//! and applies that single value across all channels (so the channels stay
//! phase-coherent). That per-sample glide is what turns a knob drag into a smooth
//! fade instead of a staircase. See [`smooth`].

mod biquad;
mod chain;
mod eq;
mod gain;
mod pan;
mod smooth;

pub use biquad::FilterKind;
pub use chain::Chain;
pub use eq::Eq;
pub use gain::Gain;
pub use pan::Pan;
pub use smooth::SmoothedParam;

/// A block-based audio processing unit.
///
/// Implementors transform `block` in place. `block[c]` is channel `c`'s samples
/// for this block; every channel slice has the same length (the block's frame
/// count). Implementations must be **realtime-safe**: allocation-free,
/// lock-free, panic-free — they run inside the audio callback.
pub trait Process {
    /// Process one block of planar audio in place.
    fn process(&mut self, block: &mut [&mut [f32]]);
}
