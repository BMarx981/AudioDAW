import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';

// `EqBand` and `kDefaultEqBands` moved to engine_interface.dart so the Project
// model can reference them without an import cycle. They're re-exported here
// via the explicit import above for callers that used to expect them from this
// file — Dart re-exports don't exist, so anyone needing the type now imports
// engine_interface.dart directly.

// EQ control ranges.
const double kEqMinFreq = 20;
const double kEqMaxFreq = 20000;
const double kEqMinGainDb = -18;
const double kEqMaxGainDb = 18;
const double kEqMinQ = 0.1;
const double kEqMaxQ = 18;

typedef EqBandDoubleCb = void Function(int band, double value);
typedef EqBandKindCb = void Function(int band, EqFilterKind kind);
typedef EqBandBoolCb = void Function(int band, bool value);

/// A 4-band parametric EQ panel: a live frequency-response curve over the band
/// controls. Dumb widget — it renders [bands] and reports edits through the
/// callbacks; the parent owns the state and forwards to the engine.
class EqView extends StatelessWidget {
  const EqView({
    super.key,
    required this.bands,
    required this.sampleRate,
    this.onFreqChanged,
    this.onGainChanged,
    this.onQChanged,
    this.onKindChanged,
    this.onEnabledChanged,
  });

  final List<EqBand> bands;
  final double sampleRate;
  final EqBandDoubleCb? onFreqChanged;
  final EqBandDoubleCb? onGainChanged;
  final EqBandDoubleCb? onQChanged;
  final EqBandKindCb? onKindChanged;
  final EqBandBoolCb? onEnabledChanged;

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
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('Equalizer', style: theme.textTheme.labelMedium),
          const SizedBox(height: 8),
          // The response curve.
          SizedBox(
            height: 160,
            child: CustomPaint(
              painter: EqResponsePainter(
                bands: bands,
                sampleRate: sampleRate,
                accent: theme.colorScheme.primary,
              ),
              size: Size.infinite,
            ),
          ),
          const SizedBox(height: 12),
          // The four band control columns.
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              for (var i = 0; i < bands.length; i++)
                Expanded(
                  child: _BandControls(index: i, band: bands[i], view: this),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Controls for a single band: enable + kind, then frequency / gain / Q sliders.
class _BandControls extends StatelessWidget {
  const _BandControls({
    required this.index,
    required this.band,
    required this.view,
  });

  final int index;
  final EqBand band;
  final EqView view;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final enabled = band.enabled;
    final canGain = band.kind.usesGain;
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 4),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              SizedBox(
                width: 28,
                height: 28,
                child: Checkbox(
                  value: enabled,
                  onChanged: view.onEnabledChanged == null
                      ? null
                      : (v) => view.onEnabledChanged!(index, v ?? false),
                ),
              ),
              Text('${index + 1}', style: theme.textTheme.labelSmall),
            ],
          ),
          DropdownButton<EqFilterKind>(
            isDense: true,
            isExpanded: true,
            value: band.kind,
            style: theme.textTheme.bodySmall,
            onChanged: !enabled || view.onKindChanged == null
                ? null
                : (k) => view.onKindChanged!(index, k!),
            items: [
              for (final k in EqFilterKind.values)
                DropdownMenuItem(value: k, child: Text(k.label)),
            ],
          ),
          _MiniSlider(
            label: 'Freq',
            readout: _fmtHz(band.freqHz),
            // Frequency rides a log scale — that's how we hear pitch.
            value: _log10(band.freqHz.clamp(kEqMinFreq, kEqMaxFreq)),
            min: _log10(kEqMinFreq),
            max: _log10(kEqMaxFreq),
            enabled: enabled,
            onChanged: view.onFreqChanged == null
                ? null
                : (v) => view.onFreqChanged!(index, math.pow(10, v).toDouble()),
          ),
          _MiniSlider(
            label: 'Gain',
            readout: canGain ? _fmtDb(band.gainDb) : '—',
            value: band.gainDb.clamp(kEqMinGainDb, kEqMaxGainDb),
            min: kEqMinGainDb,
            max: kEqMaxGainDb,
            enabled: enabled && canGain,
            onChanged: view.onGainChanged == null
                ? null
                : (v) => view.onGainChanged!(index, v),
          ),
          _MiniSlider(
            label: 'Q',
            readout: band.q.toStringAsFixed(2),
            value: band.q.clamp(kEqMinQ, kEqMaxQ),
            min: kEqMinQ,
            max: kEqMaxQ,
            enabled: enabled,
            onChanged: view.onQChanged == null
                ? null
                : (v) => view.onQChanged!(index, v),
          ),
        ],
      ),
    );
  }
}

class _MiniSlider extends StatelessWidget {
  const _MiniSlider({
    required this.label,
    required this.readout,
    required this.value,
    required this.min,
    required this.max,
    required this.enabled,
    this.onChanged,
  });

