//! WAV decoding via symphonia, producing an [`AudioClip`].
//!
//! **This runs on the control thread, never the audio thread.** It opens a file,
//! allocates growing buffers, and does plenty of work that would be illegal in
//! the realtime callback — and that's fine, because by the time the audio thread
//! sees the result it's a finished, immutable [`AudioClip`] behind an `Arc`.
//!
//! symphonia decodes into its own sample buffers; we convert to the engine's
//! internal format (planar `f32`) here, at the I/O boundary, exactly once.

use std::fs::File;
use std::io::ErrorKind;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::clip::AudioClip;

/// Decode a WAV file at `path` into an [`AudioClip`] of planar `f32` samples.
///
/// Returns a human-readable error string on any failure (missing file, not a
/// WAV, unsupported codec, …) — these surface to Dart as exceptions, so the
/// message matters.
pub fn decode_wav(path: &Path) -> Result<AudioClip, String> {
    let file = File::open(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Hint the prober with the extension so it can pick the WAV reader quickly.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("not a recognized audio file: {e}"))?;

    let mut format = probed.format;

    // Pick the first real audio track (skip any null/metadata tracks).
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "file contains no decodable audio track".to_string())?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("no decoder for this audio: {e}"))?;

    // Filled lazily once we see the first decoded packet and learn the real
    // channel count / sample rate / spec.
    let mut sample_rate: u32 = 0;
    let mut channels: usize = 0;
    let mut planar: Vec<Vec<f32>> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // The clean end-of-stream signal symphonia gives is an IO EOF.
            Err(SymphoniaError::IoError(e)) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => {
                return Err("stream changed mid-file (unsupported)".to_string());
            }
            Err(e) => return Err(format!("error reading audio packet: {e}")),
        };

        // Packets for other tracks (rare in WAV) aren't ours to decode.
        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                // First packet: learn the format and allocate everything.
                if sample_buf.is_none() {
                    let spec = *decoded.spec();
                    channels = spec.channels.count();
                    sample_rate = spec.rate;
                    if channels == 0 {
                        return Err("audio reports zero channels".to_string());
                    }
                    planar = vec![Vec::new(); channels];
                    // Capacity = max frames per packet, so re-decoding into this
                    // buffer never reallocates inside the loop.
                    sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
                }

                if let Some(buf) = sample_buf.as_mut() {
                    // Convert this packet to interleaved f32, then deinterleave
                    // into our planar channels.
                    buf.copy_interleaved_ref(decoded);
                    let interleaved = buf.samples();
                    for frame in interleaved.chunks(channels) {
                        for (ch, &s) in frame.iter().enumerate() {
                            planar[ch].push(s);
                        }
                    }
                }
            }
            // A single corrupt packet shouldn't abort the whole file.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("error decoding audio: {e}")),
        }
    }

    if sample_buf.is_none() {
        return Err("file decoded to zero audio frames".to_string());
    }

    AudioClip::new(sample_rate as f32, planar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal 16-bit PCM WAV to a temp path and return that path.
    /// Generated in-test so we don't commit binary fixtures for the simplest
    /// cases; real fixture files live in `engine/fixtures/` for richer tests.
    fn write_pcm16_wav(
        path: &Path,
        sample_rate: u32,
        channels: u16,
        frames: &[Vec<i16>], // frames[frame][channel]
    ) {
        let bits_per_sample: u16 = 16;
        let block_align: u16 = channels * bits_per_sample / 8;
        let byte_rate: u32 = sample_rate * block_align as u32;
        let data_len: u32 = (frames.len() as u32) * block_align as u32;

        let mut f = File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();

        // fmt chunk
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits_per_sample.to_le_bytes()).unwrap();

        // data chunk
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for frame in frames {
            for &s in frame {
                f.write_all(&s.to_le_bytes()).unwrap();
            }
        }
        f.flush().unwrap();
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("daw_decode_test_{name}.wav"))
    }

    #[test]
    fn decodes_mono_pcm16() {
        // A short mono ramp at 44.1 kHz. Assert channel count, rate, frame count,
        // and that the i16 values map to the expected f32 range.
        let path = temp_path("mono");
        let frames: Vec<Vec<i16>> = vec![
            vec![0],
            vec![i16::MAX], // -> ~ +1.0
            vec![0],
            vec![i16::MIN], // -> -1.0
        ];
        write_pcm16_wav(&path, 44_100, 1, &frames);

        let clip = decode_wav(&path).expect("decode should succeed");
        assert_eq!(clip.channels, 1);
        assert_eq!(clip.frames, 4);
        assert!((clip.sample_rate - 44_100.0).abs() < 0.5);

        // i16::MAX maps to just under 1.0; i16::MIN maps to exactly -1.0.
        assert!(clip.data[0][0].abs() < 1e-6, "zero stays zero");
        assert!((clip.data[0][1] - 1.0).abs() < 1e-3, "full positive ~ +1.0");
        assert!((clip.data[0][3] + 1.0).abs() < 1e-3, "full negative ~ -1.0");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decodes_stereo_pcm16_deinterleaves_correctly() {
        // Distinct left/right values per frame so we can verify deinterleaving:
        // left channel is positive, right is negative.
        let path = temp_path("stereo");
        let frames: Vec<Vec<i16>> = vec![
            vec![10_000, -10_000],
            vec![20_000, -20_000],
            vec![30_000, -30_000],
        ];
        write_pcm16_wav(&path, 48_000, 2, &frames);

        let clip = decode_wav(&path).expect("decode should succeed");
        assert_eq!(clip.channels, 2);
        assert_eq!(clip.frames, 3);
        // Left channel all positive, right channel all negative.
        assert!(
            clip.data[0].iter().all(|&s| s > 0.0),
            "left should be positive"
        );
        assert!(
            clip.data[1].iter().all(|&s| s < 0.0),
            "right should be negative"
        );
        // Monotonic increase in magnitude matches the input ordering.
        assert!(clip.data[0][0] < clip.data[0][1] && clip.data[0][1] < clip.data[0][2]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_a_clean_error_not_a_panic() {
        let err = decode_wav(Path::new("/no/such/file_xyz.wav"));
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("could not open"));
    }

    #[test]
    fn non_wav_bytes_are_rejected() {
        let path = temp_path("garbage");
        {
            let mut f = File::create(&path).unwrap();
            f.write_all(b"this is definitely not a wav file").unwrap();
        }
        assert!(decode_wav(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
