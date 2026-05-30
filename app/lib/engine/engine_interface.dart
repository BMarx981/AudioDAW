/// The seam between the UI and the audio engine.
///
/// Per TESTING.md the UI never talks to the Rust bridge directly — it depends on
/// this interface. Production wires in [RustEngine] (bridge-backed); widget tests
/// wire in a `FakeEngine` that records calls. Swapping implementations is also how
/// we'll later drop in an offline-render engine for export.
abstract class EngineInterface {
  /// Open the audio device and start the sine tone. Throws if no device is
  /// available.
  Future<void> start();

  /// Stop playback and release the device.
  Future<void> stop();

  /// Set the oscillator frequency. Fire-and-forget: safe to call on every slider
  /// tick. No-op if the engine isn't running.
  void setFrequency(double hz);

  /// Whether the engine is currently playing.
  bool get isRunning;
}
