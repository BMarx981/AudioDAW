import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:daw/engine/engine_interface.dart';
import 'package:daw/ui/eq_view.dart';

void main() {
  const sr = 48000.0;

  group('eqResponseDb (mirrors engine dsp::biquad)', () {
    test('flat default EQ is ~0 dB everywhere', () {
      for (final f in const [100.0, 1000.0, 8000.0]) {
        final db = eqResponseDb(kDefaultEqBands, sr, f);
        expect(
          db.abs(),
          lessThan(0.3),
          reason: 'flat EQ should be ~0 dB at $f Hz, got $db',
        );
      }
    });

    test(
      'a +12 dB bell lifts its center by ~12 dB and leaves distant tones flat',
      () {
        final bands = [
          const EqBand(
            kind: EqFilterKind.peak,
            freqHz: 1000,
            q: 1.0,
            gainDb: 12,
            enabled: true,
          ),
        ];
        expect((eqResponseDb(bands, sr, 1000) - 12).abs(), lessThan(1.0));
        expect(eqResponseDb(bands, sr, 60).abs(), lessThan(1.0));
      },
    );

    test('low-pass attenuates well above cutoff', () {
      final bands = [
        const EqBand(
          kind: EqFilterKind.lowpass,
          freqHz: 1000,
          q: 0.707,
          gainDb: 0,
          enabled: true,
        ),
      ];
      expect(eqResponseDb(bands, sr, 100).abs(), lessThan(1.0));
      expect(eqResponseDb(bands, sr, 10000), lessThan(-20));
    });

    test('a disabled band drops out of the curve', () {
      final bands = [
        const EqBand(
          kind: EqFilterKind.peak,
          freqHz: 1000,
          q: 1.0,
          gainDb: 12,
          enabled: false,
        ),
      ];
      expect(eqResponseDb(bands, sr, 1000).abs(), lessThan(0.01));
    });

    test('two boosts sum in dB', () {
      final bands = [
        const EqBand(
          kind: EqFilterKind.peak,
          freqHz: 2000,
          q: 1.0,
          gainDb: 6,
          enabled: true,
        ),
        const EqBand(
          kind: EqFilterKind.peak,
          freqHz: 2000,
          q: 1.0,
          gainDb: 6,
          enabled: true,
        ),
      ];
      expect(eqResponseDb(bands, sr, 2000), greaterThan(10));
    });
  });

  testWidgets('EqView reports edits through its callbacks', (tester) async {
    int? enabledBand;
    bool? enabledValue;
    EqFilterKind? changedKind;
    int? kindBand;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: EqView(
            bands: kDefaultEqBands,
            sampleRate: sr,
            onEnabledChanged: (i, v) {
              enabledBand = i;
              enabledValue = v;
            },
            onKindChanged: (i, k) {
              kindBand = i;
              changedKind = k;
            },
          ),
        ),
      ),
    );

    expect(find.text('Equalizer'), findsOneWidget);
    // Four band kind dropdowns (one per band).
    expect(find.byType(DropdownButton<EqFilterKind>), findsNWidgets(4));

    // Toggle the first band off.
    await tester.tap(find.byType(Checkbox).first);
    await tester.pump();
    expect(enabledBand, 0);
    expect(enabledValue, false);

    // Change band 0's kind via the dropdown.
    await tester.tap(find.byType(DropdownButton<EqFilterKind>).first);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Notch').last);
    await tester.pumpAndSettle();
    expect(kindBand, 0);
    expect(changedKind, EqFilterKind.notch);
  });

  testWidgets('EQ response curve golden', (tester) async {
    // A distinctive shape: low-shelf cut, mid bell boost, high-pass — drawn with
    // a fixed accent so the golden is deterministic.
    const bands = [
      EqBand(
        kind: EqFilterKind.lowShelf,
        freqHz: 120,
        q: 0.707,
        gainDb: -9,
        enabled: true,
      ),
      EqBand(
        kind: EqFilterKind.peak,
        freqHz: 1000,
        q: 2.0,
        gainDb: 12,
        enabled: true,
      ),
      EqBand(
        kind: EqFilterKind.peak,
        freqHz: 3000,
        q: 1.0,
        gainDb: 0,
        enabled: false,
      ),
      EqBand(
        kind: EqFilterKind.highpass,
        freqHz: 60,
        q: 0.707,
        gainDb: 0,
        enabled: true,
      ),
    ];
    await tester.pumpWidget(
      Center(
        child: RepaintBoundary(
          child: SizedBox(
            width: 600,
            height: 200,
            child: CustomPaint(
              painter: EqResponsePainter(
                bands: bands,
                sampleRate: sr,
                accent: const Color(0xFF00E5FF),
              ),
              size: const Size(600, 200),
            ),
          ),
        ),
      ),
    );
    await expectLater(
      find.byType(RepaintBoundary),
      matchesGoldenFile('goldens/eq_response.png'),
    );
  });
}
