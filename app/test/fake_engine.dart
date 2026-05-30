import 'dart:async';
import 'dart:typed_data';

import 'package:daw/engine/engine_interface.dart';

/// Test double for [EngineInterface]. Records calls so tests can assert on them,
/// and never touches Rust. This is the seam TESTING.md describes — widget tests
/// run entirely against this.
class FakeEngine implements EngineInterface {
  final List<String> loadedPaths = [];
  int playCount = 0;
  int pauseCount = 0;
  int stopCount = 0;
  final List<double> seeks = [];
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
  Future<ClipInfo> loadWav(String path) async {
    if (loadError != null) throw loadError!;
    loadedPaths.add(path);
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
  bool get isRunning => _running;

  /// Streams are driven manually in tests via [emitScopeFrame] /
  /// [emitPlaybackState]. They default to no events so widgets settle under
  /// `pumpAndSettle` — we never want a free-running periodic stream in a test.
  final StreamController<Float32List> _scope =
      StreamController<Float32List>.broadcast();
  final StreamController<PlaybackState> _playback =
      StreamController<PlaybackState>.broadcast();

  @override
  Stream<Float32List> get scopeFrames => _scope.stream;

  @override
  Stream<PlaybackState> get playbackState => _playback.stream;

  void emitScopeFrame(Float32List frame) => _scope.add(frame);
  void emitPlaybackState(PlaybackState state) => _playback.add(state);

  /// The most recent path passed to [loadWav], or null if none.
  String? get lastLoadedPath => loadedPaths.isEmpty ? null : loadedPaths.last;

  void dispose() {
    _scope.close();
    _playback.close();
  }
}
