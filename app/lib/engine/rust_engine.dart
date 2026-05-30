import '../src/rust/api/engine_api.dart' as rust;
import 'engine_interface.dart';

/// Production [EngineInterface] backed by the Rust engine over flutter_rust_bridge.
///
/// This is a thin wrapper — all the real work is in Rust. It exists so the UI
/// depends on [EngineInterface], not on generated bridge code, and so the
/// fire-and-forget vs. awaited distinction is expressed in idiomatic Dart.
class RustEngine implements EngineInterface {
  @override
  Future<void> start() => rust.startEngine();

  @override
  Future<void> stop() => rust.stopEngine();

  @override
  void setFrequency(double hz) => rust.setFrequency(hz: hz);

  @override
  bool get isRunning => rust.isRunning();
}
