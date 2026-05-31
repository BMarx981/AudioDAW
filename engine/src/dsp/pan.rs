//! [`Pan`] — a smoothed constant-power stereo balance.
//!
//! Pan position is a smoothed value in `[-1, 1]` (−1 = hard left, 0 = center,
//! +1 = hard right). We use the **−3 dB constant-power** law: map the position
//! to an angle `θ ∈ [0, π/2]` and scale left by `cos θ`, right by `sin θ`. Since
//! `cos²θ + sin²θ = 1`, the summed power is constant as you sweep across the
//! field — no loudness bump at center (where each side sits at `cos(π/4) ≈
//! 0.707`, i.e. −3 dB). This is the law most DAWs use for stereo channel pan.
//!
//! This is a *balance* control (it attenuates each existing side); it does not
//! cross-feed one channel into the other. That's the right behavior for a stereo
//! track strip. It expects a stereo block; on a non-stereo block it's a no-op.

use std::f32::consts::FRAC_PI_2;

use super::{Process, SmoothedParam};

/// Glide time for pan moves — same feel as [`super::Gain`].
const SMOOTH_SECS: f32 = 0.008;

/// A smoothed constant-power stereo pan.
pub struct Pan {
    /// Smoothed pan position in `[-1, 1]`.
    pos: SmoothedParam,
}

impl Pan {
    /// Create a pan unit starting at `initial` (−1..1), sitting exactly there.
    pub fn new(sample_rate: f32, initial: f32) -> Self {
        Self {
            pos: SmoothedParam::new(sample_rate, SMOOTH_SECS, initial.clamp(-1.0, 1.0)),
        }
    }

    /// Aim the pan at `pos`, clamped to `[-1, 1]`. Realtime-safe; the move glides
    /// per-sample in [`Process::process`].
    #[inline]
    pub fn set_pos(&mut self, pos: f32) {
        self.pos.set_target(pos.clamp(-1.0, 1.0));
    }

    /// Constant-power gains `(left, right)` for a pan position in `[-1, 1]`.
    /// `-1 → (1, 0)`, `0 → (0.707, 0.707)`, `+1 → (0, 1)`.
    #[inline]
    fn gains(pos: f32) -> (f32, f32) {
        // Map [-1, 1] -> [0, π/2].
        let theta = (pos.clamp(-1.0, 1.0) + 1.0) * 0.5 * FRAC_PI_2;
        (theta.cos(), theta.sin())
    }
}

impl Process for Pan {
    #[inline]
    fn process(&mut self, block: &mut [&mut [f32]]) {
        // Balance is inherently stereo; if we weren't handed a stereo block,
        // there's nothing meaningful to do.
        if block.len() < 2 {
            return;
        }
        let frames = block[0].len().min(block[1].len());
        // Split the borrow so we can touch both channels in the same frame.
        let (left, rest) = block.split_at_mut(1);
        let left = &mut left[0];
        let right = &mut rest[0];
        for f in 0..frames {
            // Advance the smoother once per frame, then derive the channel gains.
            let (gl, gr) = Self::gains(self.pos.next());
            left[f] *= gl;
            right[f] *= gr;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    const SR: f32 = 48_000.0;

    /// Settle a pan at `pos` and return the steady `(left, right)` output for a
    /// unity signal fed to both channels.
    fn steady_lr_at(pos: f32) -> (f32, f32) {
        let mut p = Pan::new(SR, pos); // constructed settled
        let mut l = vec![1.0_f32; 2048];
        let mut r = vec![1.0_f32; 2048];
        let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
        p.process(&mut block);
        (l[l.len() - 1], r[r.len() - 1])
    }

    #[test]
    fn center_is_minus_three_db_each_side() {
        let (l, r) = steady_lr_at(0.0);
        assert!((l - 0.70710677).abs() < 1e-4, "center L ~0.707, got {l}");
        assert!((r - 0.70710677).abs() < 1e-4, "center R ~0.707, got {r}");
    }

    #[test]
    fn hard_left_silences_the_right() {
        let (l, r) = steady_lr_at(-1.0);
        assert!((l - 1.0).abs() < 1e-4, "hard left: L unity, got {l}");
        assert!(r.abs() < 1e-4, "hard left: R silent, got {r}");
    }

    #[test]
    fn hard_right_silences_the_left() {
        let (l, r) = steady_lr_at(1.0);
        assert!(l.abs() < 1e-4, "hard right: L silent, got {l}");
        assert!((r - 1.0).abs() < 1e-4, "hard right: R unity, got {r}");
    }

    #[test]
    fn total_power_is_constant_across_the_field() {
        // cos²θ + sin²θ = 1, so L² + R² should equal the input power (1.0 here)
        // at every pan position — the defining property of constant-power pan.
        for &pos in &[-1.0, -0.6, -0.2, 0.0, 0.3, 0.7, 1.0] {
            let (l, r) = steady_lr_at(pos);
            let power = l * l + r * r;
            assert!(
                (power - 1.0).abs() < 1e-3,
                "power should be constant (1.0) at pos {pos}, got {power}"
            );
        }
    }

    #[test]
    fn pan_does_not_allocate() {
        let mut p = Pan::new(SR, 0.0);
        let mut l = [0.5_f32; 512];
        let mut r = [0.5_f32; 512];
        assert_no_alloc(|| {
            for i in 0..1000 {
                p.set_pos(((i % 200) as f32 / 100.0) - 1.0);
                let mut block: [&mut [f32]; 2] = [&mut l, &mut r];
                p.process(&mut block);
            }
        });
    }
}
