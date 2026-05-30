import 'dart:async';
import 'dart:typed_data';

import 'package:daw/engine/engine_interface.dart';

/// Test double for [EngineInterface]. Records calls so tests can assert on them,
/// and never touches Rust. This is the seam TESTING.md describes — widget tests
/// run entirely against this.
class FakeEngine implements EngineInterface {
  final List<double> frequencies = [];
  int startCount = 0;
  int stopCount = 0;
  bool _running = false;

  /// If set, [start] throws this — used to test the error path.
  Object? startError;

  @override
  Future<void> start() async {
    if (startError != null) throw startError!;
    startCount++;
    _running = true;
  }

  @override
  Future<void> stop() async {
    stopCount++;
    _running = false;
  }

  @override
  void setFrequency(double hz) => frequencies.add(hz);

  @override
  bool get isRunning => _running;

  /// Scope frames are driven manually in tests via [emitScopeFrame]. Defaults to
  /// no events (a flat scope) so widgets settle under `pumpAndSettle` — we never
  /// want a free-running periodic stream in a widget test.
  final StreamController<Float32List> _scope =
      StreamController<Float32List>.broadcast();

  @override
  Stream<Float32List> get scopeFrames => _scope.stream;

  /// Push one frame to any [scopeFrames] listeners.
  void emitScopeFrame(Float32List frame) => _scope.add(frame);

  /// The most recent frequency pushed, or null if none.
  double? get lastFrequency => frequencies.isEmpty ? null : frequencies.last;

  void dispose() => _scope.close();
}
