//! Oscilloscope tap: copying audio samples off the audio thread so the UI can
//! draw the waveform.
//!
//! ## Why it's shaped this way (Rust notes)
//!
//! This is the mirror image of [`crate::commands`]. There the control thread is
//! the producer and the audio thread the consumer; here the **audio thread is
//! the producer** and a normal UI-side thread is the consumer. Same primitive:
//! a lock-free single-producer/single-consumer `rtrb` ring, whose storage is
//! allocated once up front.
//!
//! - The audio callback owns the [`rtrb::Producer<f32>`] half. Pushing a sample
//!   is wait-free and never allocates, so it's safe on the realtime thread (see
//!   the realtime-safety rules in CLAUDE.md). If the ring is full because the UI
//!   fell behind, the sample is simply dropped — the correct tradeoff on the
//!   audio thread, where blocking or growing a buffer would cause a dropout.
//! - The UI side owns the [`ScopeReader`] and calls [`ScopeReader::frame`] ~60×
//!   a second to get one window of samples to draw. That side may allocate; it's
//!   not realtime.
//!
//! ## Trigger alignment
//!
//! A raw "last N samples" scope drifts horizontally every frame and looks like
//! soup. Real scopes *trigger*: they start each sweep at a consistent point in
//! the waveform. We do the simplest version — start the window at the most
//! recent rising zero-crossing — which locks the trace's phase so a steady tone
//! draws as a steady wave. This runs on the UI-side reader, not the audio
//! thread, so the slightly-involved search is fine here.

use rtrb::{Consumer, Producer, RingBuffer};

/// Samples per drawn frame. ~21 ms at 48 kHz (≈9 cycles of 440 Hz) — enough to
/// see the shape without cramming. The painter just scales this to its width.
pub const SCOPE_WINDOW: usize = 1024;

/// Capacity of the audio→UI ring, in samples. The UI drains every ~16 ms (≈768
/// samples at 48 kHz), so this holds several frames of slack before the audio
/// thread would ever have to drop. Power of two as rtrb prefers.
const SCOPE_CAPACITY: usize = 8192;

/// How much recent audio the reader keeps around to search for a trigger point.
/// Needs to be a bit more than one window so there's room to slide the trigger.
const HISTORY: usize = SCOPE_WINDOW * 3;

/// UI-side consumer of the scope tap. Holds the ring consumer plus a small
/// rolling history buffer used for trigger alignment.
pub struct ScopeReader {
    consumer: Consumer<f32>,
    /// Recent samples, oldest first. Reused across calls so steady-state draws
    /// don't reallocate. (Allocation here is harmless — UI thread — but reusing
    /// the buffer is just tidy.)
    history: Vec<f32>,
}

impl ScopeReader {
    /// Drain everything waiting in the ring and return one window of samples to
    /// draw, aligned to a rising zero-crossing when one is available.
    ///
    /// Returns fewer than [`SCOPE_WINDOW`] samples only during the brief warm-up
    /// before enough audio has arrived; the painter handles a short slice fine.
    ///
    /// Not realtime-safe and doesn't need to be — call from the UI/pump thread.
    pub fn frame(&mut self) -> Vec<f32> {
        // 1. Pull all newly-produced samples. `pop` is wait-free; it returns Err
        //    when the ring is empty, which ends the loop.
        while let Ok(s) = self.consumer.pop() {
            self.history.push(s);
        }

        // 2. Bound the history so it can't grow without limit if the UI stalls.
        //    `drain` shifts in place — no new allocation.
        if self.history.len() > HISTORY {
            let excess = self.history.len() - HISTORY;
            self.history.drain(0..excess);
        }

        let h = &self.history;
        if h.len() < SCOPE_WINDOW {
            // Warm-up: not a full window yet. Show what we have.
            return h.clone();
        }

        // 3. Find a trigger: scan backwards from the newest position that still
        //    leaves a full window after it, and take the most recent rising
        //    zero-crossing (sample <= 0 followed by > 0). Searching backwards
        //    keeps the trace as fresh as possible while phase-locked.
        let latest_start = h.len() - SCOPE_WINDOW;
        let mut start = latest_start; // fallback: just the newest window
        let mut i = latest_start;
        while i > 0 {
            i -= 1;
            if h[i] <= 0.0 && h[i + 1] > 0.0 {
                start = i;
                break;
            }
        }

        h[start..start + SCOPE_WINDOW].to_vec()
    }
}

/// Create a connected `(producer, reader)` pair backed by one pre-allocated ring.
///
/// Call once at engine start. Move the [`Producer`] into the audio callback; keep
/// the [`ScopeReader`] on the control/UI side.
pub fn scope_channel() -> (Producer<f32>, ScopeReader) {
    let (producer, consumer) = RingBuffer::<f32>::new(SCOPE_CAPACITY);
    (
        producer,
        ScopeReader {
            consumer,
            history: Vec::with_capacity(HISTORY),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn pushing_to_the_tap_does_not_allocate() {
        // The audio-thread side of the scope is the producer's `push`. Per
        // TESTING.md every audio-path test runs under assert_no_alloc.
        let (mut tx, _reader) = scope_channel();
        assert_no_alloc(|| {
            // One audio block's worth of pushes — the real callback's pattern.
            for i in 0..512 {
                let _ = tx.push((i as f32 / 512.0) * 2.0 - 1.0);
            }
        });
    }

    #[test]
    fn pushing_past_capacity_drops_instead_of_panicking() {
        let (mut tx, _reader) = scope_channel();
        // Way more than capacity must not panic or block; rtrb returns Err(Full).
        for i in 0..(SCOPE_CAPACITY * 2) {
            let _ = tx.push(i as f32);
        }
    }

    #[test]
    fn frame_is_aligned_to_a_rising_zero_crossing() {
        let (mut tx, mut reader) = scope_channel();
        // Push a few cycles of a clean sine. Use enough that history > WINDOW.
        let n = SCOPE_WINDOW * 2;
        let cycles = 8.0_f32;
        for i in 0..n {
            let phase = (i as f32 / n as f32) * cycles * std::f32::consts::TAU;
            let _ = tx.push(phase.sin());
        }

        let frame = reader.frame();
        assert_eq!(frame.len(), SCOPE_WINDOW);
        // A rising zero-crossing trigger means the window starts near zero and
        // immediately heads positive.
        assert!(
            frame[0].abs() < 0.05,
            "frame should start near a zero crossing, got {}",
            frame[0]
        );
        assert!(frame[1] > frame[0], "frame should start on the rising edge");
    }

    #[test]
    fn warmup_returns_what_is_available() {
        let (mut tx, mut reader) = scope_channel();
        for i in 0..100 {
            let _ = tx.push(i as f32);
        }
        let frame = reader.frame();
        assert_eq!(
            frame.len(),
            100,
            "before a full window, return what we have"
        );
    }
}
