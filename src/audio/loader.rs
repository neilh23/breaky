use anyhow::{Context, Result};
use std::fs::File;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::buffer::AudioBuffer;

/// Load an audio file and decode it to a mono f32 AudioBuffer.
pub fn load_audio(path: &Path) -> Result<AudioBuffer> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

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
        .context("Unsupported or invalid audio file")?;

    let mut format = probed.format;

    let track = format
        .default_track()
        .context("No audio track found")?
        .clone();

    let sample_rate = track
        .codec_params
        .sample_rate
        .context("Unknown sample rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("Failed to create decoder")?;

    let mut all_samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e).context("Error reading packet"),
        };

        if packet.track_id() != track.id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("Decode error"),
        };

        let spec = *decoded.spec();
        let num_frames = decoded.capacity();

        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        let interleaved = sample_buf.samples();

        // Mix down to mono
        if channels == 1 {
            all_samples.extend_from_slice(interleaved);
        } else {
            for frame in interleaved.chunks(channels) {
                let mono: f32 = frame.iter().sum::<f32>() / channels as f32;
                all_samples.push(mono);
            }
        }
    }

    if all_samples.is_empty() {
        anyhow::bail!("No audio samples decoded from file");
    }

    Ok(AudioBuffer {
        samples: all_samples,
        sample_rate,
        file_name,
    })
}
