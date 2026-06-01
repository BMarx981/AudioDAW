//! Control → audio messaging.
//!
//! The UI thread (where Dart calls land) must never touch the oscillator
//! directly, because the audio callback owns it on another thread. Sharing it
//! behind an `Arc<Mutex<_>>` is forbidden here: locking a mutex on the audio
//! thread can block on a deadline-critical thread and cause dropouts.
//!
//! Instead we pass small POD commands across a lock-free single-producer /
//! single-consumer ring buffer (`rtrb`). The producer lives on the control
//! side; the consumer is moved into the audio callback. Push and pop are
//! wait-free — no mutex, no allocation — so popping is safe on the audio thread.

use rtrb::{Consumer, Producer, RingBuffer};

// rtrb 0.3: `RingBuffer::new(capacity)` returns the connected `(Producer,
// Consumer)` pair directly (older versions needed a `.split()` call).

/// A command from the control thread to the audio thread.
///
/// Keep these `Copy` and small: they are memcpy'd into the ring buffer. As the
/// engine grows this becomes the one channel through which *all* parameter
/// changes flow to the audio thread.
///
/// Note the deliberate split: small POD transport commands travel here, but a
/// freshly-decoded clip (potentially megabytes) does **not** — it rides its own
/// `Arc<AudioClip>` hand-off ring (see [`crate::audio`] and [`crate::player`]),
/// so this enum stays tiny and `Copy`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Begin (or resume) playback from the current position.
    Play,
    /// Pause playback, holding the current position.
    Pause,
    /// Stop playback and rewind to the start.
    Stop,
    /// Jump the playhead to this position, in seconds from the clip start.
    /// Clamped to the clip bounds by the player.
    Seek(f32),
    /// Turn looping on/off. When on, the player wraps back to the start at the
    /// clip end instead of stopping.
    SetLooping(bool),
    /// Set the channel-strip gain target, in decibels. Clamped and smoothed by
    /// the [`crate::dsp::Gain`] unit.
    SetGainDb(f32),
    /// Set the channel-strip gain target as a raw linear multiplier. Clamped and
    /// smoothed by the [`crate::dsp::Gain`] unit. (Used by the linear gain fader.)
    SetGainLinear(f32),
    /// Set the channel-strip pan target, in `[-1, 1]` (−1 = left, +1 = right).
    /// Clamped and smoothed by the [`crate::dsp::Pan`] unit.
    SetPan(f32),
    /// Set EQ band `n`'s filter kind, by integer code (see
    /// [`crate::dsp::FilterKind::from_code`]). Carried as a `u8` rather than the
    /// `FilterKind` enum so this command type — reachable from the bridge crate
    /// via the engine's command ring — names no `dsp` type. That keeps
    /// flutter_rust_bridge from following the reference into the DSP module and
    /// tripping over its array-bearing structs (`Biquad`, `Eq`).
    SetEqBandKind(u8, u8),
    /// Set EQ band `n`'s center/corner frequency, Hz. Smoothed by the EQ.
    SetEqBandFreq(u8, f32),
    /// Set EQ band `n`'s Q (bandwidth). Smoothed by the EQ.
    SetEqBandQ(u8, f32),
    /// Set EQ band `n`'s gain, dB (peak/shelf kinds). Smoothed by the EQ.
    SetEqBandGainDb(u8, f32),
    /// Enable/disable EQ band `n` (true bypass when off).
    SetEqBandEnabled(u8, bool),
}

/// Number of in-flight commands the ring can hold. Far more than the UI can
/// realistically generate between two audio callbacks; sized generously because
/// it's cheap and means a burst of slider events never has to drop the newest
/// value in practice.
const CAPACITY: usize = 256;

/// Create a connected producer/consumer pair. The producer stays on the control
/// thread; the consumer is moved into the audio callback.
pub fn command_channel() -> (Producer<Command>, Consumer<Command>) {
    RingBuffer::new(CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_across_threads_in_order() {
        // Push from one thread, pop from another: every message arrives, in
        // order, with none missed. This is the contract the whole control->audio
        // path depends on.
        let (mut tx, mut rx) = command_channel();
        const N: usize = 200; // < CAPACITY, so nothing is dropped

        // A distinct command per index so we can assert exact ordering on the far
        // side; the cycle covers every variant including the payload-carrying one.
        let make = |i: usize| match i % 4 {
            0 => Command::Play,
            1 => Command::Pause,
            2 => Command::Stop,
            _ => Command::Seek(i as f32),
        };

        let producer = std::thread::spawn(move || {
            for i in 0..N {
                while tx.push(make(i)).is_err() {
                    std::thread::yield_now(); // ring momentarily full; spin briefly
                }
            }
        });

        let mut received = Vec::with_capacity(N);
        while received.len() < N {
            if let Ok(cmd) = rx.pop() {
                received.push(cmd);
            } else {
                std::thread::yield_now(); // nothing yet; let the producer run
            }
        }
        producer.join().unwrap();

        let expected: Vec<Command> = (0..N).map(make).collect();
        assert_eq!(received, expected, "messages lost or reordered");
    }

    #[test]
    fn pop_on_empty_is_an_error_not_a_block() {
        // The audio callback relies on `pop` returning immediately (Err) when
        // the ring is empty, never blocking.
        let (_tx, mut rx) = command_channel();
        assert!(rx.pop().is_err());
    }
}
