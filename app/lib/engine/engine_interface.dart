import 'dart:typed_data';

/// The seam between the UI and the audio engine.
///
/// Per TESTING.md the UI never talks to the Rust bridge directly — it depends on
/// this interface and on the plain value types below ([ClipInfo],
/// [PlaybackState]), never on generated bridge classes. Production wires in
/// [RustEngine]; widget tests wire in a `FakeEngine` that returns scripted data.
abstract class EngineInterface {
  /// Decode a WAV at [path], hand it to the engine (starting the audio device if
  /// needed), and return its metadata + waveform summary. The clip loads stopped
  /// at the start — call [play] to hear it. Throws if the file can't be decoded.
  Future<ClipInfo> loadWav(String path);

  /// Begin or resume playback. Fire-and-forget; no-op if nothing is loaded.
  void play();

  /// Pause, holding the current position. Fire-and-forget.
  void pause();

  /// Stop and rewind to the start. Fire-and-forget.
  void stop();

  /// Seek to [secs] from the clip start. Fire-and-forget; safe on every scrub
  /// tick (the engine clamps to the clip bounds).
  void seek(double secs);

  /// Turn looping on/off. When on, playback wraps to the start at the clip end
  /// instead of stopping. Fire-and-forget.
  void setLooping(bool looping);

  /// Set the channel-strip gain in decibels. Fire-and-forget; safe on every knob
  /// tick — the engine clamps and smooths it, so a drag is click-free.
  void setGainDb(double db);

  /// Set the channel-strip gain as a raw linear multiplier. Fire-and-forget;
  /// clamped and smoothed by the engine. (Drives the linear gain fader.)
  void setGainLinear(double linear);

  /// Set the channel-strip pan in [-1, 1] (-1 = left, 0 = center, +1 = right).
  /// Fire-and-forget; clamped and smoothed by the engine.
  void setPan(double pan);

  /// Set EQ band [band]'s filter kind. Fire-and-forget.
  void setEqBandKind(int band, EqFilterKind kind);

  /// Set EQ band [band]'s center/corner frequency in Hz. Fire-and-forget; safe
  /// on every knob tick — smoothed by the engine, so a sweep is click-free.
  void setEqBandFreq(int band, double hz);

  /// Set EQ band [band]'s Q (bandwidth). Fire-and-forget; smoothed.
  void setEqBandQ(int band, double q);

  /// Set EQ band [band]'s gain in dB (peak/shelf kinds). Fire-and-forget; smoothed.
  void setEqBandGainDb(int band, double db);

  /// Enable/disable EQ band [band] (true bypass when off). Fire-and-forget.
  void setEqBandEnabled(int band, bool on);

  /// The engine's output sample rate (Hz). The EQ response curve is drawn at
  /// this rate so it matches what the audio thread actually filters with.
  double get engineSampleRate;

  /// Whether the audio engine is running (device open).
  bool get isRunning;

  /// A stream of oscilloscope frames — one trigger-aligned window of mono samples
  /// in [-1, 1] per event, the live output of the player. Subscribe once and
  /// cache it; the production implementation spawns a pump per subscription.
  Stream<Float32List> get scopeFrames;

  /// A stream of transport snapshots (~30 Hz) for animating the playhead and
  /// reflecting play/stop. Subscribe once and cache it.
  Stream<PlaybackState> get playbackState;

  /// A stream of post-fader peak levels (~60 Hz) for the channel-strip meter.
  /// Subscribe once and cache it; the production implementation spawns a pump
  /// per subscription.
  Stream<MeterLevels> get meterLevels;
}

/// A decoded clip's metadata plus its precomputed min/max waveform summary.
///
/// The waveform is computed once in Rust at load (not per frame); [waveformMin]
/// and [waveformMax] are the per-column extremes of the mono signal in [-1, 1],
/// the same length, ready for a [CustomPainter] to draw as vertical bars.
class ClipInfo {
  const ClipInfo({
    required this.sampleRate,
    required this.channels,
    required this.frames,
    required this.durationSecs,
    required this.waveformMin,
    required this.waveformMax,
  });

  final double sampleRate;
  final int channels;
  final int frames;
  final double durationSecs;
  final Float32List waveformMin;
  final Float32List waveformMax;
}

/// A transport snapshot: where the playhead is and whether audio is advancing.
class PlaybackState {
  const PlaybackState({required this.positionSecs, required this.playing});

  final double positionSecs;
  final bool playing;

  static const stopped = PlaybackState(positionSecs: 0, playing: false);
}

/// The biquad filter shapes an EQ band can take. A UI-side mirror of the
/// engine's `FilterKind`, kept here so the UI and tests never import generated
/// bridge code. [RustEngine] maps it to the bridge enum.
enum EqFilterKind {
  peak(0, 'Bell'),
  lowShelf(1, 'Lo Shelf'),
  highShelf(2, 'Hi Shelf'),
  lowpass(3, 'Lo Pass'),
  highpass(4, 'Hi Pass'),
  bandpass(5, 'Band'),
  notch(6, 'Notch');

  const EqFilterKind(this.code, this.label);

  /// Wire code matching the engine's `filter_kind_from_code` mapping.
  final int code;

  /// Short label for a dropdown.
  final String label;

  /// Whether this kind uses the gain parameter (peak and shelves do; the pass
  /// filters and notch don't).
  bool get usesGain =>
      this == EqFilterKind.peak ||
      this == EqFilterKind.lowShelf ||
      this == EqFilterKind.highShelf;
}

/// Post-fader peak levels per channel, linear (0..≈1, may exceed 1 if boosted).
/// The meter widget maps these to its own dB scale.
class MeterLevels {
  const MeterLevels({required this.peakLeft, required this.peakRight});

  final double peakLeft;
  final double peakRight;

  static const silent = MeterLevels(peakLeft: 0, peakRight: 0);
}
