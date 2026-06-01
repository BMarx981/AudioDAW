import 'dart:async';
import 'dart:typed_data';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';
import 'channel_strip.dart';
import 'eq_view.dart';
import 'oscilloscope.dart';
import 'waveform_view.dart';

/// Open a native file dialog and return the chosen WAV's path, or null if the
/// user cancelled. The default file-picker for [HomePage]; tests inject their own
/// so they never pop a real dialog.
Future<String?> pickWavWithDialog() async {
  const wav = XTypeGroup(label: 'WAV audio', extensions: ['wav']);
  final file = await openFile(acceptedTypeGroups: const [wav]);
  return file?.path;
}

/// The Milestone 1 UI: open a WAV, see its waveform, scrub it, and play it back,
/// with a live oscilloscope of the output.
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
  ClipInfo? _clip;
  String? _filename;
  double _positionSecs = 0;
  bool _playing = false;
  bool _busy = false; // guards the async load against double taps
  bool _looping = false;
  double _gainLinear = 1; // unity
  GainFaderMode _gainMode = GainFaderMode.db6;
  double _pan = 0; // center

  // EQ band state mirrors the engine's defaults; the engine keeps the smoothed
  // audio-rate copy. The sample rate (read once) is what the curve is drawn at.
  List<EqBand> _eqBands = List.of(kDefaultEqBands);
  late final double _eqSampleRate = widget.engine.engineSampleRate;

  late final StreamSubscription<PlaybackState> _statusSub;

  // Subscribe to the continuous streams once; the engine spawns a pump per
  // subscription, so we must not read these getters on every build.
  late final Stream<Float32List> _scopeFrames = widget.engine.scopeFrames;
  late final Stream<MeterLevels> _meterLevels = widget.engine.meterLevels;

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

  double get _duration => _clip?.durationSecs ?? 0;
  double get _positionFraction =>
      _duration > 0 ? (_positionSecs / _duration).clamp(0.0, 1.0) : 0.0;

  Future<void> _openWav() async {
    if (_busy) return;
    final pick = widget.pickWavPath ?? pickWavWithDialog;
    final path = await pick();
    if (path == null) return; // cancelled

    setState(() => _busy = true);
    try {
      final clip = await widget.engine.loadWav(path);
      setState(() {
        _clip = clip;
        _filename = _basename(path);
        _positionSecs = 0;
        _playing = false;
      });
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(SnackBar(content: Text('Could not load WAV: $e')));
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _togglePlay() {
    if (_clip == null) return;
    if (_playing) {
      widget.engine.pause();
      setState(() => _playing = false);
    } else {
      widget.engine.play();
      setState(() => _playing = true);
    }
  }

  void _stop() {
    if (_clip == null) return;
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
    if (_clip == null) return;
    final secs = fraction * _duration;
    // Optimistic local update so the playhead tracks the gesture immediately;
    // the status stream confirms it on the next tick.
    setState(() => _positionSecs = secs);
    widget.engine.seek(secs);
  }

  void _onGainLinearChanged(double linear) {
    setState(() => _gainLinear = linear);
    widget.engine.setGainLinear(linear);
  }

  void _onGainModeChanged(GainFaderMode mode) {
    setState(() => _gainMode = mode);
  }

  void _onPanChanged(double pan) {
    setState(() => _pan = pan);
    widget.engine.setPan(pan);
  }

  // EQ edits: update local state for the curve, then forward to the engine.
  void _updateBand(int i, EqBand band) {
    setState(() => _eqBands = [..._eqBands]..[i] = band);
  }

  void _onEqFreq(int i, double hz) {
    _updateBand(i, _eqBands[i].copyWith(freqHz: hz));
    widget.engine.setEqBandFreq(i, hz);
  }

  void _onEqGain(int i, double db) {
    _updateBand(i, _eqBands[i].copyWith(gainDb: db));
    widget.engine.setEqBandGainDb(i, db);
  }

  void _onEqQ(int i, double q) {
    _updateBand(i, _eqBands[i].copyWith(q: q));
    widget.engine.setEqBandQ(i, q);
  }

  void _onEqKind(int i, EqFilterKind kind) {
    _updateBand(i, _eqBands[i].copyWith(kind: kind));
    widget.engine.setEqBandKind(i, kind);
  }

  void _onEqEnabled(int i, bool on) {
    _updateBand(i, _eqBands[i].copyWith(enabled: on));
    widget.engine.setEqBandEnabled(i, on);
  }

  @override
  Widget build(BuildContext context) {
    final clip = _clip;
    return Scaffold(
      appBar: AppBar(title: const Text('DAW — Milestone 3 (EQ)')),
      body: Center(
        child: SingleChildScrollView(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 640),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Row(
                    children: [
                      FilledButton.icon(
                        onPressed: _busy ? null : _openWav,
                        icon: const Icon(Icons.folder_open),
                        label: const Text('Open WAV…'),
                      ),
                      const SizedBox(width: 16),
                      Expanded(
                        child: Text(
                          _filename ?? 'No file loaded',
                          overflow: TextOverflow.ellipsis,
                          style: Theme.of(context).textTheme.bodyMedium,
                        ),
                      ),
                    ],
                  ),
                  const SizedBox(height: 20),
                  if (clip != null) ...[
                    WaveformView(
                      min: clip.waveformMin,
                      max: clip.waveformMax,
                      positionFraction: _positionFraction,
                      onSeek: _onSeek,
                    ),
                    const SizedBox(height: 8),
                    Text(
                      '${_fmt(_positionSecs)} / ${_fmt(_duration)}'
                      '   •   ${clip.sampleRate.toStringAsFixed(0)} Hz'
                      '   •   ${clip.channels == 1 ? 'mono' : '${clip.channels} ch'}',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                    const SizedBox(height: 16),
                    Row(
                      mainAxisAlignment: MainAxisAlignment.center,
                      children: [
                        FilledButton.icon(
                          onPressed: _togglePlay,
                          icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
                          label: Text(_playing ? 'Pause' : 'Play'),
                        ),
                        const SizedBox(width: 12),
                        OutlinedButton.icon(
                          onPressed: _stop,
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
                    Center(
                      child: ChannelStrip(
                        gainLinear: _gainLinear,
                        gainMode: _gainMode,
                        pan: _pan,
                        meter: _meterLevels,
                        onGainLinearChanged: _onGainLinearChanged,
                        onGainModeChanged: _onGainModeChanged,
                        onPanChanged: _onPanChanged,
                      ),
                    ),
                    const SizedBox(height: 24),
                    EqView(
                      bands: _eqBands,
                      sampleRate: _eqSampleRate,
                      onFreqChanged: _onEqFreq,
                      onGainChanged: _onEqGain,
                      onQChanged: _onEqQ,
                      onKindChanged: _onEqKind,
                      onEnabledChanged: _onEqEnabled,
                    ),
                    const SizedBox(height: 24),
                  ],
                  Text(
                    'Output',
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
