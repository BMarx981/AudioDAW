import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'package:daw/engine/rust_engine.dart';
import 'package:daw/src/rust/frb_generated.dart';
import 'package:daw/ui/home_page.dart';

// End-to-end smoke test: real Flutter + real bridge + real Rust engine + real
// audio device. Launch the UI, press Play, and confirm the engine actually
// reports a running stream — exercising the whole Dart -> Rust -> cpal path.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await RustLib.init());

  testWidgets('Play starts a real audio stream', (tester) async {
    final engine = RustEngine();
    await tester.pumpWidget(MaterialApp(home: HomePage(engine: engine)));

    expect(engine.isRunning, isFalse);

    await tester.tap(find.text('Play'));
    await tester.pumpAndSettle();

    // The Rust side reports the stream is live, and the UI flipped to Stop.
    expect(engine.isRunning, isTrue, reason: 'engine should report a running stream');
    expect(find.text('Stop'), findsOneWidget);

    // Drive a couple of frequencies to make sure the control path doesn't throw.
    engine.setFrequency(220.0);
    engine.setFrequency(880.0);
    await tester.pump(const Duration(milliseconds: 100));

    // Clean up the device.
    await tester.tap(find.text('Stop'));
    await tester.pumpAndSettle();
    expect(engine.isRunning, isFalse);
  });
}
