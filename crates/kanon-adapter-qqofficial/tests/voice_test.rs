//! Voice reaches QQ in a format it plays: native formats untouched, everything else as WAV.

use kanon_adapter_qqofficial::voice::{VOICE_RATE, playable};

/// An AIFF file of 16-bit big-endian PCM, the simplest container Kanon converts.
fn aiff(rate: u32, channels: &[Vec<f32>]) -> Vec<u8> {
    let frames = channels[0].len();
    let mut data = Vec::new();
    for frame in 0..frames {
        for channel in channels {
            let sample = (channel[frame] * f32::from(i16::MAX)).round() as i16;
            data.extend_from_slice(&sample.to_be_bytes());
        }
    }
    // The sample rate is an 80-bit IEEE extended float: exponent, then a 64-bit mantissa with an
    // explicit leading one.
    let exponent = 31 - rate.leading_zeros();
    let mut extended = ((16383 + exponent) as u16).to_be_bytes().to_vec();
    extended.extend_from_slice(&(u64::from(rate) << (63 - exponent)).to_be_bytes());

    let mut comm = Vec::new();
    comm.extend_from_slice(&(channels.len() as u16).to_be_bytes());
    comm.extend_from_slice(&(frames as u32).to_be_bytes());
    comm.extend_from_slice(&16u16.to_be_bytes());
    comm.extend_from_slice(&extended);

    let mut body = b"AIFF".to_vec();
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
    body.extend_from_slice(&[0; 8]); // offset and block size
    body.extend_from_slice(&data);

    let mut file = b"FORM".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_be_bytes());
    file.extend_from_slice(&body);
    file
}

fn tone(frequency: f32, rate: u32, seconds: f32, amplitude: f32) -> Vec<f32> {
    (0..(rate as f32 * seconds) as usize)
        .map(|n| amplitude * (std::f32::consts::TAU * frequency * n as f32 / rate as f32).sin())
        .collect()
}

/// Checks the canonical header and returns the samples of a converted voice.
fn wav_samples(wav: &[u8]) -> Vec<i16> {
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "integer PCM");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
    let rate = u32::from_le_bytes(wav[24..28].try_into().unwrap());
    assert_eq!(rate, VOICE_RATE);
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "16-bit");
    assert_eq!(&wav[36..40], b"data");
    let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
    assert_eq!(wav.len(), 44 + data_len);
    wav[44..]
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn rms(samples: &[i16]) -> f64 {
    (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
}

#[test]
fn formats_qq_plays_are_uploaded_unchanged() {
    let mut id3_mp3 = b"ID3\x03\x00\x00\x00\x00\x00\x04TAG!".to_vec();
    id3_mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64, 0, 0]);
    for native in [
        b"#!SILK_V3\x0c\x00payload".to_vec(),
        b"\x02#!SILK_V3\x0c\x00payload".to_vec(),
        b"RIFF\x24\x00\x00\x00WAVEfmt payload".to_vec(),
        vec![0xFF, 0xFB, 0x90, 0x64, 0, 0],
        id3_mp3,
    ] {
        assert_eq!(playable(native.clone()), Ok(native));
    }
}

#[test]
fn other_audio_becomes_24khz_mono_wav_with_its_tone_intact() {
    // Downsampling by an integer and a fractional ratio, upsampling, and a stereo mixdown.
    for (rate, channels) in [(48_000, 2), (44_100, 1), (8_000, 1)] {
        let source = tone(440.0, rate, 1.0, 0.5);
        let converted = playable(aiff(rate, &vec![source; channels])).expect("converts");
        let samples = wav_samples(&converted);

        assert_eq!(
            samples.len(),
            VOICE_RATE as usize,
            "{rate} Hz keeps its length"
        );
        // A 440 Hz tone crosses zero 880 times a second.
        let crossings = samples
            .windows(2)
            .filter(|pair| (pair[0] < 0) != (pair[1] < 0))
            .count();
        assert!((876..=884).contains(&crossings), "{rate} Hz: {crossings}");
        // Away from the edges, the level is the source's: a sine's RMS is its peak over √2.
        let level = rms(&samples[1000..23_000]) / f64::from(i16::MAX);
        assert!(
            (level - 0.5 / 2f64.sqrt()).abs() < 0.01,
            "{rate} Hz: {level}"
        );
    }
}

#[test]
fn tones_above_the_new_nyquist_frequency_are_filtered_out() {
    // At 24 kHz, 15 kHz would alias to an audible 9 kHz whine if it were not low-passed first.
    let converted = playable(aiff(48_000, &[tone(15_000.0, 48_000, 1.0, 0.5)])).expect("converts");
    let samples = wav_samples(&converted);
    let level = rms(&samples[1000..23_000]) / f64::from(i16::MAX);
    assert!(level < 0.005, "aliased level {level}");
}

#[test]
fn audio_kanon_cannot_convert_fails_naming_what_is_accepted() {
    for unsupported in [
        b"#!AMR\n\x3c\x00\x00\x00".to_vec(),
        b"OggS\x00\x02garbage that is no ogg page".to_vec(),
        b"%PDF-1.7".to_vec(),
    ] {
        let err = playable(unsupported).expect_err("rejected");
        assert!(err.contains("QQ plays WAV, MP3 and SILK"), "{err}");
    }
    assert!(playable(Vec::new()).is_err());
}
