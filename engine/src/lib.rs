//! DAW audio engine — Milestone 0 (sine + slider starter).
//!
//! Layout:
//! - [`api`]      — the flutter_rust_bridge surface (the only thing Flutter sees)
//! - [`audio`]    — cpal stream + the realtime callback
//! - [`osc`]      — the sine oscillator
//! - [`commands`] — the lock-free control->audio ring buffer

pub mod api;
mod frb_generated;

mod audio;
mod commands;
mod osc;

// In test builds only, install an allocator that lets `assert_no_alloc` detect
// heap activity on the audio path. This is gated to `cfg(test)` so production
// builds use the normal system allocator and pay nothing.
#[cfg(test)]
#[global_allocator]
static ALLOC_GUARD: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
