import 'dart:typed_data';

import '../src/rust/api/engine_api.dart' as rust;
import '../src/rust/project.dart' as rust_proj;
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
  void clearTrack(int track) => rust.clearTrack(track: track);

  @override
  Future<void> saveProject(String path, Project project) =>
      rust.saveProject(path: path, project: _toBridge(project));

  @override
  Future<Project> loadProject(String path) async {
    final p = await rust.loadProject(path: path);
    return _fromBridge(p);
  }

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

  // --- Project bridge conversion ------------------------------------------
  //
  // Pure data shuffling: a [Project] (UI-facing) becomes a generated
  // `ProjectFile` for the trip across the bridge, and vice versa. The only
  // non-mechanical bit is the EQ filter-kind enum <-> int code mapping, which
  // matches engine/src/dsp/biquad.rs's `FilterKind::from_code`. Errors here
  // (e.g. an unknown int code from a hand-edited JSON file) fall back to a
  // transparent peaking bell — same behavior as the Rust side.

  rust_proj.ProjectFile _toBridge(Project p) => rust_proj.ProjectFile(
    // Always write the current schema; the Rust side ignores the field on
    // save and overwrites it, but supplying it keeps the type total.
    formatVersion: 1,
    name: p.name,
    tracks: [for (final t in p.tracks) _trackToBridge(t)],
    master: _masterToBridge(p.master),
  );

  rust_proj.TrackState _trackToBridge(Track t) => rust_proj.TrackState(
    name: t.name,
    clipPath: t.clipPath,
    gainDb: t.gainDb,
    pan: t.pan,
    eqBands: [for (final b in t.eqBands) _bandToBridge(b)],
  );

  rust_proj.MasterState _masterToBridge(MasterBus m) => rust_proj.MasterState(
    gainDb: m.gainDb,
    pan: m.pan,
    eqBands: [for (final b in m.eqBands) _bandToBridge(b)],
  );

  rust_proj.EqBandState _bandToBridge(EqBand b) => rust_proj.EqBandState(
    kind: b.kind.code,
    freqHz: b.freqHz,
    q: b.q,
    gainDb: b.gainDb,
    enabled: b.enabled,
  );

  Project _fromBridge(rust_proj.ProjectFile p) => Project(
    name: p.name,
    tracks: [for (final t in p.tracks) _trackFromBridge(t)],
    master: _masterFromBridge(p.master),
  );

  Track _trackFromBridge(rust_proj.TrackState t) => Track(
    name: t.name,
    clipPath: t.clipPath,
    gainDb: t.gainDb,
    pan: t.pan,
    eqBands: [for (final b in t.eqBands) _bandFromBridge(b)],
  );

  MasterBus _masterFromBridge(rust_proj.MasterState m) => MasterBus(
    gainDb: m.gainDb,
    pan: m.pan,
    eqBands: [for (final b in m.eqBands) _bandFromBridge(b)],
  );

  EqBand _bandFromBridge(rust_proj.EqBandState b) => EqBand(
    kind: EqFilterKind.values.firstWhere(
      (k) => k.code == b.kind,
      // Unknown code (e.g. a future kind, or a corrupted file) falls back to a
      // transparent peaking bell — matches the Rust `FilterKind::from_code`
      // default.
      orElse: () => EqFilterKind.peak,
    ),
    freqHz: b.freqHz,
    q: b.q,
    gainDb: b.gainDb,
    enabled: b.enabled,
  );
}
