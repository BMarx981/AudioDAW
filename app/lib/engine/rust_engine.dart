import 'dart:typed_data';

import '../src/rust/api/engine_api.dart' as rust;
import 'engine_interface.dart';

/// Production [EngineInterface] backed by the Rust engine over flutter_rust_bridge.
///
/// A thin adapter: it maps the generated bridge types to the plain value types in
/// [engine_interface.dart] so the UI and tests never depend on generated code.
class RustEngine implements EngineInterface {
  @override
  late final int maxTracks = rust.maxTracks();

  @override
  Future<ClipInfo> loadWav(int track, String path) async {
    final c = await rust.loadWav(track: track, path: path);
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
  void setTrackGainDb(int track, double db) =>
      rust.setTrackGainDb(track: track, db: db);

  @override
  void setTrackGainLinear(int track, double linear) =>
      rust.setTrackGainLinear(track: track, linear: linear);

  @override
  void setTrackPan(int track, double pan) =>
      rust.setTrackPan(track: track, pan: pan);

  @override
  void setTrackEqBandKind(int track, int band, EqFilterKind kind) =>
      rust.setTrackEqBandKind(track: track, band: band, kind: kind.code);

  @override
  void setTrackEqBandFreq(int track, int band, double hz) =>
      rust.setTrackEqBandFreq(track: track, band: band, hz: hz);

  @override
  void setTrackEqBandQ(int track, int band, double q) =>
      rust.setTrackEqBandQ(track: track, band: band, q: q);

  @override
  void setTrackEqBandGainDb(int track, int band, double db) =>
      rust.setTrackEqBandGainDb(track: track, band: band, db: db);

  @override
  void setTrackEqBandEnabled(int track, int band, bool on) =>
      rust.setTrackEqBandEnabled(track: track, band: band, on_: on);

  @override
  void setMasterGainDb(double db) => rust.setMasterGainDb(db: db);

  @override
  void setMasterGainLinear(double linear) =>
      rust.setMasterGainLinear(linear: linear);

  @override
  void setMasterPan(double pan) => rust.setMasterPan(pan: pan);

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

  Stream<MixerMeters>? _mixerMeters;

  @override
  Stream<MixerMeters> get mixerMeters => _mixerMeters ??= rust
      .meterStream()
      .map(
        (m) => MixerMeters(
          trackPeaksL: List<double>.from(m.trackPeaksL),
          trackPeaksR: List<double>.from(m.trackPeaksR),
          masterPeakL: m.masterPeakL,
          masterPeakR: m.masterPeakR,
        ),
      )
      .asBroadcastStream();
}
