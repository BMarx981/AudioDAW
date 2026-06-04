import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';
import 'channel_strip.dart';
import 'eq_view.dart';
import 'oscilloscope.dart';
import 'waveform_view.dart';

/// Open a native file dialog and return the chosen WAV's path, or null if the
/// user cancelled. The default file-picker for [HomePage]; tests inject their
/// own so they never pop a real dialog.
Future<String?> pickWavWithDialog() async {
  const wav = XTypeGroup(label: 'WAV audio', extensions: ['wav']);
  final file = await openFile(acceptedTypeGroups: const [wav]);
  return file?.path;
}

/// Open a native save dialog for the project file. Returns the chosen path or
/// null on cancel.
Future<String?> pickProjectSavePathWithDialog() async {
  const proj = XTypeGroup(label: 'DAW project', extensions: ['json']);
  final loc = await getSaveLocation(
    acceptedTypeGroups: const [proj],
    suggestedName: 'project.json',
  );
  return loc?.path;
}

/// Open a native open dialog for a project file. Returns the chosen path or
/// null on cancel.
Future<String?> pickProjectOpenPathWithDialog() async {
  const proj = XTypeGroup(label: 'DAW project', extensions: ['json']);
  final file = await openFile(acceptedTypeGroups: const [proj]);
  return file?.path;
}

/// The Milestone 5 UI: a dynamic number of tracks (up to `engine.maxTracks`),
/// each with its own gain/pan/EQ chain, summed to a master bus that has its
/// own fader and meter. Tracks can be added and removed at runtime without
/// dropouts (the engine pre-allocates the whole strip pool); projects can be
/// saved to and loaded from JSON.
///
/// The widget stays dumb — it owns only view state and forwards intent to the
/// injected [EngineInterface]. It never imports the Rust bridge, which is what
/// lets widget tests drive it with a fake engine and fake file pickers.
class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    required this.engine,
    this.pickWavPath,
    this.pickProjectSavePath,
    this.pickProjectOpenPath,
  });

  final EngineInterface engine;

  /// Returns the path of a WAV to load, or null to cancel. Defaults to the
  /// native open dialog; tests inject a stub.
  final Future<String?> Function()? pickWavPath;

  /// Returns the path to save a project to, or null to cancel.
  final Future<String?> Function()? pickProjectSavePath;

  /// Returns the path of a project to open, or null to cancel.
  final Future<String?> Function()? pickProjectOpenPath;

  @override
  State<HomePage> createState() => _HomePageState();
}

/// One row of per-track UI state. Mirrors a [Track] (the project DTO) plus a
/// few transient bits the file doesn't need to remember: the live [ClipInfo]
/// for the waveform, a busy flag for the loader, a `missing` flag set when a
/// project referenced a clip path that couldn't be decoded.
///
/// `engineSlot` is the **stable** pool index this track uses. When a track is
/// removed, that slot goes back into the free pool and a future "add" reuses
/// it. Decoupling display order from engine slot means removing track 2 doesn't
/// force re-decoding the WAVs on tracks 3..N.
class _TrackUi {
  _TrackUi({
    required this.name,
    required this.engineSlot,
    this.clipPath,
    this.gainLinear = 1.0,
    this.pan = 0.0,
    List<EqBand>? eqBands,
  }) : eqBands = eqBands ?? List.of(kDefaultEqBands);

  String name;
  int engineSlot;
  String? clipPath;
  String? filename;
  ClipInfo? clip;
  double gainLinear;
  // Fader mode is a pure UI choice; resets to the default whenever a track is
  // created/loaded — there's nothing in the project file to restore.
  GainFaderMode gainMode = GainFaderMode.db6;
  double pan;
  List<EqBand> eqBands;
  bool busy = false;
  bool missing = false;
}

class _HomePageState extends State<HomePage> {
  /// User-visible tracks, in display order. May be empty (no tracks → silence).
  final List<_TrackUi> _tracks = [];

  /// Engine slots currently held by [_tracks]. Used to find a free slot for
  /// "Add Track" without scanning the list every time.
  final Set<int> _usedSlots = {};

