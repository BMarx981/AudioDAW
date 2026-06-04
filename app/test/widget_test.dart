import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:daw/engine/engine_interface.dart';
import 'package:daw/ui/channel_strip.dart';
import 'package:daw/ui/home_page.dart';
import 'package:daw/ui/oscilloscope.dart';
import 'package:daw/ui/waveform_view.dart';

import 'fake_engine.dart';

/// A non-empty [MixerMeters] snapshot so per-track `trackLevels()` calls don't
/// pop into "out of range" silence when the test setup uses a default stream.
MixerMeters _silentSnapshot(int tracks) => MixerMeters(
      trackPeaksL: List.filled(tracks, 0),
      trackPeaksR: List.filled(tracks, 0),
      masterPeakL: 0,
      masterPeakR: 0,
    );

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
    Widget homePage(
      FakeEngine engine, {
      String? path = '/tmp/test.wav',
      String? saveProjectPath = '/tmp/proj.json',
      String? openProjectPath = '/tmp/proj.json',
    }) =>
        MaterialApp(
          home: HomePage(
            engine: engine,
            pickWavPath: () async => path,
            pickProjectSavePath: () async => saveProjectPath,
            pickProjectOpenPath: () async => openProjectPath,
          ),
        );

    /// Tap the "+" tile [n] times so the home page has `n` tracks. The mixer
    /// for the existing M4-style flows assumes tracks exist; the M5 default is
    /// an empty project.
    Future<void> addTracks(WidgetTester tester, int n) async {
      for (var i = 0; i < n; i++) {
        final add = find.byTooltip('Add track');
        await tester.ensureVisible(add);
        await tester.tap(add);
        await tester.pumpAndSettle();
      }
    }

    /// Pump a HomePage and pre-populate it with [tracks] tracks. The default
    /// matches the M4 baseline (two tracks) so existing test bodies need only
    /// switch their setup helper.
    Future<void> pumpHome(
      WidgetTester tester,
      FakeEngine engine, {
      String? path = '/tmp/test.wav',
      int tracks = 2,
    }) async {
      await tester.pumpWidget(homePage(engine, path: path));
      await addTracks(tester, tracks);
    }

    /// Tap the n-th "Open WAV…" button (0 = Track 1, 1 = Track 2).
    Future<void> openTrack(WidgetTester tester, int n) async {
      final btn = find.text('Open WAV…').at(n);
      await tester.ensureVisible(btn);
      await tester.tap(btn);
      await tester.pumpAndSettle();
    }

    testWidgets('starts with no tracks, only the master strip', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      // No Open buttons yet — tracks come from the "+" tile.
      expect(find.text('Open WAV…'), findsNothing);
      // Only the master strip is in the row by default.
      expect(find.byType(ChannelStrip), findsOneWidget);
      expect(find.text('Master'), findsOneWidget);
      // The Add tile is present and enabled.
      expect(find.byTooltip('Add track'), findsOneWidget);
      // Empty-state hint is visible.
      expect(find.textContaining('No tracks yet'), findsOneWidget);
    });

    testWidgets('Add tile creates a track with its own strip and row', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));
      await addTracks(tester, 1);

      expect(find.text('Open WAV…'), findsOneWidget);
      expect(find.text('No file loaded'), findsOneWidget);
      // Mixer row: one track + master.
      expect(find.byType(ChannelStrip), findsNWidgets(2));
      expect(find.text('Track 1'), findsWidgets);
    });

    testWidgets(
      'remove button drops the track and asks the engine to clear it',
      (tester) async {
        final engine = FakeEngine();
        addTearDown(engine.dispose);
        await pumpHome(tester, engine);
        // Two tracks now. The first close button removes Track 1 (engine slot 0).
        final close = find.byTooltip('Remove track').first;
        await tester.ensureVisible(close);
        await tester.tap(close);
        await tester.pumpAndSettle();

        expect(engine.clearedTracks, [0]);
        // One track remains; total strips = 1 + master.
        expect(find.byType(ChannelStrip), findsNWidgets(2));
      },
    );

    testWidgets(
      'saving emits a Project with one Track per UI track',
      (tester) async {
        final engine = FakeEngine();
        addTearDown(engine.dispose);
        await pumpHome(tester, engine);
        // Open a WAV into Track 1 so its clipPath ends up in the snapshot.
        await openTrack(tester, 0);

        await tester.tap(find.byTooltip('Save project…'));
        await tester.pumpAndSettle();

        expect(engine.savedProjects, hasLength(1));
        final saved = engine.savedProjects.single;
        expect(saved.path, '/tmp/proj.json');
        expect(saved.project.tracks, hasLength(2));
        expect(saved.project.tracks[0].clipPath, '/tmp/test.wav');
        expect(saved.project.tracks[0].name, 'Track 1');
        expect(saved.project.tracks[1].clipPath, isNull);
      },
    );

    testWidgets(
      'loading applies the project: tracks, params, and clip loads',
      (tester) async {
        final engine = FakeEngine()
          ..loadProjectResult = const Project(
            name: 'Demo',
            tracks: [
              Track(
                name: 'Kick',
                clipPath: '/tmp/kick.wav',
                gainDb: -6,
                pan: -0.25,
              ),
              Track(name: 'Bass', clipPath: null, gainDb: 0, pan: 0.5),
            ],
            master: MasterBus(gainDb: -3, pan: 0),
          );
        addTearDown(engine.dispose);
        await tester.pumpWidget(homePage(engine));

        await tester.tap(find.byTooltip('Open project…'));
        await tester.pumpAndSettle();

        // Two tracks in the row (plus master).
        expect(find.byType(ChannelStrip), findsNWidgets(3));
        expect(find.text('Kick'), findsWidgets);
        expect(find.text('Bass'), findsWidgets);

        // Only Track 1 had a clip path → exactly one loadWav was attempted.
        expect(engine.loadedClips, hasLength(1));
        expect(engine.loadedClips.single.path, '/tmp/kick.wav');
        expect(engine.loadedClips.single.track, 0);

        // Parameters got pushed to the engine (one per track from _applyProject).
        expect(engine.trackGainLinears, hasLength(2));
        expect(engine.trackPans, hasLength(2));
        expect(engine.masterGainLinears, hasLength(1));

        // Project name appears in the AppBar title.
        expect(find.textContaining('Demo'), findsOneWidget);
      },
    );

    testWidgets(
      'a missing referenced WAV marks just that track, not the whole project',
      (tester) async {
        final engine = FakeEngine()
          ..loadError = 'file not found'
          ..loadProjectResult = const Project(
            tracks: [Track(name: 'Track 1', clipPath: '/tmp/gone.wav')],
            master: MasterBus(),
          );
        addTearDown(engine.dispose);
        await tester.pumpWidget(homePage(engine));

        await tester.tap(find.byTooltip('Open project…'));
        await tester.pumpAndSettle();

        // The track exists in the UI…
        expect(find.byType(ChannelStrip), findsNWidgets(2)); // track + master
        // …and the open-WAV row flags it as missing.
        expect(find.textContaining('(missing)'), findsOneWidget);
      },
    );

    testWidgets('opening a WAV into Track 1 loads it for track 0', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 0);

      expect(engine.loadedClips, hasLength(1));
      expect(engine.loadedClips.first.track, 0);
      expect(engine.loadedClips.first.path, '/tmp/test.wav');
      // Waveform now appears for the (selected) track 0.
      expect(find.byType(WaveformView), findsOneWidget);
      expect(find.text('test.wav'), findsOneWidget);
      // "No file loaded" only remains for Track 2 now.
      expect(find.text('No file loaded'), findsOneWidget);
    });

    testWidgets('opening Track 2 targets track index 1', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 1);

      expect(engine.loadedClips.last.track, 1);
    });

    testWidgets('a cancelled pick loads nothing', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine, path: null);

      await openTrack(tester, 0);

      expect(engine.loadedClips, isEmpty);
      expect(find.byType(WaveformView), findsNothing);
    });

    testWidgets('Play and Pause drive the engine and toggle the label', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 0);

      await tester.ensureVisible(find.text('Play'));
      await tester.tap(find.text('Play'));
      await tester.pump();
      expect(engine.playCount, 1);
      expect(find.text('Pause'), findsOneWidget);

      await tester.tap(find.text('Pause'));
      await tester.pump();
      expect(engine.pauseCount, 1);
      expect(find.text('Play'), findsOneWidget);
    });

    testWidgets('Stop stops the engine', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 0);
      await tester.ensureVisible(find.text('Stop'));
      await tester.tap(find.text('Stop'));
      await tester.pump();

      expect(engine.stopCount, 1);
    });

    testWidgets('playback-status stream advances the position label', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 0);

      engine.emitPlaybackState(
        const PlaybackState(positionSecs: 0.5, playing: true),
      );
      await tester.pumpAndSettle();

      // 0.5 s of a 1.0 s clip.
      expect(find.textContaining('00:00.5 / 00:01.0'), findsOneWidget);
    });

    testWidgets('scrubbing the waveform seeks the engine', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await openTrack(tester, 0);

      await tester.tap(find.byType(WaveformView));
      await tester.pump();

      expect(engine.seeks, isNotEmpty);
      expect(engine.seeks.last, inInclusiveRange(0.0, 1.0));
    });

    testWidgets('a load error surfaces as a snackbar', (tester) async {
      final engine = FakeEngine()..loadError = 'not a wav';
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);

      await tester.ensureVisible(find.text('Open WAV…').first);
      await tester.tap(find.text('Open WAV…').first);
      await tester.pumpAndSettle();

      expect(find.textContaining('not a wav'), findsOneWidget);
      expect(find.byType(WaveformView), findsNothing);
    });

    testWidgets(
      'dragging Track 1\'s gain fader sends a per-track command',
      (tester) async {
        final engine = FakeEngine();
        addTearDown(engine.dispose);
        await pumpHome(tester, engine);
        await openTrack(tester, 0);

        // 12 Slider widgets in the mixer row (3 strips × 4 sliders each, the
        // 4th being pan). The first strip's sliders come first, then track 2,
        // then master.
        final sliders = find.byType(Slider);
        // Active gain fader for Track 1 is the very first slider.
        await tester.ensureVisible(sliders.first);
        await tester.drag(sliders.first, const Offset(0, 40));
        await tester.pump();

        expect(
          engine.trackGainLinears,
          isNotEmpty,
          reason: 'gain drag should reach engine',
        );
        expect(
          engine.trackGainLinears.last.track,
          0,
          reason: 'Track 1\'s slider must target track index 0',
        );
        expect(
          engine.masterGainLinears,
          isEmpty,
          reason: 'master must not see the track drag',
        );
      },
    );

    testWidgets(
      'dragging the master fader sends a master command',
      (tester) async {
        final engine = FakeEngine();
        addTearDown(engine.dispose);
        await pumpHome(tester, engine);
        await openTrack(tester, 0);

        // Master is the third strip in the row; its active gain fader is the
        // 9th slider (0..3 = track1, 4..7 = track2, 8..11 = master).
        final sliders = find.byType(Slider);
        await tester.ensureVisible(sliders.at(8));
        await tester.drag(sliders.at(8), const Offset(0, 40));
        await tester.pump();

        expect(engine.masterGainLinears, isNotEmpty);
        expect(engine.trackGainLinears, isEmpty);
      },
    );

    testWidgets('selecting Track 2 retargets the EQ panel', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);
      await openTrack(tester, 0);
      await openTrack(tester, 1);

      // EQ panel default-targets the first track.
      expect(find.text('EQ — Track 1'), findsOneWidget);

      // Tap the Track 2 strip header (specifically the one inside the second
      // ChannelStrip — 'Track 2' also appears in the open-WAV row label).
      final track2Header = find.descendant(
        of: find.byType(ChannelStrip).at(1),
        matching: find.text('Track 2'),
      );
      await tester.ensureVisible(track2Header);
      await tester.tap(track2Header);
      await tester.pumpAndSettle();

      expect(find.text('EQ — Track 2'), findsOneWidget);
    });

    testWidgets('the loop button toggles looping on the engine', (
      tester,
    ) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await pumpHome(tester, engine);
      await openTrack(tester, 0);

      await tester.ensureVisible(find.byTooltip('Loop'));
      await tester.tap(find.byTooltip('Loop'));
      await tester.pump();
      expect(engine.loopings, [true]);

      await tester.tap(find.byTooltip('Loop'));
      await tester.pump();
      expect(engine.loopings, [true, false]);
    });

    testWidgets('mixer-meters event repaints without error', (tester) async {
      final engine = FakeEngine();
      addTearDown(engine.dispose);
      await tester.pumpWidget(homePage(engine));

      engine.emitMixerMeters(MixerMeters(
        trackPeaksL: List.filled(engine.maxTracks, 0.4),
        trackPeaksR: List.filled(engine.maxTracks, 0.4),
        masterPeakL: 0.5,
        masterPeakR: 0.5,
      ));
      await tester.pump();
      expect(tester.takeException(), isNull);
    });
  });

  group('ChannelStrip', () {
    Widget host({
      double gainLinear = 1,
      GainFaderMode gainMode = GainFaderMode.db6,
      double pan = 0,
      String title = 'Channel',
      ValueChanged<double>? onGainLinear,
      ValueChanged<double>? onPan,
      ValueChanged<GainFaderMode>? onMode,
    }) => MaterialApp(
          home: Scaffold(
            body: Center(
              child: ChannelStrip(
                title: title,
                gainLinear: gainLinear,
                gainMode: gainMode,
                pan: pan,
                meter: const Stream<MeterLevels>.empty(),
                onGainLinearChanged: onGainLinear,
                onPanChanged: onPan,
                onGainModeChanged: onMode,
              ),
            ),
          ),
        );

    testWidgets('renders three gain faders, a pan slider, and a meter', (
      tester,
    ) async {
      await tester.pumpWidget(host(gainLinear: 1, pan: 0));

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
      await tester.pumpWidget(host(gainLinear: 0, pan: -0.5));

      // Both dB faders (db6, db12) read -∞ at a muted gain.
      expect(find.text('-∞'), findsNWidgets(2));
      expect(find.text('L 50'), findsOneWidget);
    });

    testWidgets('switching the active fader reports a mode change', (
      tester,
    ) async {
      GainFaderMode? mode;
      await tester.pumpWidget(host(onMode: (m) => mode = m));

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

    testWidgets('honors the title and selected highlight', (tester) async {
      await tester.pumpWidget(host(title: 'Master'));
      expect(find.text('Master'), findsOneWidget);
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

  group('MixerMeters', () {
    test('trackLevels(t) returns the right pair', () {
      final m = MixerMeters(
        trackPeaksL: const [0.1, 0.2, 0.3],
        trackPeaksR: const [0.4, 0.5, 0.6],
        masterPeakL: 0.7,
        masterPeakR: 0.8,
      );
      expect(m.trackLevels(1).peakLeft, 0.2);
      expect(m.trackLevels(1).peakRight, 0.5);
      expect(m.masterLevels.peakLeft, 0.7);
    });

    test('out-of-range track index returns silence', () {
      final m = _silentSnapshot(0);
      expect(m.trackLevels(5).peakLeft, 0);
      expect(m.trackLevels(5).peakRight, 0);
    });
  });
}
