import 'dart:typed_data';

/// The seam between the UI and the audio engine.
///
/// Per TESTING.md the UI never talks to the Rust bridge directly — it depends on
/// this interface and on the plain value types below ([ClipInfo],
/// [PlaybackState], [Project] …), never on generated bridge classes. Production
/// wires in [RustEngine]; widget tests wire in a `FakeEngine` that returns
/// scripted data.
///
/// ## Multitrack (Milestone 4)
///
/// Parameter setters take a `track` index; matching `master*` setters drive
/// the master bus. Transport (`play`/`pause`/`stop`/`seek`) is global — one
/// playhead advances every track. The meter stream is a single [MixerMeters]
/// event per tick carrying the per-track peaks and the master bus peak together,
/// so the UI drives all the meters from one subscription.
///
/// ## Project model (Milestone 5)
///
/// The UI owns the canonical per-track state (slider positions, EQ bands…); on
/// Save it pours that into a [Project] DTO and calls [saveProject], which
/// writes JSON. On Load, [loadProject] returns the same DTO and the UI then
/// drives the engine setters (and `loadWav` calls) to bring the audio side
/// into sync. Removing a track in the UI calls [clearTrack] to drop that
/// strip's clip + reset its parameters, freeing the engine pool slot for
/// reuse. `loadWav` failures stay on the throwing-Future contract, so a load
/// that references a missing WAV surfaces per-track without aborting the
/// whole project — the UI catches the error and marks just that track empty.
abstract class EngineInterface {
  /// The track-pool capacity exposed by the engine. UI state arrays are sized
  /// to this; up to this many tracks can exist at once in a project.
  int get maxTracks;

  /// Decode a WAV at [path], hand it to [track] in the engine (starting the
  /// audio device if needed), and return its metadata + waveform summary. The
  /// clip loads stopped at the start — call [play] to hear it. Throws if the
  /// file can't be decoded.
  Future<ClipInfo> loadWav(int track, String path);

  /// Begin or resume playback on every track. Fire-and-forget; no-op if no clip
  /// is loaded.
  void play();

  /// Pause every track, holding the current position. Fire-and-forget.
  void pause();

  /// Stop every track and rewind to the start. Fire-and-forget.
  void stop();

  /// Seek every track to [secs] from its clip start. Fire-and-forget; safe on
  /// every scrub tick (the engine clamps to the clip bounds).
  void seek(double secs);

  /// Turn looping on/off on every track. When on, playback wraps to the start
  /// at the clip end instead of stopping. Fire-and-forget.
  void setLooping(bool looping);

  /// Set [track]'s gain in decibels. Fire-and-forget; safe on every knob tick.
  void setTrackGainDb(int track, double db);

  /// Set [track]'s gain as a raw linear multiplier. Fire-and-forget.
  void setTrackGainLinear(int track, double linear);

  /// Set [track]'s pan in [-1, 1]. Fire-and-forget.
  void setTrackPan(int track, double pan);

  /// Set [track]'s EQ band filter kind. Fire-and-forget.
  void setTrackEqBandKind(int track, int band, EqFilterKind kind);

  /// Set [track]'s EQ band frequency in Hz. Fire-and-forget; smoothed.
  void setTrackEqBandFreq(int track, int band, double hz);

  /// Set [track]'s EQ band Q. Fire-and-forget; smoothed.
  void setTrackEqBandQ(int track, int band, double q);

  /// Set [track]'s EQ band gain in dB. Fire-and-forget; smoothed.
  void setTrackEqBandGainDb(int track, int band, double db);

  /// Enable/disable [track]'s EQ band. Fire-and-forget.
  void setTrackEqBandEnabled(int track, int band, bool on);

  /// Set the master bus gain in dB. Fire-and-forget.
  void setMasterGainDb(double db);

  /// Set the master bus gain as a raw linear multiplier. Fire-and-forget.
  void setMasterGainLinear(double linear);

  /// Set the master bus pan in [-1, 1]. Fire-and-forget.
  void setMasterPan(double pan);