  // Master bus state.
  double _masterGain = 1.0;
  GainFaderMode _masterGainMode = GainFaderMode.db6;
  double _masterPan = 0;

  // Global transport state. With one playhead per engine, every track plays
  // in sync — so a single set of fields is correct.
  double _positionSecs = 0;
  bool _playing = false;
  bool _looping = false;

  // The track whose EQ panel is currently being edited. -1 when no track is
  // selected (e.g. all tracks have been removed).
  int _selectedTrack = -1;

  // Set while a project load/save is in flight, so the AppBar action buttons
  // dim and don't queue duplicate dialogs.
  bool _projectBusy = false;

  /// Cached project name (from the last load/save), shown in the AppBar.
  String _projectName = 'Untitled';

  late final double _eqSampleRate = widget.engine.engineSampleRate;
  late final StreamSubscription<PlaybackState> _statusSub;
  late final Stream<Float32List> _scopeFrames = widget.engine.scopeFrames;

  late final Stream<MixerMeters> _mixerMeters =
      widget.engine.mixerMeters.asBroadcastStream();

  /// Per-engine-slot meter views. The mixer publishes a slot-indexed array so
  /// we key on slot, not on display index — that way removing track 0 doesn't
  /// drift the rest of the strips' meter sources by one.
  late final List<Stream<MeterLevels>> _slotMeters = List.generate(
    widget.engine.maxTracks,
    (slot) => _mixerMeters.map((m) => m.trackLevels(slot)).asBroadcastStream(),
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

  _TrackUi? get _selected {
    if (_selectedTrack < 0 || _selectedTrack >= _tracks.length) return null;
    return _tracks[_selectedTrack];
  }

  ClipInfo? get _selectedClip => _selected?.clip;
  double get _duration => _selectedClip?.durationSecs ?? 0;
  double get _positionFraction =>
      _duration > 0 ? (_positionSecs / _duration).clamp(0.0, 1.0) : 0.0;

  bool get _anyClipLoaded => _tracks.any((t) => t.clip != null);

  // --- Track add / remove --------------------------------------------------

  /// Lowest engine-pool index not currently in use, or null if the pool is
  /// full. Pool capacity is fixed at engine start, so this is cheap.
  int? _nextFreeSlot() {
    for (var i = 0; i < widget.engine.maxTracks; i++) {
      if (!_usedSlots.contains(i)) return i;
    }
    return null;
  }

  /// Create a new empty track at the end of the row. Disabled (the UI guards)
  /// when the engine pool is full.
  void _addTrack() {
    final slot = _nextFreeSlot();
    if (slot == null) return;
    setState(() {
      _usedSlots.add(slot);
      _tracks.add(
        _TrackUi(
          // "Track N" where N is one past the highest existing name we can
          // parse — sequential even after removes, which is the least
          // surprising default. The user can rename later (no rename UI yet).
          name: 'Track ${_nextTrackNumber()}',
          engineSlot: slot,
        ),
      );
      // Auto-select only the very first track. Subsequent adds preserve the
      // user's current EQ-panel focus, which avoids a surprise context switch
      // when you build up the row.
      if (_selectedTrack < 0) _selectedTrack = _tracks.length - 1;
    });
  }

  /// One past the largest "Track N" suffix already in use; falls back to
  /// `tracks.length + 1` for un-parseable names so adding always produces a
  /// fresh number.
  int _nextTrackNumber() {
    var highest = 0;
    final re = RegExp(r'^Track (\d+)$');
    for (final t in _tracks) {
      final m = re.firstMatch(t.name);
      if (m != null) {
        final n = int.tryParse(m.group(1)!) ?? 0;
        if (n > highest) highest = n;
      }
    }
    return highest >= _tracks.length ? highest + 1 : _tracks.length + 1;
  }

  /// Remove the track at display index [i]. Calls [clearTrack] on the engine
  /// so the slot resets to defaults — no allocation on the audio thread, the
  /// strip pool already exists at full size.
  void _removeTrack(int i) {
    if (i < 0 || i >= _tracks.length) return;
    final removed = _tracks[i];
    widget.engine.clearTrack(removed.engineSlot);
    setState(() {
      _tracks.removeAt(i);
      _usedSlots.remove(removed.engineSlot);
      // Keep _selectedTrack in range — slide it down by one if a track before
      // it was removed; clamp to the new last index otherwise.
      if (_tracks.isEmpty) {
        _selectedTrack = -1;
      } else {
        if (i < _selectedTrack) {
          _selectedTrack -= 1;
        }
        _selectedTrack = _selectedTrack.clamp(0, _tracks.length - 1);
      }
      // If the removed track had nothing playable left, reflect that in the
      // global transport state so the buttons grey out.
      if (!_anyClipLoaded) _playing = false;
    });
  }

  // --- Transport ----------------------------------------------------------

  Future<void> _openWavFor(int i) async {
    if (i < 0 || i >= _tracks.length) return;
    final track = _tracks[i];
    if (track.busy) return;
    final pick = widget.pickWavPath ?? pickWavWithDialog;
    final path = await pick();
    if (path == null) return; // cancelled
    await _loadClipInto(i, path);
  }

  /// Decode and assign a WAV at [path] into track at display index [i].
  /// Surface decode failures as a `missing` flag + a SnackBar without aborting
  /// — used both by the user-initiated "Open WAV…" and by [_loadProject].
  Future<void> _loadClipInto(int i, String path) async {
    final track = _tracks[i];
    setState(() => track.busy = true);
    try {
      final clip = await widget.engine.loadWav(track.engineSlot, path);
      setState(() {
        track.clip = clip;
        track.clipPath = path;
        track.filename = _basename(path);
        track.missing = false;
        _positionSecs = 0;
        _playing = false;
      });
    } catch (e) {
      setState(() {
        // Keep the path so the user can see what was missing, but mark the
        // track as having no live clip and surface it visually.
        track.clipPath = path;
        track.filename = _basename(path);
        track.missing = true;
        track.clip = null;
      });
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Could not load ${_basename(path)}: $e')),
        );
      }
    } finally {
      if (mounted) setState(() => track.busy = false);
    }
  }

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
    if (_selected?.clip == null) return;
    final secs = fraction * _duration;
    setState(() => _positionSecs = secs);
    widget.engine.seek(secs);
  }

  // --- Per-track mixer callbacks ------------------------------------------

  void _onTrackGainChanged(int i, double linear) {
    final t = _tracks[i];
    setState(() => t.gainLinear = linear);
    widget.engine.setTrackGainLinear(t.engineSlot, linear);
  }

  void _onTrackGainModeChanged(int i, GainFaderMode mode) {
    setState(() => _tracks[i].gainMode = mode);
  }

  void _onTrackPanChanged(int i, double pan) {
    final t = _tracks[i];
    setState(() => t.pan = pan);
    widget.engine.setTrackPan(t.engineSlot, pan);
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

  void _updateSelectedBand(int band, EqBand updated) {
    final t = _selected;
    if (t == null) return;
    setState(() {
      t.eqBands = [...t.eqBands]..[band] = updated;
    });
  }

  void _onEqFreq(int band, double hz) {
    final t = _selected;
    if (t == null) return;
    _updateSelectedBand(band, t.eqBands[band].copyWith(freqHz: hz));
    widget.engine.setTrackEqBandFreq(t.engineSlot, band, hz);
  }

  void _onEqGain(int band, double db) {
    final t = _selected;
    if (t == null) return;
    _updateSelectedBand(band, t.eqBands[band].copyWith(gainDb: db));
    widget.engine.setTrackEqBandGainDb(t.engineSlot, band, db);
  }

  void _onEqQ(int band, double q) {
    final t = _selected;
    if (t == null) return;
    _updateSelectedBand(band, t.eqBands[band].copyWith(q: q));
    widget.engine.setTrackEqBandQ(t.engineSlot, band, q);
  }

  void _onEqKind(int band, EqFilterKind kind) {
    final t = _selected;
    if (t == null) return;
    _updateSelectedBand(band, t.eqBands[band].copyWith(kind: kind));
    widget.engine.setTrackEqBandKind(t.engineSlot, band, kind);
  }

  void _onEqEnabled(int band, bool on) {
    final t = _selected;
    if (t == null) return;
    _updateSelectedBand(band, t.eqBands[band].copyWith(enabled: on));
    widget.engine.setTrackEqBandEnabled(t.engineSlot, band, on);
  }

  // --- Save / Load --------------------------------------------------------

  /// Build a [Project] snapshot from the current UI state.
  Project _snapshotProject() => Project(
    name: _projectName,
    tracks: [
      for (final t in _tracks)
        Track(
          name: t.name,
          clipPath: t.clipPath,
          gainDb: _linearToDb(t.gainLinear),
          pan: t.pan,
          eqBands: t.eqBands,
        ),
    ],
    master: MasterBus(
      gainDb: _linearToDb(_masterGain),
      pan: _masterPan,
      eqBands: const [], // master EQ isn't UI-exposed yet — empty in v1
    ),
  );

  Future<void> _saveProject() async {
    if (_projectBusy) return;
    final pick = widget.pickProjectSavePath ?? pickProjectSavePathWithDialog;
    final path = await pick();
    if (path == null) return;
    setState(() => _projectBusy = true);
    try {
      await widget.engine.saveProject(path, _snapshotProject());
      if (!mounted) return;
      setState(() => _projectName = _basenameNoExt(path));
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Saved to ${_basename(path)}')),
      );
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not save: $e')),
      );
    } finally {
      if (mounted) setState(() => _projectBusy = false);
    }
  }

  Future<void> _loadProject() async {
    if (_projectBusy) return;
    final pick = widget.pickProjectOpenPath ?? pickProjectOpenPathWithDialog;
    final path = await pick();
    if (path == null) return;

    setState(() => _projectBusy = true);
    try {
      final p = await widget.engine.loadProject(path);
      await _applyProject(p, sourcePath: path);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Could not load: $e')),
      );
    } finally {
      if (mounted) setState(() => _projectBusy = false);
    }
  }

  /// Replace the current UI + engine state with [p]. Engine slots get
  /// re-assigned 0..N-1 in display order. WAV loads are best-effort: a missing
  /// file marks just its track as `missing` rather than aborting the load.
  Future<void> _applyProject(Project p, {String? sourcePath}) async {
    // 1. Clear every currently-used slot on the engine. Cheap; just a command
    //    push per slot.
    for (final t in _tracks) {
      widget.engine.clearTrack(t.engineSlot);
    }

    // 2. Build the new track list and reserve fresh slots 0..N-1.
    final maxSlots = widget.engine.maxTracks;
    final newTracks = <_TrackUi>[];
    for (var i = 0; i < p.tracks.length && i < maxSlots; i++) {
      final src = p.tracks[i];
      newTracks.add(
        _TrackUi(
          name: src.name,
          engineSlot: i,
          clipPath: src.clipPath,
          gainLinear: _dbToLinear(src.gainDb),
          pan: src.pan,
          eqBands: src.eqBands.isEmpty ? List.of(kDefaultEqBands) : src.eqBands,
        ),
      );
    }
    setState(() {
      _tracks
        ..clear()
        ..addAll(newTracks);
      _usedSlots
        ..clear()
        ..addAll([for (final t in newTracks) t.engineSlot]);
      _masterGain = _dbToLinear(p.master.gainDb);
      _masterPan = p.master.pan;
      _projectName = p.name.isEmpty
          ? (sourcePath == null ? 'Untitled' : _basenameNoExt(sourcePath))
          : p.name;
      _selectedTrack = _tracks.isEmpty ? -1 : 0;
      _positionSecs = 0;
      _playing = false;
    });

    // 3. Push the per-track parameters to the engine so the audio side
    //    matches what the UI just rendered.
    for (final t in _tracks) {
      widget.engine.setTrackGainLinear(t.engineSlot, t.gainLinear);
      widget.engine.setTrackPan(t.engineSlot, t.pan);
      for (var b = 0; b < t.eqBands.length; b++) {
        final band = t.eqBands[b];
        widget.engine.setTrackEqBandKind(t.engineSlot, b, band.kind);
        widget.engine.setTrackEqBandFreq(t.engineSlot, b, band.freqHz);
        widget.engine.setTrackEqBandQ(t.engineSlot, b, band.q);
        widget.engine.setTrackEqBandGainDb(t.engineSlot, b, band.gainDb);
        widget.engine.setTrackEqBandEnabled(t.engineSlot, b, band.enabled);
      }
    }
    widget.engine.setMasterGainLinear(_masterGain);
    widget.engine.setMasterPan(_masterPan);

    // 4. Best-effort WAV loads. Sequential so progress is visible; per-file
    //    failures stay on the per-track row, not on the whole project.
    for (var i = 0; i < _tracks.length; i++) {
      final cp = _tracks[i].clipPath;
      if (cp != null && cp.isNotEmpty) {
        await _loadClipInto(i, cp);
      }
    }
  }

  // --- Build --------------------------------------------------------------

  @override
  Widget build(BuildContext context) {
    final maxOut = widget.engine.maxTracks;
    final canAdd = _tracks.length < maxOut;
    return Scaffold(
      appBar: AppBar(
        title: Text('DAW — Milestone 5  •  $_projectName'),
        actions: [
          IconButton(
            tooltip: 'Open project…',
            onPressed: _projectBusy ? null : _loadProject,
            icon: const Icon(Icons.folder_open),
          ),
          IconButton(
            tooltip: 'Save project…',
            onPressed: _projectBusy ? null : _saveProject,
            icon: const Icon(Icons.save),
          ),
          const SizedBox(width: 8),
        ],
      ),
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
                  // Per-track "Open WAV" rows.
                  if (_tracks.isEmpty)
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 12),
                      child: Text(
                        'No tracks yet. Add one below to get started.',
                        style: Theme.of(context).textTheme.bodyMedium,
                      ),
                    ),
                  for (int i = 0; i < _tracks.length; i++) ...[
                    _OpenWavRow(
                      label: _tracks[i].name,
                      filename: _tracks[i].filename,
                      missing: _tracks[i].missing,
                      busy: _tracks[i].busy,
                      onOpen: () => _openWavFor(i),
                    ),
                    const SizedBox(height: 8),
                  ],
                  const SizedBox(height: 8),

                  // Waveform of the currently-selected track.
                  if (_selectedClip != null) ...[
                    WaveformView(
                      min: _selectedClip!.waveformMin,
                      max: _selectedClip!.waveformMax,
                      positionFraction: _positionFraction,
                      onSeek: _onSeek,
                    ),
                    const SizedBox(height: 4),
                    Text(
                      '${_selected!.name}  •  '
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

                  // Mixer row: dynamic track strips, an "Add Track" tile, and
                  // the master strip on the right. Horizontal-scrollable so it
                  // stays usable past about six tracks on narrow windows.
                  SingleChildScrollView(
                    scrollDirection: Axis.horizontal,
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        for (int i = 0; i < _tracks.length; i++) ...[
                          _StripWithRemove(
                            onRemove: () => _removeTrack(i),
                            child: ChannelStrip(
                              title: _tracks[i].name,
                              selected: _selectedTrack == i,
                              onTap: () =>
                                  setState(() => _selectedTrack = i),
                              gainLinear: _tracks[i].gainLinear,
                              gainMode: _tracks[i].gainMode,
                              pan: _tracks[i].pan,
                              meter: _slotMeters[_tracks[i].engineSlot],
                              onGainLinearChanged: (lin) =>
                                  _onTrackGainChanged(i, lin),
                              onGainModeChanged: (m) =>
                                  _onTrackGainModeChanged(i, m),
                              onPanChanged: (p) => _onTrackPanChanged(i, p),
                            ),
                          ),
                          const SizedBox(width: 12),
                        ],
                        _AddTrackTile(
                          enabled: canAdd,
                          tooltip: canAdd
                              ? 'Add track'
                              : 'Pool is full ($maxOut max)',
                          onTap: _addTrack,
                        ),
                        const SizedBox(width: 12),
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

                  // EQ panel — edits whichever track strip is selected. Only
                  // shown when there's actually a track to edit.
                  if (_selected != null) ...[
                    Text(
                      'EQ — ${_selected!.name}',
                      style: Theme.of(context).textTheme.labelMedium,
                    ),
                    const SizedBox(height: 8),
                    EqView(
                      bands: _selected!.eqBands,
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
    required this.missing,
    required this.onOpen,
  });

  final String label;
  final String? filename;
  final bool busy;
  final bool missing;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final filenameText = filename == null
        ? 'No file loaded'
        : (missing ? '$filename (missing)' : filename!);
    return Row(
      children: [
        SizedBox(
          width: 80,
          child: Text(label, style: theme.textTheme.labelMedium),
        ),
        FilledButton.icon(
          onPressed: busy ? null : onOpen,
          icon: const Icon(Icons.folder_open),
          label: const Text('Open WAV…'),
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Text(
            filenameText,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodyMedium?.copyWith(
              color: missing ? theme.colorScheme.error : null,
            ),
          ),
        ),
      ],
    );
  }
}

/// A channel strip with a small "remove" button floating above it. The button
/// sits outside the strip's own widget so the [ChannelStrip] stays dumb.
class _StripWithRemove extends StatelessWidget {
  const _StripWithRemove({required this.child, required this.onRemove});

  final Widget child;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        SizedBox(
          height: 28,
          child: Align(
            alignment: Alignment.centerRight,
            child: IconButton(
              tooltip: 'Remove track',
              onPressed: onRemove,
              iconSize: 18,
              padding: EdgeInsets.zero,
              constraints: const BoxConstraints(),
              icon: const Icon(Icons.close),
            ),
          ),
        ),
        child,
      ],
    );
  }
}

