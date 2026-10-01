//! Voice messages in a form QQ plays.
//!
//! QQ's open API plays voice (`file_type = 3`) uploaded as SILK, WAV or MP3; Tencent's own SDK
//! uploads those three unchanged. Anything else — FLAC, Ogg Vorbis, AAC/M4A, ALAC, AIFF, CAF — is
//! decoded here in pure Rust and re-encoded as 24 kHz mono 16-bit WAV, so the node needs neither
//! ffmpeg nor a SILK library at runtime. Opus and AMR have no decoder here; they fail with a
//! reason that names what is accepted instead of reaching QQ and failing there without one.

use std::f64::consts::PI;
use std::io::Cursor;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::api::MAX_UPLOAD_BYTES;

/// Sample rate of converted voice: the highest rate SILK, the codec QQ plays voice with, keeps.
pub const VOICE_RATE: u32 = 24_000;

/// Zero crossings of the resampling kernel on each side of its center.
///
/// Sixteen keeps the low-pass transition narrow enough that speech energy just below the new
/// Nyquist frequency survives while what lies above it is cut instead of aliasing back down.
const KERNEL_ZEROS: f64 = 16.0;

/// Size of the canonical WAV header [`wav`] writes.
const WAV_HEADER_BYTES: usize = 44;

/// Returns `audio` in a form QQ plays as a voice message.
///
/// SILK, WAV and MP3 come back unchanged. Anything else is decoded, mixed down to mono, resampled
/// to [`VOICE_RATE`] and returned as WAV. The error is a reason fit for a delivery failure.
///
/// Decoding is CPU-bound; call this from a blocking thread, not from the async runtime.
pub fn playable(audio: Vec<u8>) -> Result<Vec<u8>, String> {
    if is_silk(&audio) || is_wav(&audio) || is_mp3(&audio) {
        return Ok(audio);
    }
    let (samples, rate) = decode_mono(audio)?;
    Ok(wav(&resample(&samples, rate, VOICE_RATE), VOICE_RATE))
}

/// SILK as QQ writes it: the `#!SILK_V3` header, with or without Tencent's leading `0x02`.
fn is_silk(audio: &[u8]) -> bool {
    audio
        .strip_prefix(&[0x02])
        .unwrap_or(audio)
        .starts_with(b"#!SILK_V3")
}

/// A RIFF container of WAVE audio.
fn is_wav(audio: &[u8]) -> bool {
    audio.len() >= 12 && &audio[..4] == b"RIFF" && &audio[8..12] == b"WAVE"
}

/// An MPEG Layer III stream, optionally behind an ID3v2 tag.
///
/// The frame header is checked rather than the tag alone: an ID3 tag also fronts AAC and FLAC
/// files now and then, and QQ would reject those as MP3.
fn is_mp3(audio: &[u8]) -> bool {
    let start = match audio {
        // ID3v2: a 10-byte header whose size is a 28-bit synchsafe integer, plus a 10-byte
        // footer when flag bit 4 is set.
        [b'I', b'D', b'3', _, _, flags, size @ ..] if size.len() >= 4 => {
            let body = size[..4]
                .iter()
                .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
            10 + body + if flags & 0x10 != 0 { 10 } else { 0 }
        }
        _ => 0,
    };
    // Eleven set sync bits, then layer bits `01`: Layer III. Layer `00` is AAC's ADTS header.
    matches!(
        audio.get(start..start + 2),
        Some(&[0xFF, second]) if second & 0xE0 == 0xE0 && second & 0x06 == 0x02
    )
}

