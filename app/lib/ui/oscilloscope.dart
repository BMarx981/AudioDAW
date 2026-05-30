import 'dart:typed_data';

import 'package:flutter/material.dart';

/// A simple realtime oscilloscope.
///
/// Dumb widget: it just plots whatever sample windows arrive on [frames]. All
/// the signal work (tapping the audio thread, trigger alignment) lives in Rust;
/// each event here is one window of mono samples in roughly [-1, 1].
class Oscilloscope extends StatelessWidget {
  const Oscilloscope({
    super.key,
    required this.frames,
    this.height = 160,
    this.traceColor,
    this.backgroundColor = const Color(0xFF101015),
  });

  final Stream<Float32List> frames;
  final double height;
  final Color? traceColor;
  final Color backgroundColor;

  @override
  Widget build(BuildContext context) {
    final trace = traceColor ?? Theme.of(context).colorScheme.primary;
    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: SizedBox(
        height: height,
        width: double.infinity,
        child: StreamBuilder<Float32List>(
          stream: frames,
          builder: (context, snapshot) {
            return CustomPaint(
              painter: _ScopePainter(
                samples: snapshot.data ?? Float32List(0),
                traceColor: trace,
                backgroundColor: backgroundColor,
              ),
            );
          },
        ),
      ),
    );
  }
}

class _ScopePainter extends CustomPainter {
  _ScopePainter({
    required this.samples,
    required this.traceColor,
    required this.backgroundColor,
  });

  final Float32List samples;
  final Color traceColor;
  final Color backgroundColor;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    canvas.drawRect(rect, Paint()..color = backgroundColor);

    final midY = size.height / 2;

    // Centre (zero) line.
    canvas.drawLine(
      Offset(0, midY),
      Offset(size.width, midY),
      Paint()
        ..color = traceColor.withValues(alpha: 0.18)
        ..strokeWidth = 1,
    );

    if (samples.length < 2) return;

    // A little headroom so a full-scale sine doesn't touch the edges.
    final amp = midY * 0.9;
    final dx = size.width / (samples.length - 1);

    final path = Path()..moveTo(0, midY - samples[0].clamp(-1.0, 1.0) * amp);
    for (var i = 1; i < samples.length; i++) {
      path.lineTo(dx * i, midY - samples[i].clamp(-1.0, 1.0) * amp);
    }

    canvas.drawPath(
      path,
      Paint()
        ..color = traceColor
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2
        ..strokeJoin = StrokeJoin.round
        ..isAntiAlias = true,
    );
  }

  @override
  bool shouldRepaint(_ScopePainter old) =>
      old.samples != samples ||
      old.traceColor != traceColor ||
      old.backgroundColor != backgroundColor;
}
