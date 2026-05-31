import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:daw/engine/engine_interface.dart';
import 'package:daw/ui/channel_strip.dart';
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

    testWidgets('the channel strip appears once a clip is loaded', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      expect(find.byType(ChannelStrip), findsNothing);
      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();
      expect(find.byType(ChannelStrip), findsOneWidget);
    });

    testWidgets('dragging gain and pan drives the engine', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));
      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      // Four sliders: the three gain faders (db6, linear, db12) then pan.
      final sliders = find.byType(Slider);
      expect(sliders, findsNWidgets(4));

      // The strip lives below the fold in the test surface; scroll each control
      // into view before dragging it. The active gain fader (db6) is first.
      await tester.ensureVisible(sliders.at(0));
      await tester.drag(sliders.at(0), const Offset(0, 40)); // gain fader
      await tester.pump();
      await tester.ensureVisible(sliders.at(3));
      await tester.drag(sliders.at(3), const Offset(-60, 0)); // pan toward left
      await tester.pump();

      expect(
        engine.gainLinears,
        isNotEmpty,
        reason: 'gain drag should reach engine',
      );
      expect(engine.pans, isNotEmpty, reason: 'pan drag should reach engine');
      // Dragging the pan slider left moves toward -1.
      expect(engine.pans.last, lessThan(0.0));
      // Gain is a non-negative linear multiplier.
      expect(engine.gainLinears.last, greaterThanOrEqualTo(0.0));
    });

    testWidgets('the loop button toggles looping on the engine', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));
      await tester.tap(find.text('Open WAV…'));
      await tester.pumpAndSettle();

      await tester.tap(find.byTooltip('Loop'));
      await tester.pump();
      expect(engine.loopings, [true]);

      await tester.tap(find.byTooltip('Loop'));
      await tester.pump();
      expect(engine.loopings, [true, false]);
    });

    testWidgets('switching the active fader reports a mode change', (
      tester,
    ) async {
      GainFaderMode? mode;
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Center(
              child: ChannelStrip(
                gainLinear: 1,
                gainMode: GainFaderMode.db6,
                pan: 0,
                meter: engine.meterLevels,
                onGainModeChanged: (m) => mode = m,
              ),
            ),
          ),
        ),
      );

      // 'Lin' appears both on the segment and the fader's own label; target the
      // segment.
      await tester.tap(
        find.descendant(
          of: find.byType(SegmentedButton<GainFaderMode>),
          matching: find.text('Lin'),
        ),
      );
      await tester.pump();
      expect(mode, GainFaderMode.linear);
    });
  });

  group('ChannelStrip', () {
    Widget host(
      FakeEngine engine, {
      double gainLinear = 1,
      GainFaderMode gainMode = GainFaderMode.db6,
      double pan = 0,
      ValueChanged<double>? onGainLinear,
      ValueChanged<double>? onPan,
    }) => MaterialApp(
      home: Scaffold(
        body: Center(
          child: ChannelStrip(
            gainLinear: gainLinear,
            gainMode: gainMode,
            pan: pan,
            meter: engine.meterLevels,
            onGainLinearChanged: onGainLinear,
            onPanChanged: onPan,
          ),
        ),
      ),
    );

    testWidgets('renders three gain faders, a pan slider, and a meter', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(host(engine, gainLinear: 1, pan: 0));

      expect(find.byType(Slider), findsNWidgets(4)); // 3 gain + 1 pan
      expect(find.byType(SegmentedButton<GainFaderMode>), findsOneWidget);
      expect(find.text('dB  −60…+6'), findsOneWidget); // active-mode caption
      expect(find.text('C'), findsOneWidget); // centered pan
      // At unity, both dB faders read +0.0 dB and the linear fader reads 1.00×.
      expect(find.text('+0.0 dB'), findsNWidgets(2));
      expect(find.text('1.00×'), findsOneWidget);
    });

    testWidgets('mute reads -∞ on the dB faders; off-center pan as L/R', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(host(engine, gainLinear: 0, pan: -0.5));

      // Both dB faders (db6, db12) read -∞ at a muted gain.
      expect(find.text('-∞'), findsNWidgets(2));
      expect(find.text('L 50'), findsOneWidget);
    });

    testWidgets('meter stream updates repaint without error', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(host(engine));

      engine.emitMeterLevels(const MeterLevels(peakLeft: 0.8, peakRight: 0.3));
      await tester.pump();
      expect(tester.takeException(), isNull);
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