  /// Drop [track]'s clip and reset its strip to defaults. The pool slot stays
  /// available for reuse. Fire-and-forget.
  void clearTrack(int track);

  /// Write [project] to [path] as JSON. Throws if the file can't be written.
  Future<void> saveProject(String path, Project project);

  /// Read a project from [path]. Throws if the file is missing, malformed, or
  /// from a newer schema than this build supports. The caller is responsible
  /// for then calling [loadWav] for each track's clipPath and pushing the
  /// per-track parameter setters to bring the engine into sync.
  Future<Project> loadProject(String path);

  /// The engine's output sample rate (Hz). The EQ response curve is drawn at
  /// this rate so it matches what the audio thread actually filters with.
  double get engineSampleRate;

  /// Whether the audio engine is running (device open).
  bool get isRunning;

  /// A stream of oscilloscope frames — one trigger-aligned window of mono
  /// samples in [-1, 1] per event, the live output of the master mix. Subscribe
  /// once and cache it; the production implementation spawns a pump per
  /// subscription.
  Stream<Float32List> get scopeFrames;

  /// A stream of transport snapshots (~30 Hz) for animating the playhead and
  /// reflecting play/stop. Subscribe once and cache it.
  Stream<PlaybackState> get playbackState;

  /// A stream of post-fader peak levels (~60 Hz) for every track plus the
  /// master bus. Subscribe once and cache it.
  Stream<MixerMeters> get mixerMeters;
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

/// Post-fader peak levels for one channel strip, linear (0..≈1, may exceed 1 if
/// boosted). The meter widget maps these to its own dB scale. Kept as a
/// separate type from [MixerMeters] so the dumb channel-strip widget can be
/// driven by a single track's stream without knowing about the mixer shape.
class MeterLevels {
  const MeterLevels({required this.peakLeft, required this.peakRight});

  final double peakLeft;
  final double peakRight;

  static const silent = MeterLevels(peakLeft: 0, peakRight: 0);
}

/// A snapshot of every meter the mixer publishes: per-track L/R peaks (one
/// entry per strip slot) plus the master bus L/R peak. Subscribe once; the UI
/// fans the data out to per-strip meters by deriving a [MeterLevels] view.
class MixerMeters {
  const MixerMeters({
    required this.trackPeaksL,
    required this.trackPeaksR,
    required this.masterPeakL,
    required this.masterPeakR,
  });

  final List<double> trackPeaksL;
  final List<double> trackPeaksR;
  final double masterPeakL;
  final double masterPeakR;

  /// The L/R peak for [track] as a [MeterLevels], or silence if out of range.
  MeterLevels trackLevels(int track) {
    if (track < 0 || track >= trackPeaksL.length || track >= trackPeaksR.length) {
      return MeterLevels.silent;
    }
    return MeterLevels(
      peakLeft: trackPeaksL[track],
      peakRight: trackPeaksR[track],
    );
  }

  /// The master L/R peak as a [MeterLevels].
  MeterLevels get masterLevels =>
      MeterLevels(peakLeft: masterPeakL, peakRight: masterPeakR);

  static final silent = MixerMeters(
    trackPeaksL: const [],
    trackPeaksR: const [],
    masterPeakL: 0,
    masterPeakR: 0,
  );
}

/// One EQ band's settings, as the UI holds them (the UI-rate values). The
/// engine keeps its own smoothed audio-rate copy; this is the source of truth
/// for the controls and the drawn response curve. Lives in the engine seam
/// (not the UI layer) because it's also the per-band shape inside a [Track].
class EqBand {
  const EqBand({
    required this.kind,
    required this.freqHz,
    required this.q,
    required this.gainDb,
    required this.enabled,
  });

  final EqFilterKind kind;
  final double freqHz;
  final double q;
  final double gainDb;
  final bool enabled;

