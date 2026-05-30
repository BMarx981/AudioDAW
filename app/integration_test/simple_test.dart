import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'package:daw/engine/rust_engine.dart';
import 'package:daw/src/rust/frb_generated.dart';
import 'package:daw/ui/home_page.dart';

// End-to-end smoke test: real Flutter + real bridge + real Rust engine + real
// audio device. Generate a tiny WAV, open it through the UI (which decodes it in
// Rust and opens the device), play it, and confirm the engine reports a running
// stream — exercising the whole Dart -> Rust -> cpal path.
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await RustLib.init());

  testWidgets('open + play streams a real WAV through the engine', (
    tester,
  ) async {
    // A 0.25 s, 48 kHz mono sine, written to a temp WAV the picker will return.
    final wavPath = '${Directory.systemTemp.path}/daw_e2e_test.wav';
    _writeSineWav(wavPath, sampleRate: 48000, seconds: 0.25, hz: 440);
    addTearDown(() {
      final f = File(wavPath);
      if (f.existsSync()) f.deleteSync();
    });

    final engine = RustEngine();
    await tester.pumpWidget(
      MaterialApp(
        home: HomePage(engine: engine, pickWavPath: () async => wavPath),
      ),
    );

    expect(engine.isRunning, isFalse);

    // Open the file: decodes in Rust and starts the audio device.
    await tester.tap(find.text('Open WAV…'));
    await tester.pumpAndSettle();
    expect(
      engine.isRunning,
      isTrue,
      reason: 'loading a WAV should start the audio stream',
    );

    // Play it, let a few buffers run, then stop.
    await tester.tap(find.text('Play'));
    await tester.pump(const Duration(milliseconds: 150));
    expect(find.text('Pause'), findsOneWidget);

    await tester.tap(find.text('Stop'));
    await tester.pumpAndSettle();
    expect(find.text('Play'), findsOneWidget);
  });
}

/// Write a mono 16-bit PCM WAV of a sine tone.
void _writeSineWav(
  String path, {
  required int sampleRate,
  required double seconds,
  required double hz,
}) {
  final frames = (sampleRate * seconds).round();
  const bytesPerSample = 2;
  final dataLen = frames * bytesPerSample;

  final b = BytesBuilder();
  void str(String s) => b.add(s.codeUnits);
  void u32(int v) =>
      b.add(Uint8List(4)..buffer.asByteData().setUint32(0, v, Endian.little));
  void u16(int v) =>
      b.add(Uint8List(2)..buffer.asByteData().setUint16(0, v, Endian.little));

  str('RIFF');
  u32(36 + dataLen);
  str('WAVE');
  str('fmt ');
  u32(16);
  u16(1); // PCM
  u16(1); // mono
  u32(sampleRate);
  u32(sampleRate * bytesPerSample); // byte rate
  u16(bytesPerSample); // block align
  u16(16); // bits per sample
  str('data');
  u32(dataLen);

  final samples = Uint8List(dataLen);
  final view = samples.buffer.asByteData();
  for (var i = 0; i < frames; i++) {
    final v = (math.sin(2 * math.pi * hz * i / sampleRate) * 30000).round();
    view.setInt16(i * 2, v, Endian.little);
  }
  b.add(samples);

  File(path).writeAsBytesSync(b.toBytes());
}
