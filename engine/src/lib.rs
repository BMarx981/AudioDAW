//! DAW audio engine — Milestone 1 (WAV playback).
//!
//! Layout:
//! - [`api`]      — the flutter_rust_bridge surface (the only thing Flutter sees)
//! - [`audio`]    — cpal stream + the realtime callback + the `Engine` handle
//! - [`strip`]    — channel strip: source → gain → pan → output, plus a meter
//! - [`dsp`]      — built-in DSP units + the `Process` trait they compose through
//! - [`player`]   — the realtime WAV playback source (planar render)
//! - [`clip`]     — decoded, immutable audio shared across threads by `Arc`
//! - [`decode`]   — symphonia WAV decode (control thread only)
//! - [`commands`] — the lock-free control->audio command ring
//! - [`scope`]    — the lock-free audio->UI sample tap for the oscilloscope
//! - [`osc`]      — the Milestone 0 sine oscillator; kept for the synth
//!   milestone but no longer wired into the audio callback

pub mod api;
mod frb_generated;

mod audio;
mod clip;
mod commands;
mod decode;
mod dsp;
mod mixer;
mod player;
mod project;
mod sampler;
mod scope;
mod strip;

// Kept for a future milestone (built-in synth); not currently wired into the
// audio path, so suppress the dead-code lint on its still-unused parts.
#[allow(dead_code)]
mod osc;

// In test builds only, install an allocator that lets `assert_no_alloc` detect
// heap activity on the audio path. This is gated to `cfg(test)` so production
// builds use the normal system allocator and pay nothing.
#[cfg(test)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
