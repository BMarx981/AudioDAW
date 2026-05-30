import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:daw/engine/engine_interface.dart';
import 'package:daw/ui/home_page.dart';
import 'package:daw/ui/oscilloscope.dart';
import 'package:daw/ui/waveform_view.dart';

import 'fake_engine.dart';

void main() {
  group('WaveformView', () {
    testWidgets('renders a waveform from min/max data without error', (
      tester,
    ) async {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: WaveformView(
              min: Float32List.fromList(const [-0.2, -0.8, -0.5]),
              max: Float32List.fromList(const [0.2, 0.8, 0.5]),
              positionFraction: 0.25,
            ),
          ),
        ),
      );
      await tester.pump();
      expect(tester.takeException(), isNull);
      expect(find.byType(CustomPaint), findsWidgets);
    });

    testWidgets('handles empty data without error', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: WaveformView(min: Float32List(0), max: Float32List(0)),
          ),
        ),
      );
      await tester.pump();
      expect(tester.takeException(), isNull);
    });

    testWidgets('tapping reports a seek fraction in [0, 1]', (tester) async {
      double? seeked;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Center(
              child: SizedBox(
                width: 200,
                child: WaveformView(
                  min: Float32List.fromList(const [-1, -1]),
                  max: Float32List.fromList(const [1, 1]),
                  onSeek: (f) => seeked = f,
                ),
              ),
            ),
          ),
        ),
      );

      // Tap the centre of the 200px-wide waveform => fraction ~0.5.
      await tester.tap(find.byType(WaveformView));
      await tester.pump();

      expect(seeked, isNotNull);
      expect(seeked, closeTo(0.5, 0.05));
    });
  });

  group('HomePage', () {
    // A HomePage wired to a fake engine and a stub file picker that returns a
    // fixed path, so no native dialog is ever shown.
    Widget homePage(FakeEngine engine, {String? path = '/tmp/test.wav'}) =>
        MaterialApp(
          home: HomePage(engine: engine, pickWavPath: () async => path),
        );

    testWidgets('Open loads a clip and reveals the waveform + transport', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      // Nothing loaded yet.
      expect(find.byType(WaveformView), findsNothing);
      expect(find.text('No file loaded'), findsOneWidget);

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      expect(engine.lastLoadedPath, '/tmp/test.wav');
      expect(find.byType(WaveformView), findsOneWidget);
      expect(find.text('Play'), findsOneWidget);
      expect(find.text('test.wav'), findsOneWidget);
    });

    testWidgets('a cancelled pick loads nothing', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine, path: null)); // user cancels

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      expect(engine.loadedPaths, isEmpty);
      expect(find.byType(WaveformView), findsNothing);
    });

    testWidgets('Play and Pause drive the engine and toggle the label', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      await tester.tap(find.text('Play'));
      await tester.pump();
      expect(engine.playCount, 1);
      expect(find.text('Pause'), findsOneWidget);

      await tester.tap(find.text('Pause'));
      await tester.pump();
      expect(engine.pauseCount, 1);
      expect(find.text('Play'), findsOneWidget);
    });

    testWidgets('Stop stops the engine and rewinds', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Stop'));
      await tester.pump();

      expect(engine.stopCount, 1);
    });

    testWidgets('playback-status stream advances the position label', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine)); // 1.0s clip

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      engine.emitPlaybackState(
        const PlaybackState(positionSecs: 0.5, playing: true),
      );
      await tester.pumpAndSettle(); // let the broadcast-stream event deliver

      // 0.5 s of a 1.0 s clip.
      expect(find.textContaining('00:00.5 / 00:01.0'), findsOneWidget);
    });

    testWidgets('scrubbing the waveform seeks the engine', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      await tester.tap(find.byType(WaveformView));
      await tester.pump();

      expect(engine.seeks, isNotEmpty, reason: 'a tap should seek');
      // Seek target is within the clip's 1.0 s duration.
      expect(engine.seeks.last, inInclusiveRange(0.0, 1.0));
    });

    testWidgets('a load error surfaces as a snackbar', (tester) async {
      final engine = FakeEngine()..loadError = 'not a wav';
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      await tester.tap(find.text('Open WAV…'));
      await tester.pump(); // run the load future + build the snackbar
      await tester.pump();

      expect(find.textContaining('not a wav'), findsOneWidget);
      expect(find.byType(WaveformView), findsNothing);
    });
  });

  group('Oscilloscope', () {
    testWidgets('renders on the home page', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: HomePage(engine: engine, pickWavPath: () async => null),
        ),
      );

      expect(find.byType(Oscilloscope), findsOneWidget);
      engine.emitScopeFrame(
        Float32List.fromList(List.generate(256, (i) => i.isEven ? 0.5 : -0.5)),
      );
      await tester.pump();
      expect(tester.takeException(), isNull);
    });
  });
}
