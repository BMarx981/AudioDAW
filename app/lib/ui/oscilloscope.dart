import 'dart:typed_data';

import 'package:flutter/material.dart';

/// A simple realtime oscilloscope.
///
/// Dumb relative to the engine: it just plots whatever sample windows arrive on
/// [frames] (each event is one window of mono samples in roughly [-1, 1]; all
/// the signal work — tapping the audio thread, trigger alignment — lives in
/// Rust). It owns one bit of *view* state: a vertical zoom so a quiet trace can
/// be magnified to fill the box. Zoom is purely visual and never touches the
/// engine.
class Oscilloscope extends StatefulWidget {
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
  State<Oscilloscope> createState() => _OscilloscopeState();
}

class _OscilloscopeState extends State<Oscilloscope> {
  static const double _minZoom = 1;
  static const double _maxZoom = 16;
  double _zoom = 2; // a little magnified by default — full-scale rarely happens

  void _zoomBy(double factor) {
    setState(() => _zoom = (_zoom * factor).clamp(_minZoom, _maxZoom));
  }

  @override
  Widget build(BuildContext context) {
    final trace = widget.traceColor ?? Theme.of(context).colorScheme.primary;
    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: SizedBox(
        height: widget.height,
        width: double.infinity,
        child: Stack(
          children: [
            Positioned.fill(
              child: StreamBuilder<Float32List>(
                stream: widget.frames,
                builder: (context, snapshot) {
                  return CustomPaint(
                    painter: _ScopePainter(
                      samples: snapshot.data ?? Float32List(0),
                      zoom: _zoom,
                      traceColor: trace,
                      backgroundColor: widget.backgroundColor,
                    ),
                  );
                },
              ),
            ),
            Positioned(
              top: 4,
              right: 4,
              child: _ZoomControls(
                zoom: _zoom,
                onZoomIn: _zoom < _maxZoom ? () => _zoomBy(2) : null,
                onZoomOut: _zoom > _minZoom ? () => _zoomBy(0.5) : null,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// A compact zoom in/out pill overlaid on the scope, showing the current factor.
class _ZoomControls extends StatelessWidget {
  const _ZoomControls({required this.zoom, this.onZoomIn, this.onZoomOut});

  final double zoom;
  final VoidCallback? onZoomIn;
  final VoidCallback? onZoomOut;

  @override
  Widget build(BuildContext context) {
    final fg = Colors.white.withValues(alpha: 0.85);
    final label = zoom % 1 == 0
        ? zoom.toInt().toString()
        : zoom.toStringAsFixed(1);
    return Container(
      decoration: BoxDecoration(
        color: Colors.black.withValues(alpha: 0.35),
        borderRadius: BorderRadius.circular(8),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 2),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          IconButton(
            onPressed: onZoomOut,
            tooltip: 'Zoom out',
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            color: fg,
            icon: const Icon(Icons.zoom_out),
          ),
          Text('$label×', style: TextStyle(color: fg, fontSize: 12)),
          IconButton(
            onPressed: onZoomIn,
            tooltip: 'Zoom in',
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            color: fg,
            icon: const Icon(Icons.zoom_in),
          ),
        ],
      ),
    );
  }
}

class _ScopePainter extends CustomPainter {
  _ScopePainter({
    required this.samples,
    required this.zoom,
    required this.traceColor,
    required this.backgroundColor,
  });

  final Float32List samples;
  final double zoom;
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

    // A little headroom so a full-scale sine doesn't touch the edges. The zoom
    // multiplies the sample before clamping to the view, so an over-zoomed loud
    // trace simply flattens against the top/bottom rather than drawing outside.
    final amp = midY * 0.9;
    final dx = size.width / (samples.length - 1);
    double y(int i) => midY - (samples[i] * zoom).clamp(-1.0, 1.0) * amp;

    final path = Path()..moveTo(0, y(0));
    for (var i = 1; i < samples.length; i++) {
      path.lineTo(dx * i, y(i));
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
      old.zoom != zoom ||
      old.traceColor != traceColor ||
      old.backgroundColor != backgroundColor;
}
