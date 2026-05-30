import 'dart:math' as math;

/// Logarithmic mapping between the slider's 0..1 position and frequency in Hz.
///
/// Pitch is perceived logarithmically, so a linear slider should map to an
/// exponential frequency curve — equal slider distances give equal *musical*
/// intervals. `20 * 100^value` gives 20 Hz at one end and 2000 Hz at the other
/// (two decades). Kept as pure top-level functions so they're trivially testable
/// independent of any widget.
const double kMinHz = 20.0;
const double kMaxHz = 2000.0;
const double _decades = kMaxHz / kMinHz; // 100

/// Slider position (0..1) -> frequency in Hz.
double sliderToHz(double value) => kMinHz * math.pow(_decades, value).toDouble();

/// Frequency in Hz -> slider position (0..1). Inverse of [sliderToHz], used to
/// place the slider thumb for an initial frequency.
double hzToSlider(double hz) =>
    math.log(hz / kMinHz) / math.log(_decades);