  final String label;
  final String readout;
  final double value;
  final double min;
  final double max;
  final bool enabled;
  final ValueChanged<double>? onChanged;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      children: [
        SizedBox(
          height: 14,
          child: FittedBox(
            fit: BoxFit.scaleDown,
            child: Text(
              '$label  $readout',
              maxLines: 1,
              style: theme.textTheme.labelSmall,
            ),
          ),
        ),
        SliderTheme(
          data: SliderTheme.of(context).copyWith(
            trackHeight: 2,
            thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 7),
          ),
          child: Slider(
            min: min,
            max: max,
            value: value.clamp(min, max),
            onChanged: enabled ? onChanged : null,
          ),
        ),
      ],
    );
  }
}

/// Paints the combined frequency response of all enabled bands as a curve on a
/// log-frequency / dB grid. The curve is computed from the very same RBJ math the
/// engine uses ([eqResponseDb]), so what you see is what you hear.
class EqResponsePainter extends CustomPainter {
  EqResponsePainter({
    required this.bands,
    required this.sampleRate,
    required this.accent,
  });

  final List<EqBand> bands;
  final double sampleRate;
  final Color accent;

  static const double _minDb = kEqMinGainDb;
  static const double _maxDb = kEqMaxGainDb;

  double _xForFreq(double f, Size size) {
    final t =
        (_log10(f) - _log10(kEqMinFreq)) /
        (_log10(kEqMaxFreq) - _log10(kEqMinFreq));
    return t * size.width;
  }

  double _yForDb(double db, Size size) {
    final t = (db - _minDb) / (_maxDb - _minDb);
    return size.height * (1 - t.clamp(0.0, 1.0));
  }

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawRect(
      Offset.zero & size,
      Paint()..color = const Color(0xFF0B0B10),
    );

    final grid = Paint()
      ..color = const Color(0xFF2A2A35)
      ..strokeWidth = 1;
    // Vertical grid at decade frequencies.
    for (final f in const [100.0, 1000.0, 10000.0]) {
      final x = _xForFreq(f, size);
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), grid);
    }
    // Horizontal grid every 6 dB, with the 0 dB line emphasized.
    for (double db = _minDb; db <= _maxDb; db += 6) {
      final y = _yForDb(db, size);
      canvas.drawLine(
        Offset(0, y),
        Offset(size.width, y),
        db == 0
            ? (Paint()
                ..color = const Color(0xFF454555)
                ..strokeWidth = 1)
            : grid,
      );
    }

    // The response curve.
    final path = Path();
    const samples = 256;
    for (var i = 0; i <= samples; i++) {
      final t = i / samples;
      // Sweep frequency logarithmically across the width.
      final f = math
          .pow(
            10,
            _log10(kEqMinFreq) + t * (_log10(kEqMaxFreq) - _log10(kEqMinFreq)),
          )
          .toDouble();
      final db = eqResponseDb(bands, sampleRate, f);
      final pt = Offset(t * size.width, _yForDb(db, size));
      if (i == 0) {
        path.moveTo(pt.dx, pt.dy);
      } else {
        path.lineTo(pt.dx, pt.dy);
      }
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = accent
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2,
    );

    // A marker dot at each enabled band's frequency/gain.
    for (final b in bands) {
      if (!b.enabled) continue;
      final db = b.kind.usesGain ? b.gainDb : 0.0;
      final c = Offset(
        _xForFreq(b.freqHz.clamp(kEqMinFreq, kEqMaxFreq), size),
        _yForDb(db, size),
      );
      canvas.drawCircle(c, 3.5, Paint()..color = accent);
    }
  }

  @override
  bool shouldRepaint(EqResponsePainter old) =>
      old.sampleRate != sampleRate ||
      old.accent != accent ||
      !_sameBands(old.bands, bands);

  static bool _sameBands(List<EqBand> a, List<EqBand> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      final x = a[i], y = b[i];
      if (x.kind != y.kind ||
          x.freqHz != y.freqHz ||
          x.q != y.q ||
          x.gainDb != y.gainDb ||
          x.enabled != y.enabled) {
        return false;
      }
    }
    return true;
  }
}

// ---------------------------------------------------------------------------
// Frequency-response math — a faithful port of engine `dsp::biquad`, so the UI
// curve is identical to the filter the audio thread runs. Tested against the
// same analytic expectations as the Rust side.
// ---------------------------------------------------------------------------

/// Combined magnitude response (dB) of all enabled [bands] at [freqHz], for a
/// stream at [sampleRate]. Sum of the per-band biquad magnitudes.
double eqResponseDb(List<EqBand> bands, double sampleRate, double freqHz) {
  var sum = 0.0;
  for (final b in bands) {
    if (!b.enabled) continue;
    final c = _coeffs(b.kind, sampleRate, b.freqHz, b.q, b.gainDb);
    sum += _magnitudeDb(c, sampleRate, freqHz);
  }
  return sum;
}

