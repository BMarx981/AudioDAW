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
  void setLooping(bool looping) => rust.setLooping(looping: looping);

  @override
  void setGainDb(double db) => rust.setGainDb(db: db);

  @override
  void setGainLinear(double linear) => rust.setGainLinear(linear: linear);

  @override
  void setPan(double pan) => rust.setPan(pan: pan);

  @override
  void setEqBandKind(int band, EqFilterKind kind) =>
      rust.setEqBandKind(band: band, kind: kind.code);

  @override
  void setEqBandFreq(int band, double hz) =>
      rust.setEqBandFreq(band: band, hz: hz);

  @override
  void setEqBandQ(int band, double q) => rust.setEqBandQ(band: band, q: q);

  @override
  void setEqBandGainDb(int band, double db) =>
      rust.setEqBandGainDb(band: band, db: db);

  @override
  void setEqBandEnabled(int band, bool on) =>
      rust.setEqBandEnabled(band: band, on_: on);

  @override
  double get engineSampleRate => rust.engineSampleRate();

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

  Stream<MeterLevels>? _meterLevels;

  @override
  Stream<MeterLevels> get meterLevels => _meterLevels ??= rust
      .meterStream()
      .map((m) => MeterLevels(peakLeft: m.peakLeft, peakRight: m.peakRight))
      .asBroadcastStream();
}
