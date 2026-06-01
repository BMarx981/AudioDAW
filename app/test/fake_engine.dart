import 'dart:async';
import 'dart:typed_data';

import 'package:daw/engine/engine_interface.dart';

/// Test double for [EngineInterface]. Records calls so tests can assert on them,
/// and never touches Rust. This is the seam TESTING.md describes — widget tests
/// run entirely against this.
class FakeEngine implements EngineInterface {
  @override
  int get maxTracks => 8;

  /// Every (track, path) pair `loadWav` was called with, in call order.
  final List<({int track, String path})> loadedClips = [];
  int playCount = 0;
  int pauseCount = 0;
  int stopCount = 0;
  final List<double> seeks = [];
  final List<bool> loopings = [];
  final List<({int track, double db})> trackGainDbs = [];
  final List<({int track, double linear})> trackGainLinears = [];
  final List<({int track, double pan})> trackPans = [];
  final List<String> trackEqCalls = [];
  final List<double> masterGainDbs = [];
  final List<double> masterGainLinears = [];
  final List<double> masterPans = [];
  bool _running = false;

  /// If set, [loadWav] throws this — used to test the error path.
  Object? loadError;

  /// What [loadWav] returns when it succeeds. Defaults to a tiny two-column
  /// waveform so widgets have something to draw.
  ClipInfo loadResult = ClipInfo(
    sampleRate: 48000,
    channels: 1,
    frames: 48000,
    durationSecs: 1.0,
    waveformMin: Float32List.fromList(const [-0.5, -1.0]),
    waveformMax: Float32List.fromList(const [0.5, 1.0]),
  );

  @override
  Future<ClipInfo> loadWav(int track, String path) async {
    if (loadError != null) throw loadError!;
    loadedClips.add((track: track, path: path));
    _running = true;
    return loadResult;
  }

  @override
  void play() => playCount++;

  @override
  void pause() => pauseCount++;

  @override
  void stop() => stopCount++;

  @override
  void seek(double secs) => seeks.add(secs);

  @override
  void setLooping(bool looping) => loopings.add(looping);

  @override
  void setTrackGainDb(int track, double db) =>
      trackGainDbs.add((track: track, db: db));

  @override
  void setTrackGainLinear(int track, double linear) =>
      trackGainLinears.add((track: track, linear: linear));

  @override
  void setTrackPan(int track, double pan) =>
      trackPans.add((track: track, pan: pan));

  @override
  void setTrackEqBandKind(int track, int band, EqFilterKind kind) =>
      trackEqCalls.add('kind:$track:$band:${kind.name}');

  @override
  void setTrackEqBandFreq(int track, int band, double hz) =>
      trackEqCalls.add('freq:$track:$band:$hz');

  @override
  void setTrackEqBandQ(int track, int band, double q) =>
      trackEqCalls.add('q:$track:$band:$q');

  @override
  void setTrackEqBandGainDb(int track, int band, double db) =>
      trackEqCalls.add('gain:$track:$band:$db');

  @override
  void setTrackEqBandEnabled(int track, int band, bool on) =>
      trackEqCalls.add('enabled:$track:$band:$on');

  @override
  void setMasterGainDb(double db) => masterGainDbs.add(db);

  @override
  void setMasterGainLinear(double linear) => masterGainLinears.add(linear);

  @override
  void setMasterPan(double pan) => masterPans.add(pan);

  @override
  double get engineSampleRate => 48000;

  @override
  bool get isRunning => _running;

  /// Streams are driven manually in tests via [emitScopeFrame] /
  /// [emitPlaybackState]. They default to no events so widgets settle under
  /// `pumpAndSettle` — we never want a free-running periodic stream in a test.
  final StreamController<Float32List> _scope =
      StreamController<Float32List>.broadcast();
  final StreamController<PlaybackState> _playback =
      StreamController<PlaybackState>.broadcast();
  final StreamController<MixerMeters> _meters =
      StreamController<MixerMeters>.broadcast();

  @override
  Stream<Float32List> get scopeFrames => _scope.stream;

  @override
  Stream<PlaybackState> get playbackState => _playback.stream;

  @override
  Stream<MixerMeters> get mixerMeters => _meters.stream;

  void emitScopeFrame(Float32List frame) => _scope.add(frame);
  void emitPlaybackState(PlaybackState state) => _playback.add(state);
  void emitMixerMeters(MixerMeters meters) => _meters.add(meters);

  /// The most recent path passed to [loadWav], or null if none.
  String? get lastLoadedPath =>
      loadedClips.isEmpty ? null : loadedClips.last.path;

  void dispose() {
    _scope.close();
    _playback.close();
    _meters.close();
  }
}
