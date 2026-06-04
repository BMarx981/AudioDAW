//! Control → audio messaging.
//!
//! The UI thread (where Dart calls land) must never touch the audio graph
//! directly, because the callback owns it on another thread. Sharing it behind
//! an `Arc<Mutex<_>>` is forbidden here: locking a mutex on the audio thread can
//! block on a deadline-critical thread and cause dropouts.
//!
//! Instead we pass small POD commands across a lock-free single-producer /
//! single-consumer ring buffer (`rtrb`). The producer lives on the control
//! side; the consumer is moved into the audio callback. Push and pop are
//! wait-free — no mutex, no allocation — so popping is safe on the audio thread.
//!
//! ## Multitrack (Milestone 4)
//!
//! Every parameter command now carries a `u8` track index so the mixer can
//! route it to the right strip. The transport commands (`Play`, `Pause`, etc.)
//! stay global — a single playhead drives every track, so they apply to all of
//! them at once. The master bus has its own small set of commands.

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
    // Transport — applied to every track in parallel so the mixer has one
    // unified playhead.
    /// Begin (or resume) playback from the current position on every track.
    Play,
    /// Pause playback on every track, holding the current position.
    Pause,
    /// Stop playback on every track and rewind to the start.
    Stop,
    /// Jump every track's playhead to this position, in seconds from its clip
    /// start. Clamped to the clip bounds by each player.
    Seek(f32),
    /// Turn looping on/off on every track. When on, each player wraps back to
    /// its start at its clip end instead of stopping.
    SetLooping(bool),

    // Per-track parameters. The leading `u8` is the track index into the
    // mixer's strip pool; an out-of-range index is silently ignored by the
    // mixer (the audio thread never panics).
    /// Drop the clip on track `t` and reset its strip to defaults (the slot
    /// stays in the pool for reuse). The audio callback intercepts this one
    /// specially so the displaced clip can be shipped to the retirement ring
    /// — the strip itself never frees memory. See `audio.rs::audio_callback`.
    /// Note: through Milestone 5 each strip only ever holds one clip in slot 0,
    /// so this clears that slot; once the timeline UI places clips in arbitrary
    /// slots, removing a single clip uses [`Command::RemoveClip`] instead.
    ClearTrack(u8),
    /// Drop one specific clip slot on a track. Same retirement contract as
    /// [`Command::ClearTrack`] — the audio callback ships the displaced source
    /// `Arc` to the retirement ring. Out-of-range track or slot is a no-op.
    RemoveClip(u8, u8),
    /// Move a placed clip on the timeline. `(track, slot, start_frame)`.
    MoveClip(u8, u8, i64),
    /// Change a placed clip's length on the timeline. `(track, slot, length_frames)`.
    ResizeClip(u8, u8, u32),
    /// Shift where in the source a placed clip begins reading.
    /// `(track, slot, source_offset_frames)`.
    SetClipSourceOffset(u8, u8, u32),
    /// Set track `t`'s gain target, in decibels.
    SetTrackGainDb(u8, f32),
    /// Set track `t`'s gain target as a raw linear multiplier.
    SetTrackGainLinear(u8, f32),
    /// Set track `t`'s pan target, in `[-1, 1]`.
    SetTrackPan(u8, f32),
    /// Set track `t`'s EQ band `b` filter kind, by integer code.
    SetTrackEqBandKind(u8, u8, u8),
    /// Set track `t`'s EQ band `b` center/corner frequency, Hz.
    SetTrackEqBandFreq(u8, u8, f32),
    /// Set track `t`'s EQ band `b` Q.
    SetTrackEqBandQ(u8, u8, f32),
    /// Set track `t`'s EQ band `b` gain, dB.
    SetTrackEqBandGainDb(u8, u8, f32),
    /// Enable/disable track `t`'s EQ band `b`.
    SetTrackEqBandEnabled(u8, u8, bool),

    // Master bus parameters.
    /// Set the master bus gain target, in decibels.
    SetMasterGainDb(f32),
    /// Set the master bus gain target as a raw linear multiplier.
    SetMasterGainLinear(f32),
    /// Set the master bus pan target, in `[-1, 1]`.
    SetMasterPan(f32),
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
        // side; the cycle covers transport, per-track, and master variants.
        let make = |i: usize| match i % 5 {
            0 => Command::Play,
            1 => Command::Pause,
            2 => Command::Stop,
            3 => Command::SetTrackGainDb((i % 8) as u8, i as f32 * -0.5),
            _ => Command::SetMasterPan(((i % 200) as f32 / 100.0) - 1.0),
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
