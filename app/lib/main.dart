import 'package:flutter/material.dart';

import 'engine/engine_interface.dart';
import 'engine/rust_engine.dart';
import 'src/rust/frb_generated.dart';
import 'ui/home_page.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  // Initialize the Rust bridge (loads the dylib, runs the #[frb(init)] hook).
  await RustLib.init();
  runApp(DawApp(engine: RustEngine()));
}

class DawApp extends StatelessWidget {
  const DawApp({super.key, required this.engine});

  final EngineInterface engine;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'DAW — Sine Starter',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.deepPurple),
        useMaterial3: true,
      ),
      home: HomePage(engine: engine),
    );
  }
}
