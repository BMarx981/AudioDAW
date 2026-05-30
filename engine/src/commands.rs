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
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Glide the oscillator to this frequency, in Hz.
    SetFrequency(f32),
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

        let producer = std::thread::spawn(move || {
            for i in 0..N {
                // Distinct values so we can assert exact ordering on the far side.
                while tx.push(Command::SetFrequency(i as f32)).is_err() {
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

        let expected: Vec<Command> = (0..N).map(|i| Command::SetFrequency(i as f32)).collect();
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
