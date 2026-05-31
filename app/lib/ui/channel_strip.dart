import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';

/// Visual floor for the dB faders. The engine treats anything ≤ this as a hard
/// mute, so the bottom of a dB fader reads as −∞.
const double kGainFloorDb = -60;

/// The three gain-fader representations being compared. The signal is always
/// driven by a single linear multiplier; these only change how the fader's
/// travel maps to it, and how high it reaches.
enum GainFaderMode {
  /// Logarithmic, −60…+6 dB. Perceptually even travel; +6 dB ceiling.
  db6('dB', 'dB  −60…+6'),

  /// Raw linear multiplier, 0…2 (= +6.02 dB). Bunched travel — quiet fades
  /// happen in a sliver at the bottom.
  linear('Lin', 'Linear  0…2'),

  /// Logarithmic, −∞…+12 dB. Same even feel as `db6` with double the headroom.
  db12('dB+', 'dB  −∞…+12');

  const GainFaderMode(this.short, this.caption);

  /// Label for the selector segment.
  final String short;

  /// Caption shown under the active fader.
  final String caption;
}

// dB ↔ linear helpers. Mute is a true 0 at/below the floor.
double _linToDb(double lin) =>
    lin <= 0 ? double.negativeInfinity : 20 * math.log(lin) / math.ln10;
double _dbToLin(double db) =>
    db <= kGainFloorDb ? 0 : math.pow(10, db / 20).toDouble();

/// A single channel strip: a gain section (three selectable fader styles for the
/// A/B comparison), a pan control, and a live post-fader peak meter.
///
/// Dumb widget — it owns no engine state. The signal gain is a single
/// [gainLinear] multiplier; [gainMode] picks which fader is active. Changes are
/// reported back through the callbacks; the meter is driven by the [meter]
/// stream. This keeps it drivable by a fake engine in widget tests.
class ChannelStrip extends StatelessWidget {
  const ChannelStrip({
    super.key,
    required this.gainLinear,
    required this.gainMode,
    required this.pan,
    required this.meter,
    this.onGainLinearChanged,
    this.onGainModeChanged,
    this.onPanChanged,
  });

  /// Current gain as a raw linear multiplier (the single source of truth).
  final double gainLinear;

  /// Which fader style is currently active / interactive.
  final GainFaderMode gainMode;

  final double pan;
  final Stream<MeterLevels> meter;
  final ValueChanged<double>? onGainLinearChanged;
  final ValueChanged<GainFaderMode>? onGainModeChanged;
  final ValueChanged<double>? onPanChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
      decoration: BoxDecoration(
        color: const Color(0xFF15151B),
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('Channel', style: theme.textTheme.labelMedium),
          const SizedBox(height: 12),

          // Selector: which fader representation is live.
          SegmentedButton<GainFaderMode>(
            showSelectedIcon: false,
            segments: [
              for (final m in GainFaderMode.values)
                ButtonSegment(value: m, label: Text(m.short)),
            ],
            selected: {gainMode},
            onSelectionChanged: onGainModeChanged == null
                ? null
                : (s) => onGainModeChanged!(s.first),
          ),
          const SizedBox(height: 12),

          // The three faders side by side. Only the active one is interactive;
          // the others mirror the same gain in their own scale, greyed out.
          SizedBox(
            height: 210,
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                for (final m in GainFaderMode.values) ...[
                  _GainFader(
                    mode: m,
                    gainLinear: gainLinear,
                    active: m == gainMode,
                    onChanged: m == gainMode ? onGainLinearChanged : null,
                  ),
                  const SizedBox(width: 4),
                ],
                const SizedBox(width: 4),
                _MeterBars(meter: meter),
              ],
            ),
          ),
          const SizedBox(height: 4),
          Text(gainMode.caption, style: theme.textTheme.bodySmall),

          const SizedBox(height: 16),
          // Pan: a centered slider in [-1, 1].
          SizedBox(
            width: 200,
            child: Slider(
              min: -1,
              max: 1,
              value: pan.clamp(-1.0, 1.0),
              onChanged: onPanChanged,
            ),
          ),
          Text(_fmtPan(pan), style: theme.textTheme.bodySmall),
        ],
      ),
    );
  }
}

/// One vertical gain fader in a given [mode]. Reads its position from the shared
/// [gainLinear] (converted into its own scale) and reports changes back as a new
/// linear value. Disabled (greyed) when not [active].
class _GainFader extends StatelessWidget {
  const _GainFader({
    required this.mode,
    required this.gainLinear,
    required this.active,
    this.onChanged,
  });

  final GainFaderMode mode;
  final double gainLinear;
  final bool active;
  final ValueChanged<double>? onChanged;

  /// (min, max) of this fader's scale.
  (double, double) get _range => switch (mode) {
    GainFaderMode.db6 => (kGainFloorDb, 6),
    GainFaderMode.linear => (0, 2),
    GainFaderMode.db12 => (kGainFloorDb, 12),
  };