  EqBand copyWith({
    EqFilterKind? kind,
    double? freqHz,
    double? q,
    double? gainDb,
    bool? enabled,
  }) => EqBand(
    kind: kind ?? this.kind,
    freqHz: freqHz ?? this.freqHz,
    q: q ?? this.q,
    gainDb: gainDb ?? this.gainDb,
    enabled: enabled ?? this.enabled,
  );
}

/// The default 4-band layout — must match the engine's `Eq::new` defaults so
/// the UI and audio agree from the first frame: low-shelf, two bells,
/// high-shelf, all flat (0 dB).
const List<EqBand> kDefaultEqBands = [
  EqBand(
    kind: EqFilterKind.lowShelf,
    freqHz: 120,
    q: 0.707,
    gainDb: 0,
    enabled: true,
  ),
  EqBand(
    kind: EqFilterKind.peak,
    freqHz: 500,
    q: 1.0,
    gainDb: 0,
    enabled: true,
  ),
  EqBand(
    kind: EqFilterKind.peak,
    freqHz: 3000,
    q: 1.0,
    gainDb: 0,
    enabled: true,
  ),
  EqBand(
    kind: EqFilterKind.highShelf,
    freqHz: 8000,
    q: 0.707,
    gainDb: 0,
    enabled: true,
  ),
];

/// One track's persistent state — what gets written to the project file.
/// Mirrors the Rust `TrackState` (in `engine/src/project.rs`) but uses plain
/// Dart types (e.g. [EqFilterKind] instead of an int code) so the UI never
/// has to think about wire encoding.
class Track {
  const Track({
    required this.name,
    this.clipPath,
    this.gainDb = 0.0,
    this.pan = 0.0,
    this.eqBands = kDefaultEqBands,
  });

  /// User-facing label. Auto-filled to `"Track N"` on creation; preserved
  /// across save/load so a renamed track keeps its name.
  final String name;

  /// Last WAV the track was loaded with, or null for an empty track. Surviving
  /// a save → load with the file moved or deleted is the missing-file case the
  /// UI surfaces on the affected row without aborting the whole project.
  final String? clipPath;

  final double gainDb;
  final double pan;
  final List<EqBand> eqBands;

  Track copyWith({
    String? name,
    Object? clipPath = _unset,
    double? gainDb,
    double? pan,
    List<EqBand>? eqBands,
  }) => Track(
    name: name ?? this.name,
    // copyWith of a nullable field needs a sentinel so callers can clear it
    // (clipPath: null) without it being mistaken for "unchanged".
    clipPath: identical(clipPath, _unset) ? this.clipPath : clipPath as String?,
    gainDb: gainDb ?? this.gainDb,
    pan: pan ?? this.pan,
    eqBands: eqBands ?? this.eqBands,
  );
}

/// Master bus state. Same shape as a [Track] minus identity and clip path;
/// kept separate so the UI can render it without a "this is the master" flag.
class MasterBus {
  const MasterBus({
    this.gainDb = 0.0,
    this.pan = 0.0,
    this.eqBands = kDefaultEqBands,
  });

  final double gainDb;
  final double pan;
  final List<EqBand> eqBands;

  MasterBus copyWith({
    double? gainDb,
    double? pan,
    List<EqBand>? eqBands,
  }) => MasterBus(
    gainDb: gainDb ?? this.gainDb,
    pan: pan ?? this.pan,
    eqBands: eqBands ?? this.eqBands,
  );
}

/// A whole project as the UI holds it and as it lives on disk. `tracks` may
/// have any length up to [EngineInterface.maxTracks]; an empty project is
/// valid (no tracks, default master).
class Project {
  const Project({
    this.name = 'Untitled',
    this.tracks = const [],
    this.master = const MasterBus(),
  });

  final String name;
  final List<Track> tracks;
  final MasterBus master;

  Project copyWith({String? name, List<Track>? tracks, MasterBus? master}) =>
      Project(
        name: name ?? this.name,
        tracks: tracks ?? this.tracks,
        master: master ?? this.master,
      );
}

/// Sentinel object used by copyWith methods to distinguish "set to null" from
/// "don't change". Private to this file.
const Object _unset = Object();
