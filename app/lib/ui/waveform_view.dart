import 'dart:typed_data';

import 'package:flutter/material.dart';

/// Draws a clip's waveform from a precomputed min/max summary and overlays a
/// playhead. Tapping or dragging horizontally reports the target position back
/// through [onSeek] as a fraction in [0, 1].
///
/// Dumb widget: it owns no transport state. The summary arrays come from the
/// engine (computed once at load, in Rust); [positionFraction] is driven by the
/// playback-status stream. All this widget does is paint and translate gestures.
class WaveformView extends StatelessWidget {
  const WaveformView({
    super.key,
    required this.min,
    required this.max,
    this.positionFraction = 0.0,
    this.onSeek,
    this.height = 160,
    this.waveColor,
    this.playheadColor,
    this.backgroundColor = const Color(0xFF101015),
  });

  /// Per-column extremes of the mono signal in [-1, 1]. Same length.
  final Float32List min;
  final Float32List max;

  /// Playhead position as a fraction of the clip, in [0, 1].
  final double positionFraction;

  /// Called with a fraction in [0, 1] when the user taps or drags to scrub.
  final ValueChanged<double>? onSeek;

  final double height;
  final Color? waveColor;
  final Color? playheadColor;
  final Color backgroundColor;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final wave = waveColor ?? scheme.primary;
    final playhead = playheadColor ?? scheme.secondary;

    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: SizedBox(
        height: height,
        width: double.infinity,
        child: LayoutBuilder(
          builder: (context, constraints) {
            void emitSeek(double dx) {
              if (onSeek == null || constraints.maxWidth <= 0) return;
              onSeek!((dx / constraints.maxWidth).clamp(0.0, 1.0));
            }

            return GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTapDown: (d) => emitSeek(d.localPosition.dx),
              onHorizontalDragStart: (d) => emitSeek(d.localPosition.dx),
              onHorizontalDragUpdate: (d) => emitSeek(d.localPosition.dx),
              child: CustomPaint(
                painter: _WaveformPainter(
                  min: min,
                  max: max,
                  positionFraction: positionFraction.clamp(0.0, 1.0),
                  waveColor: wave,
                  playheadColor: playhead,
                  backgroundColor: backgroundColor,
                ),
              ),
            );
          },
        ),
      ),
    );
  }
}

class _WaveformPainter extends CustomPainter {
  _WaveformPainter({
    required this.min,
    required this.max,
    required this.positionFraction,
    required this.waveColor,
    required this.playheadColor,
    required this.backgroundColor,
  });

  final Float32List min;
  final Float32List max;
  final double positionFraction;
  final Color waveColor;
  final Color playheadColor;
  final Color backgroundColor;

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(Offset.zero & size, Paint()..color = backgroundColor);

    final midY = size.height / 2;

    // Zero (centre) line.
    canvas.drawLine(
      Offset(0, midY),
      Offset(size.width, midY),
      Paint()..color = waveColor.withValues(alpha: 0.18),
    );

    final n = min.length;
    if (n > 0 && max.length == n && size.width >= 1) {
      final amp = midY * 0.95;
      final wavePaint = Paint()
        ..color = waveColor
        ..strokeWidth = 1
        ..isAntiAlias = false;

      // One vertical bar per pixel column, mapping the column to a summary
      // bucket. The summary already downsampled the audio, so this is cheap.
      final cols = size.width.ceil();
      for (var x = 0; x < cols; x++) {
        final bucket = ((x / size.width) * n).floor().clamp(0, n - 1);
        final hi = max[bucket].clamp(-1.0, 1.0);
        final lo = min[bucket].clamp(-1.0, 1.0);
        // A flat/silent column still gets a 1px tick so the trace stays visible.
        final yTop = midY - hi * amp;
        final yBot = midY - lo * amp;
        canvas.drawLine(
          Offset(x.toDouble(), yTop),
          Offset(x.toDouble(), yBot == yTop ? yBot + 1 : yBot),
          wavePaint,
        );
      }
    }

    // Playhead.
    final px = positionFraction * size.width;
    canvas.drawLine(
      Offset(px, 0),
      Offset(px, size.height),
      Paint()
        ..color = playheadColor
        ..strokeWidth = 2,
    );
  }

  @override
  bool shouldRepaint(_WaveformPainter old) =>
      old.min != min ||
      old.max != max ||
      old.positionFraction != positionFraction ||
      old.waveColor != waveColor ||
      old.playheadColor != playheadColor ||
      old.backgroundColor != backgroundColor;
}