class _Coeffs {
  const _Coeffs(this.b0, this.b1, this.b2, this.a1, this.a2);
  final double b0, b1, b2, a1, a2;
}

_Coeffs _coeffs(
  EqFilterKind kind,
  double sr,
  double f0,
  double q,
  double gainDb,
) {
  if (sr <= 0) return const _Coeffs(1, 0, 0, 0, 0);
  final nyq = sr * 0.5;
  f0 = f0.clamp(1.0, nyq * 0.99);
  q = q.clamp(0.05, 40.0);
  gainDb = gainDb.clamp(-24.0, 24.0);

  final a = math.pow(10, gainDb / 40).toDouble();
  final w0 = 2 * math.pi * f0 / sr;
  final cosW0 = math.cos(w0);
  final sinW0 = math.sin(w0);
  final alpha = sinW0 / (2 * q);

  double b0, b1, b2, a0, a1, a2;
  switch (kind) {
    case EqFilterKind.lowpass:
      final t = 1 - cosW0;
      b0 = t * 0.5;
      b1 = t;
      b2 = t * 0.5;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
    case EqFilterKind.highpass:
      final t = 1 + cosW0;
      b0 = t * 0.5;
      b1 = -t;
      b2 = t * 0.5;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
    case EqFilterKind.bandpass:
      b0 = alpha;
      b1 = 0;
      b2 = -alpha;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
    case EqFilterKind.notch:
      b0 = 1;
      b1 = -2 * cosW0;
      b2 = 1;
      a0 = 1 + alpha;
      a1 = -2 * cosW0;
      a2 = 1 - alpha;
    case EqFilterKind.peak:
      b0 = 1 + alpha * a;
      b1 = -2 * cosW0;
      b2 = 1 - alpha * a;
      a0 = 1 + alpha / a;
      a1 = -2 * cosW0;
      a2 = 1 - alpha / a;
    case EqFilterKind.lowShelf:
      final twoSqrtAAlpha = 2 * math.sqrt(a) * alpha;
      final ap1 = a + 1, am1 = a - 1;
      b0 = a * (ap1 - am1 * cosW0 + twoSqrtAAlpha);
      b1 = 2 * a * (am1 - ap1 * cosW0);
      b2 = a * (ap1 - am1 * cosW0 - twoSqrtAAlpha);
      a0 = ap1 + am1 * cosW0 + twoSqrtAAlpha;
      a1 = -2 * (am1 + ap1 * cosW0);
      a2 = ap1 + am1 * cosW0 - twoSqrtAAlpha;
    case EqFilterKind.highShelf:
      final twoSqrtAAlpha = 2 * math.sqrt(a) * alpha;
      final ap1 = a + 1, am1 = a - 1;
      b0 = a * (ap1 + am1 * cosW0 + twoSqrtAAlpha);
      b1 = -2 * a * (am1 + ap1 * cosW0);
      b2 = a * (ap1 + am1 * cosW0 - twoSqrtAAlpha);
      a0 = ap1 - am1 * cosW0 + twoSqrtAAlpha;
      a1 = 2 * (am1 - ap1 * cosW0);
      a2 = ap1 - am1 * cosW0 - twoSqrtAAlpha;
  }

  if (a0.abs() < 1e-30) return const _Coeffs(1, 0, 0, 0, 0);
  final inv = 1 / a0;
  return _Coeffs(b0 * inv, b1 * inv, b2 * inv, a1 * inv, a2 * inv);
}

double _magnitudeDb(_Coeffs c, double sr, double f) {
  final w = 2 * math.pi * f / sr;
  final cos1 = math.cos(w), sin1 = math.sin(w);
  final cos2 = math.cos(2 * w), sin2 = math.sin(2 * w);
  final numRe = c.b0 + c.b1 * cos1 + c.b2 * cos2;
  final numIm = -(c.b1 * sin1 + c.b2 * sin2);
  final denRe = 1 + c.a1 * cos1 + c.a2 * cos2;
  final denIm = -(c.a1 * sin1 + c.a2 * sin2);
  final n2 = numRe * numRe + numIm * numIm;
  final d2 = denRe * denRe + denIm * denIm;
  if (d2 < 1e-30) return 0;
  return 10 * math.log(n2 / d2) / math.ln10;
}

double _log10(double x) => math.log(x) / math.ln10;

String _fmtHz(double hz) => hz >= 1000
    ? '${(hz / 1000).toStringAsFixed(hz >= 10000 ? 0 : 1)}k'
    : '${hz.round()}';

String _fmtDb(double db) => '${db >= 0 ? '+' : ''}${db.toStringAsFixed(1)}';