/// The plus-sign tile that appears after the last track. Same vertical
/// footprint as a [ChannelStrip] so the row stays aligned.
class _AddTrackTile extends StatelessWidget {
  const _AddTrackTile({
    required this.enabled,
    required this.tooltip,
    required this.onTap,
  });

  final bool enabled;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colour = enabled
        ? theme.colorScheme.primary
        : theme.disabledColor;
    return Column(
      children: [
        const SizedBox(height: 28), // align with _StripWithRemove
        Tooltip(
          message: tooltip,
          child: Material(
            color: Colors.transparent,
            child: InkWell(
              borderRadius: BorderRadius.circular(12),
              onTap: enabled ? onTap : null,
              child: Container(
                width: 80,
                height: 410, // same height as a ChannelStrip body
                decoration: BoxDecoration(
                  color: const Color(0xFF15151B),
                  borderRadius: BorderRadius.circular(12),
                  border: Border.all(
                    color: colour.withValues(alpha: 0.4),
                    width: 1,
                  ),
                ),
                child: Center(
                  child: Icon(Icons.add, color: colour, size: 32),
                ),
              ),
            ),
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

/// Last path segment of [path], handling both `/` and `\` so we don't pull
/// in `dart:io` just to show a filename.
String _basename(String path) {
  final cut = path.lastIndexOf(RegExp(r'[/\\]'));
  return cut < 0 ? path : path.substring(cut + 1);
}

/// [_basename] minus the final `.ext`, if any. Used to derive a project name
/// from a saved/loaded file path.
String _basenameNoExt(String path) {
  final base = _basename(path);
  final dot = base.lastIndexOf('.');
  return dot <= 0 ? base : base.substring(0, dot);
}

/// Linear-to-dB helper for converting [Project] file values to fader linear.
/// `kGainFloorDb` (channel_strip.dart) is the mute floor — anything at or below
/// it maps to a true 0.0 linear, matching the engine's hard-mute behavior.
double _linearToDb(double linear) {
  if (linear <= 0) return kGainFloorDb;
  return 20 * (math.log(linear) / math.ln10);
}

double _dbToLinear(double db) {
  if (db <= kGainFloorDb) return 0;
  return math.pow(10, db / 20).toDouble();
}