/// Decodes the default audio track into mono samples and returns them with their sample rate.
///
/// Channels are averaged as each packet decodes, so memory holds one channel of the source
/// rather than all of them. Decoding stops with an error once the audio is longer than one QQ
/// upload holds after conversion, instead of buffering a whole album first.
fn decode_mono(audio: Vec<u8>) -> Result<(Vec<f32>, u32), String> {
    let unsupported = |err: symphonia::core::errors::Error| {
        format!(
            "voice audio is in a format Kanon cannot convert ({err}); QQ plays WAV, MP3 and \
             SILK, and Kanon converts FLAC, Ogg Vorbis, AAC/M4A, ALAC, AIFF and CAF"
        )
    };
    let stream = MediaSourceStream::new(Box::new(Cursor::new(audio)), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(unsupported)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("voice file has no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(CodecParameters::audio)
        .ok_or("voice file's audio track has no codec parameters")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(unsupported)?;

    // Converted voice is 16-bit mono WAV, so this many output samples fill one upload.
    let max_output = ((MAX_UPLOAD_BYTES - WAV_HEADER_BYTES) / 2) as u64;
    let mut mono = Vec::new();
    let mut rate = None;
    let mut interleaved: Vec<f32> = Vec::new();
    while let Some(packet) = format
        .next_packet()
        .map_err(|err| format!("voice file is damaged: {err}"))?
    {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|err| format!("voice file is damaged: {err}"))?;
        let spec_rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count().max(1);
        match rate {
            None => rate = Some(spec_rate),
            Some(rate) if rate != spec_rate => {
                return Err("voice file changes its sample rate midway".into());
            }
            Some(_) => {}
        }
        decoded.copy_to_vec_interleaved::<f32>(&mut interleaved);
        mono.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        if mono.len() as u64 * u64::from(VOICE_RATE) > max_output * u64::from(spec_rate) {
            return Err(format!(
                "voice is longer than the {} seconds one QQ upload holds",
                max_output / u64::from(VOICE_RATE)
            ));
        }
    }
    match rate {
        Some(rate) if !mono.is_empty() => Ok((mono, rate)),
        _ => Err("voice file contains no audio".into()),
    }
}

/// Resamples mono audio from `from` Hz to `to` Hz and converts it to 16-bit samples.
///
/// Each output sample is a Hann-windowed sinc interpolation of its input neighborhood. When
/// downsampling, the sinc's cutoff drops to the new Nyquist frequency, so a 48 kHz recording
/// folded into 24 kHz loses its top octave instead of aliasing it into the speech band.
///
/// The kernel depends only on where an output sample falls between two input samples, and that
/// fractional position repeats every `to / gcd(from, to)` outputs. The taps are therefore built
/// once per phase, which keeps the inner loop to multiply-adds.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<i16> {
    let divisor = gcd(from, to);
    // Input advances by `step / phases` samples per output sample.
    let step = u64::from(from / divisor);
    let phases = u64::from(to / divisor);
    let cutoff = (f64::from(to) / f64::from(from)).min(1.0);
    // A lower cutoff stretches the sinc, so the window widens to keep the same zero crossings.
    let half = (KERNEL_ZEROS / cutoff).ceil() as usize;
    let width = 2 * half;

    // Tap `j` of a phase weighs the input sample `j + 1 - half` places after the one at or
    // before the output position; `x` is that sample's distance from the position.
    let mut kernel = Vec::with_capacity(phases as usize * width);
    for phase in 0..phases {
        let fraction = phase as f64 / phases as f64;
        let taps: Vec<f64> = (0..width)
            .map(|j| {
                let x = fraction + half as f64 - 1.0 - j as f64;
                let window = 0.5 + 0.5 * (PI * x / half as f64).cos();
                cutoff * sinc(cutoff * x) * window
            })
            .collect();
        // Normalizing each phase to unit gain keeps a constant signal constant: without it the
        // truncated kernel's gain wobbles slightly from phase to phase, which is audible as a
        // faint tone at the phase rate.
        let gain: f64 = taps.iter().sum();
        kernel.extend(taps.iter().map(|tap| (tap / gain) as f32));
    }

    let output_len = input.len() as u64 * phases / step;
    (0..output_len)
        .map(|n| {
            let position = n * step;
            let base = (position / phases) as usize;
            let phase = (position % phases) as usize;
            let taps = &kernel[phase * width..(phase + 1) * width];
            // Tap `j` reads input `base + 1 + j - half`. Samples before the start and past the
            // end count as silence: the leading taps are skipped and the zip stops at the end.
            let skipped = half.saturating_sub(base + 1);
            let first = base + 1 + skipped - half;
            let mut acc = 0.0f32;
            for (tap, sample) in taps[skipped..].iter().zip(input.iter().skip(first)) {
                acc += tap * sample;
            }
            (acc.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
        })
        .collect()
}

/// The normalized sinc function, `sin(πx) / πx`.
fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Greatest common divisor, for reducing the resampling ratio.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Wraps 16-bit mono samples in a canonical PCM WAV file.
fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(WAV_HEADER_BYTES + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // integer PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // one channel
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // bytes per frame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}
