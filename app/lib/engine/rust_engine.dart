import 'dart:typed_data';

import '../src/rust/api/engine_api.dart' as rust;
import 'engine_interface.dart';

/// Production [EngineInterface] backed by the Rust engine over flutter_rust_bridge.
///
/// A thin adapter: it maps the generated bridge types to the plain value types in
/// [engine_interface.dart] so the UI and tests never depend on generated code.
class RustEngine implements EngineInterface {
  @override
  Future<ClipInfo> loadWav(String path) async {
    final c = await rust.loadWav(path: path);
    return ClipInfo(
      sampleRate: c.sampleRate,
      channels: c.channels,
      frames: c.frames.toInt(),
      durationSecs: c.durationSecs,
      waveformMin: c.waveformMin,
      waveformMax: c.waveformMax,
    );
  }

  @override
  void play() => rust.play();

  @override
  void pause() => rust.pause();

  @override
  void stop() => rust.stop();

  @override
  void seek(double secs) => rust.seek(secs: secs);

  @override
  bool get isRunning => rust.isRunning();

  /// Cached so repeated reads don't each spawn a Rust-side pump thread.
  Stream<Float32List>? _scopeFrames;

  @override
  Stream<Float32List> get scopeFrames =>
      _scopeFrames ??= rust.scopeStream().asBroadcastStream();

  Stream<PlaybackState>? _playbackState;

  @override
  Stream<PlaybackState> get playbackState => _playbackState ??= rust
      .playbackStatusStream()
      .map(
        (s) => PlaybackState(positionSecs: s.positionSecs, playing: s.playing),
      )
      .asBroadcastStream();
}
