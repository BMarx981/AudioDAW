import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:daw/ui/home_page.dart';
import 'package:daw/ui/frequency_mapping.dart';

import 'fake_engine.dart';

void main() {
  group('frequency mapping', () {
    test('maps slider extremes to 20 Hz and 2000 Hz', () {
      expect(sliderToHz(0.0), closeTo(20.0, 1e-6));
      expect(sliderToHz(1.0), closeTo(2000.0, 1e-6));
    });

    test('midpoint is the geometric mean (200 Hz)', () {
      // Log mapping => the centre is sqrt(20 * 2000) = 200 Hz, not 1010.
      expect(sliderToHz(0.5), closeTo(200.0, 1e-3));
    });

    test('hzToSlider is the inverse of sliderToHz', () {
      for (final v in [0.0, 0.25, 0.5, 0.75, 1.0]) {
        expect(hzToSlider(sliderToHz(v)), closeTo(v, 1e-9));
      }
    });
  });

  group('HomePage', () {
    testWidgets('dragging the slider pushes the log-mapped frequency',
        (tester) async {
      final engine = FakeEngine();
      await tester.pumpWidget(MaterialApp(home: HomePage(engine: engine)));

      // Drag the slider to the right. The exact gesture distance doesn't need to
      // land on a precise value — we assert the engine was driven and that the
      // pushed value matches the displayed Hz via the log mapping.
      await tester.drag(find.byType(Slider), const Offset(500, 0));
      await tester.pump();

      expect(engine.frequencies, isNotEmpty,
          reason: 'slider drag should call setFrequency');

      final hz = engine.lastFrequency!;
      expect(find.text('${hz.toStringAsFixed(1)} Hz'), findsOneWidget);
      expect(hz, inInclusiveRange(kMinHz, kMaxHz));
    });

    testWidgets('Play starts the engine and pushes the initial frequency',
        (tester) async {
      final engine = FakeEngine();
      await tester.pumpWidget(MaterialApp(home: HomePage(engine: engine)));

      await tester.tap(find.text('Play'));
      await tester.pumpAndSettle();

      expect(engine.startCount, 1);
      expect(engine.frequencies, isNotEmpty,
          reason: 'Play should send the current frequency to the engine');
      expect(find.text('Stop'), findsOneWidget);
    });

    testWidgets('Stop stops the engine', (tester) async {
      final engine = FakeEngine();
      await tester.pumpWidget(MaterialApp(home: HomePage(engine: engine)));

      await tester.tap(find.text('Play'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Stop'));
      await tester.pumpAndSettle();

      expect(engine.stopCount, 1);
      expect(find.text('Play'), findsOneWidget);
    });

    testWidgets('a start error surfaces as a snackbar and stays stopped',
        (tester) async {
      final engine = FakeEngine()..startError = 'no audio device';
      await tester.pumpWidget(MaterialApp(home: HomePage(engine: engine)));

      await tester.tap(find.text('Play'));
      await tester.pump(); // build the snackbar

      expect(find.textContaining('no audio device'), findsOneWidget);
      expect(find.text('Play'), findsOneWidget, reason: 'should remain stopped');
    });
  });
}