  /// The shared gain expressed in this fader's scale, clamped to its range.
  double get _value {
    final (lo, hi) = _range;
    final raw = mode == GainFaderMode.linear
        ? gainLinear
        : _linToDb(gainLinear);
    return raw.clamp(lo, hi);
  }

  /// This fader's own readout label.
  String get _readout => switch (mode) {
    GainFaderMode.linear => '${gainLinear.toStringAsFixed(2)}×',
    _ => _fmtGainDb(_linToDb(gainLinear)),
  };

  void _onChanged(double v) {
    if (onChanged == null) return;
    onChanged!(mode == GainFaderMode.linear ? v : _dbToLin(v));
  }

  @override
  Widget build(BuildContext context) {
    final (lo, hi) = _range;
    final theme = Theme.of(context);
    // Value stays fully legible for every fader (so all three can be read at a
    // glance); the active one is emphasized. Only the slider itself greys out.
    final valueStyle = theme.textTheme.bodySmall?.copyWith(
      fontWeight: active ? FontWeight.bold : FontWeight.normal,
      color: active ? theme.colorScheme.primary : null,
    );
    final labelStyle = theme.textTheme.labelSmall?.copyWith(
      color: active ? null : theme.disabledColor,
    );
    return SizedBox(
      width: 60,
      child: Column(
        children: [
          // Current value, scribble-strip style, above the fader. Scaled down
          // if a wide value (e.g. "-12.0 dB") would overflow the column.
          SizedBox(
            height: 18,
            child: FittedBox(
              fit: BoxFit.scaleDown,
              child: Text(_readout, maxLines: 1, style: valueStyle),
            ),
          ),
          const SizedBox(height: 2),
          Expanded(
            child: RotatedBox(
              quarterTurns: 3,
              child: Slider(
                min: lo,
                max: hi,
                value: _value,
                onChanged: active ? _onChanged : null,
              ),
            ),
          ),
          const SizedBox(height: 2),
          Text(mode.short, style: labelStyle),
        ],
      ),
    );
  }
}

String _fmtGainDb(double db) {
  if (db <= kGainFloorDb || db == double.negativeInfinity) return '-∞';
  return '${db >= 0 ? '+' : ''}${db.toStringAsFixed(1)} dB';
}

String _fmtPan(double pan) {
  if (pan.abs() < 0.005) return 'C';
  final side = pan < 0 ? 'L' : 'R';
  return '$side ${(pan.abs() * 100).round()}';
}

// Meter visual range. The signal can exceed +6 dB (boost), in which case the
// meter simply pegs at the top — useful "you're slamming it" feedback.
const double _kMeterFloorDb = -60;
const double _kMeterTopDb = 6;

/// Two vertical peak-meter bars (L, R) driven by the meter stream, on a dB scale.
class _MeterBars extends StatelessWidget {
  const _MeterBars({required this.meter});

  final Stream<MeterLevels> meter;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: 28,
      height: double.infinity,
      child: ClipRRect(
        borderRadius: BorderRadius.circular(4),
        child: StreamBuilder<MeterLevels>(
          stream: meter,
          builder: (context, snapshot) {
            return CustomPaint(
              painter: _MeterPainter(snapshot.data ?? MeterLevels.silent),
            );
          },
        ),
      ),
    );
  }
}

class _MeterPainter extends CustomPainter {
  _MeterPainter(this.levels);

  final MeterLevels levels;

  /// Linear amplitude → height fraction in [0, 1] on a dB scale.
  double _fraction(double linear) {
    if (linear <= 0) return 0;
    final db = 20 * (math.log(linear) / math.ln10);
    return ((db - _kMeterFloorDb) / (_kMeterTopDb - _kMeterFloorDb)).clamp(
      0.0,
      1.0,
    );
  }

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(
      Offset.zero & size,
      Paint()..color = const Color(0xFF0B0B10),
    );

    const gap = 2.0;
    final barW = (size.width - gap) / 2;
    _bar(canvas, size, 0, barW, _fraction(levels.peakLeft));
    _bar(canvas, size, barW + gap, barW, _fraction(levels.peakRight));
  }

  void _bar(Canvas canvas, Size size, double x, double w, double frac) {
    final h = size.height * frac;
    // Green below ~-6 dB, amber approaching 0, red at the top.
    final color = frac > 0.92
        ? const Color(0xFFE53935)
        : frac > 0.82
        ? const Color(0xFFFFB300)
        : const Color(0xFF43A047);
    canvas.drawRect(
      Rect.fromLTWH(x, size.height - h, w, h),
      Paint()..color = color,
    );
  }

  @override
  bool shouldRepaint(_MeterPainter old) =>
      old.levels.peakLeft != levels.peakLeft ||
      old.levels.peakRight != levels.peakRight;
}
