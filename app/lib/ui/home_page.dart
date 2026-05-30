import 'package:flutter/material.dart';

import '../engine/engine_interface.dart';
import 'frequency_mapping.dart';

/// The entire Milestone 0 UI: a play/stop button and a frequency slider.
///
/// The widget stays dumb — it owns only view state (slider position, playing
/// flag) and forwards intent to the injected [EngineInterface]. It never
/// imports the Rust bridge, which is what lets the widget test drive it with a
/// fake engine.
class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.engine});

  final EngineInterface engine;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  // Start at A4 (440 Hz), placed on the log slider.
  double _slider = hzToSlider(440.0);
  bool _playing = false;
  bool _busy = false; // guards against double taps while start/stop awaits

  double get _hz => sliderToHz(_slider);

  Future<void> _togglePlay() async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      if (_playing) {
        await widget.engine.stop();
      } else {
        await widget.engine.start();
        // Push the current slider frequency immediately so playback starts at
        // what the UI shows, not the engine's default.
        widget.engine.setFrequency(_hz);
      }
      setState(() => _playing = !_playing);
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text('Audio engine error: $e')),
        );
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _onSliderChanged(double value) {
    setState(() => _slider = value);
    // Fire-and-forget: drive the engine on every tick. The Rust side ramps to
    // this frequency, so dragging produces a smooth glide, not a click.
    widget.engine.setFrequency(_hz);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Sine + Slider — Milestone 0')),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Text(
                  '${_hz.toStringAsFixed(1)} Hz',
                  style: Theme.of(context).textTheme.displaySmall,
                ),
                const SizedBox(height: 24),
                Slider(
                  value: _slider,
                  onChanged: _onSliderChanged,
                  label: '${_hz.toStringAsFixed(0)} Hz',
                ),
                const SizedBox(height: 24),
                FilledButton.icon(
                  onPressed: _busy ? null : _togglePlay,
                  icon: Icon(_playing ? Icons.stop : Icons.play_arrow),
                  label: Text(_playing ? 'Stop' : 'Play'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
