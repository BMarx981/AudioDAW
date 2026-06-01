import 'dart:async';
import 'dart:typed_data';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';
import 'channel_strip.dart';
import 'eq_view.dart';
import 'oscilloscope.dart';
import 'waveform_view.dart';

/// Number of user-visible tracks for Milestone 4 — two playable tracks plus a
/// master bus. The engine's strip pool is larger (`engine.maxTracks`) so M5 can
/// expose more without an audio-thread refactor.
const int kVisibleTracks = 2;

/// Open a native file dialog and return the chosen WAV's path, or null if the
/// user cancelled. The default file-picker for [HomePage]; tests inject their own
/// so they never pop a real dialog.
Future<String?> pickWavWithDialog() async {
  const wav = XTypeGroup(label: 'WAV audio', extensions: ['wav']);
  final file = await openFile(acceptedTypeGroups: const [wav]);
  return file?.path;
}

/// The Milestone 4 UI: open WAVs into two tracks, mix them through gain/pan/EQ,
/// summed to a master bus with its own fader and meter.
///
/// The widget stays dumb — it owns only view state and forwards intent to the
/// injected [EngineInterface]. It never imports the Rust bridge, which is what
/// lets the widget test drive it with a fake engine and a fake file picker.
class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.engine, this.pickWavPath});

  final EngineInterface engine;

  /// Returns the path of a WAV to load, or null to cancel. Defaults to a native
  /// open dialog; tests inject a stub.
  final Future<String?> Function()? pickWavPath;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  // Per-track state, sized to the user-visible track count. The engine's pool
  // is larger; we don't index past kVisibleTracks here.
  final List<ClipInfo?> _clips = List.filled(kVisibleTracks, null);
  final List<String?> _filenames = List.filled(kVisibleTracks, null);
  // Per-track mixer state. Each is independent so a fade on track 1 doesn't
  // touch track 2.
  late final List<double> _trackGain = List.filled(kVisibleTracks, 1.0);
  late final List<GainFaderMode> _trackGainMode =
      List.filled(kVisibleTracks, GainFaderMode.db6);
  late final List<double> _trackPan = List.filled(kVisibleTracks, 0.0);
  late final List<List<EqBand>> _trackEqBands =
      List.generate(kVisibleTracks, (_) => List.of(kDefaultEqBands));

  // Master bus state.
  double _masterGain = 1.0;
  GainFaderMode _masterGainMode = GainFaderMode.db6;
  double _masterPan = 0;

  // Global transport state. With one playhead per engine, every track plays in
  // sync — so a single set of fields is correct.
  double _positionSecs = 0;
  bool _playing = false;
  bool _looping = false;

  // The track whose EQ panel is currently being edited (Milestone 4 UX: one EQ
  // panel below the mixer, with strip selection swapping its focus).
  int _selectedTrack = 0;

  // Set when a load is in flight — guards each track's "Open WAV…" against
  // double-taps. Tracked per track so loading track 1 doesn't lock track 0's
  // button.
  late final List<bool> _busy = List.filled(kVisibleTracks, false);

  late final double _eqSampleRate = widget.engine.engineSampleRate;
  late final StreamSubscription<PlaybackState> _statusSub;
  late final Stream<Float32List> _scopeFrames = widget.engine.scopeFrames;

  /// One subscription to the engine's mixer-wide meter stream; per-strip views
  /// are derived as broadcast streams from this so each [ChannelStrip] gets
  /// only its own track's data.
  late final Stream<MixerMeters> _mixerMeters =
      widget.engine.mixerMeters.asBroadcastStream();

  // Per-track meter views derived from the mixer stream. Cached so each rebuild
  // doesn't re-create the mapped stream (which would re-subscribe on every
  // build and churn the StreamBuilder).
  late final List<Stream<MeterLevels>> _trackMeters = List.generate(
    kVisibleTracks,
    (t) => _mixerMeters.map((m) => m.trackLevels(t)).asBroadcastStream(),
  );
  late final Stream<MeterLevels> _masterMeter =
      _mixerMeters.map((m) => m.masterLevels).asBroadcastStream();

  @override
  void initState() {
    super.initState();
    _statusSub = widget.engine.playbackState.listen((s) {
      if (!mounted) return;
      setState(() {
        _positionSecs = s.positionSecs;
        _playing = s.playing;
      });
    });
  }

  @override
  void dispose() {
    _statusSub.cancel();
    super.dispose();
  }

  // --- Selected-track helpers ---------------------------------------------

  ClipInfo? get _selectedClip => _clips[_selectedTrack];
  double get _duration => _selectedClip?.durationSecs ?? 0;
  double get _positionFraction =>
      _duration > 0 ? (_positionSecs / _duration).clamp(0.0, 1.0) : 0.0;

  // --- Transport ----------------------------------------------------------

  Future<void> _openWavFor(int track) async {
    if (_busy[track]) return;
    final pick = widget.pickWavPath ?? pickWavWithDialog;
    final path = await pick();
    if (path == null) return; // cancelled

    setState(() => _busy[track] = true);
    try {
      final clip = await widget.engine.loadWav(track, path);
      setState(() {
        _clips[track] = clip;
        _filenames[track] = _basename(path);
        _positionSecs = 0;
        _playing = false;
      });
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Could not load WAV: $e')),
        );
      }
    } finally {
      if (mounted) setState(() => _busy[track] = false);
    }
  }

  /// True once at least one track has a clip — only then do the transport
  /// controls make sense to show as active.
  bool get _anyClipLoaded => _clips.any((c) => c != null);

  void _togglePlay() {
    if (!_anyClipLoaded) return;
    if (_playing) {
      widget.engine.pause();
      setState(() => _playing = false);
    } else {
      widget.engine.play();
      setState(() => _playing = true);
    }
  }

  void _stop() {
    if (!_anyClipLoaded) return;
    widget.engine.stop();
    setState(() {
      _playing = false;
      _positionSecs = 0;
    });
  }

  void _toggleLoop() {
    setState(() => _looping = !_looping);
    widget.engine.setLooping(_looping);
  }

  void _onSeek(double fraction) {
    if (_selectedClip == null) return;
    final secs = fraction * _duration;
    // Optimistic local update so the playhead tracks the gesture immediately;
    // the status stream confirms it on the next tick.
    setState(() => _positionSecs = secs);
    widget.engine.seek(secs);
  }

  // --- Per-track mixer callbacks ------------------------------------------

  void _onTrackGainChanged(int track, double linear) {
    setState(() => _trackGain[track] = linear);
    widget.engine.setTrackGainLinear(track, linear);
  }

  void _onTrackGainModeChanged(int track, GainFaderMode mode) {
    setState(() => _trackGainMode[track] = mode);
  }

  void _onTrackPanChanged(int track, double pan) {
    setState(() => _trackPan[track] = pan);
    widget.engine.setTrackPan(track, pan);
  }

  // --- Master bus callbacks -----------------------------------------------

  void _onMasterGainChanged(double linear) {
    setState(() => _masterGain = linear);
    widget.engine.setMasterGainLinear(linear);
  }

  void _onMasterGainModeChanged(GainFaderMode mode) {
    setState(() => _masterGainMode = mode);
  }

  void _onMasterPanChanged(double pan) {
    setState(() => _masterPan = pan);
    widget.engine.setMasterPan(pan);
  }

  // --- EQ panel (targets the selected track) ------------------------------

  void _updateSelectedBand(int i, EqBand band) {
    setState(() {
      _trackEqBands[_selectedTrack] = [..._trackEqBands[_selectedTrack]]
        ..[i] = band;
    });
  }

  void _onEqFreq(int i, double hz) {
    _updateSelectedBand(i, _trackEqBands[_selectedTrack][i].copyWith(freqHz: hz));
    widget.engine.setTrackEqBandFreq(_selectedTrack, i, hz);
  }

  void _onEqGain(int i, double db) {
    _updateSelectedBand(i, _trackEqBands[_selectedTrack][i].copyWith(gainDb: db));
    widget.engine.setTrackEqBandGainDb(_selectedTrack, i, db);
  }

  void _onEqQ(int i, double q) {
    _updateSelectedBand(i, _trackEqBands[_selectedTrack][i].copyWith(q: q));
    widget.engine.setTrackEqBandQ(_selectedTrack, i, q);
  }

  void _onEqKind(int i, EqFilterKind kind) {
    _updateSelectedBand(i, _trackEqBands[_selectedTrack][i].copyWith(kind: kind));
    widget.engine.setTrackEqBandKind(_selectedTrack, i, kind);
  }

  void _onEqEnabled(int i, bool on) {
    _updateSelectedBand(i, _trackEqBands[_selectedTrack][i].copyWith(enabled: on));
    widget.engine.setTrackEqBandEnabled(_selectedTrack, i, on);
  }

  // --- Build --------------------------------------------------------------

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('DAW — Milestone 4 (Mixer)')),
      body: Center(
        child: SingleChildScrollView(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 880),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  // Per-track "Open WAV" rows — one per visible track.
                  for (int t = 0; t < kVisibleTracks; t++) ...[
                    _OpenWavRow(
                      label: 'Track ${t + 1}',
                      filename: _filenames[t],
                      busy: _busy[t],
                      onOpen: () => _openWavFor(t),
                    ),
                    const SizedBox(height: 8),
                  ],
                  const SizedBox(height: 8),

                  // Waveform of the currently-selected track. Mostly there to
                  // anchor the scrubber to whichever clip the user is editing.
                  if (_selectedClip != null) ...[
                    WaveformView(
                      min: _selectedClip!.waveformMin,
                      max: _selectedClip!.waveformMax,
                      positionFraction: _positionFraction,
                      onSeek: _onSeek,
                    ),
                    const SizedBox(height: 4),
                    Text(
                      'Track ${_selectedTrack + 1}  •  '
                      '${_fmt(_positionSecs)} / ${_fmt(_duration)}'
                      '   •   ${_selectedClip!.sampleRate.toStringAsFixed(0)} Hz'
                      '   •   ${_selectedClip!.channels == 1 ? 'mono' : '${_selectedClip!.channels} ch'}',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                    const SizedBox(height: 16),
                  ],

                  // Global transport.
                  Row(
                    mainAxisAlignment: MainAxisAlignment.center,
                    children: [
                      FilledButton.icon(
                        onPressed: _anyClipLoaded ? _togglePlay : null,
                        icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
                        label: Text(_playing ? 'Pause' : 'Play'),
                      ),
                      const SizedBox(width: 12),
                      OutlinedButton.icon(
                        onPressed: _anyClipLoaded ? _stop : null,
                        icon: const Icon(Icons.stop),
                        label: const Text('Stop'),
                      ),
                      const SizedBox(width: 12),
                      IconButton(
                        onPressed: _toggleLoop,
                        isSelected: _looping,
                        tooltip: 'Loop',
                        icon: const Icon(Icons.repeat),
                        selectedIcon: const Icon(Icons.repeat_on),
                      ),
                    ],
                  ),
                  const SizedBox(height: 24),

                  // Mixer row: two track strips + master strip, side by side.
                  // Wrapped in a horizontal scroller so it stays usable on
                  // narrow windows (and inside the 800×600 widget-test viewport)
                  // — real DAW mixers scroll horizontally anyway.
                  SingleChildScrollView(
                    scrollDirection: Axis.horizontal,
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        for (int t = 0; t < kVisibleTracks; t++) ...[
                          ChannelStrip(
                            title: 'Track ${t + 1}',
                            selected: _selectedTrack == t,
                            onTap: () =>
                                setState(() => _selectedTrack = t),
                            gainLinear: _trackGain[t],
                            gainMode: _trackGainMode[t],
                            pan: _trackPan[t],
                            meter: _trackMeters[t],
                            onGainLinearChanged: (lin) =>
                                _onTrackGainChanged(t, lin),
                            onGainModeChanged: (m) =>
                                _onTrackGainModeChanged(t, m),
                            onPanChanged: (p) => _onTrackPanChanged(t, p),
                          ),
                          const SizedBox(width: 12),
                        ],
                        ChannelStrip(
                          title: 'Master',
                          gainLinear: _masterGain,
                          gainMode: _masterGainMode,
                          pan: _masterPan,
                          meter: _masterMeter,
                          onGainLinearChanged: _onMasterGainChanged,
                          onGainModeChanged: _onMasterGainModeChanged,
                          onPanChanged: _onMasterPanChanged,
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(height: 24),

                  // EQ panel — edits whichever track strip is selected. The
                  // header makes the focus obvious; the strip's highlight in
                  // the row above is the visual companion.
                  Text(
                    'EQ — Track ${_selectedTrack + 1}',
                    style: Theme.of(context).textTheme.labelMedium,
                  ),
                  const SizedBox(height: 8),
                  EqView(
                    bands: _trackEqBands[_selectedTrack],
                    sampleRate: _eqSampleRate,
                    onFreqChanged: _onEqFreq,
                    onGainChanged: _onEqGain,
                    onQChanged: _onEqQ,
                    onKindChanged: _onEqKind,
                    onEnabledChanged: _onEqEnabled,
                  ),
                  const SizedBox(height: 24),
                  Text(
                    'Master output',
                    style: Theme.of(context).textTheme.labelMedium,
                  ),
                  const SizedBox(height: 8),
                  Oscilloscope(frames: _scopeFrames, height: 200),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// One row of the per-track "Open WAV…" header: a track label, an open button,
/// and the current filename (or a placeholder).
class _OpenWavRow extends StatelessWidget {
  const _OpenWavRow({
    required this.label,
    required this.filename,
    required this.busy,
    required this.onOpen,
  });

  final String label;
  final String? filename;
  final bool busy;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        SizedBox(
          width: 64,
          child: Text(label, style: Theme.of(context).textTheme.labelMedium),
        ),
        FilledButton.icon(
          onPressed: busy ? null : onOpen,
          icon: const Icon(Icons.folder_open),
          label: const Text('Open WAV…'),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Text(
            filename ?? 'No file loaded',
            overflow: TextOverflow.ellipsis,
            style: Theme.of(context).textTheme.bodyMedium,
          ),
        ),
      ],
    );
  }
}

/// `mm:ss.t` for a duration in seconds.
String _fmt(double secs) {
  if (secs.isNaN || secs < 0) secs = 0;
  final m = secs ~/ 60;
  final s = secs - m * 60;
  return '${m.toString().padLeft(2, '0')}:${s.toStringAsFixed(1).padLeft(4, '0')}';
}

/// Last path segment of [path], handling both `/` and `\` so we don't pull in
/// `dart:io` just to show a filename.
String _basename(String path) {
  final cut = path.lastIndexOf(RegExp(r'[/\\]'));
  return cut < 0 ? path : path.substring(cut + 1);
}
