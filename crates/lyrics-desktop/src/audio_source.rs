//! A symphonia-backed [`Source`] that decodes straight from a file on disk.
//!
//! rodio's bundled decoder wraps whatever reader it is handed in its internal
//! `ReadSeekSource`, whose `MediaSource::byte_len()` is hardcoded to `None`:
//!
//! ```text
//! rodio-0.20.1/src/decoder/read_seek_source.rs
//!     fn byte_len(&self) -> Option<u64> { None }
//! ```
//!
//! The ISO/MP4 (M4A/AAC) demuxer needs the real length of the stream to locate
//! the `moov` atom, so probing an M4A file fails with
//! `Error::SeekError(SeekErrorKind::Unseekable)`:
//!
//! ```text
//! symphonia-format-isomp4-0.5.5/src/demuxer.rs
//!     let len = mss.byte_len().ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
//! ```
//!
//! ...and rodio 0.20.1 reacts to a seek error during initialisation by aborting
//! the process:
//!
//! ```text
//! rodio-0.20.1/src/decoder/symphonia.rs
//!     Error::SeekError(_) => unreachable!("Seek errors should not occur during initialization")
//! ```
//!
//! Every rodio constructor is affected, so no amount of "try another decoder"
//! fixes it. Feeding symphonia a plain `std::fs::File` — whose `MediaSource`
//! impl reports the file size — sidesteps the whole path, makes M4A playable,
//! and gives us a working `try_seek` as a bonus.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::Source;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units;

/// Decode errors are not fatal on their own — mirroring rodio, we only give up
/// after this many consecutive packets fail to decode.
const MAX_DECODE_RETRIES: usize = 3;

/// An `i16` PCM [`Source`] decoded from a file on disk.
pub struct FileSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    spec: SignalSpec,
    buffer: SampleBuffer<i16>,
    /// Read position inside `buffer` (in interleaved samples).
    cursor: usize,
    /// Capacity of `buffer` in frames, to avoid reallocating on every packet.
    buffer_capacity: usize,
    /// When set, the buffered frame predates a seek and must be discarded.
    stale: bool,
    /// Reached the end of the stream (or gave up on a damaged one).
    sealed: bool,
    duration: Option<Duration>,
}

impl FileSource {
    /// Opens and probes `path`. Fails with a human-readable message rather than
    /// panicking for anything we cannot play.
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
        // A `File` (not a plain `Read`) is what provides `byte_len()`, which the
        // MP4 demuxer requires.
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("Unsupported or damaged audio: {e}"))?;

        let track = probed
            .format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| "No playable audio track in this file".to_string())?;

        let track_id = track.id;
        let duration = track
            .codec_params
            .time_base
            .zip(track.codec_params.n_frames)
            .map(|(base, frames)| {
                let t = base.calc_time(frames);
                Duration::from_secs_f64(t.seconds as f64 + t.frac)
            });
        let spec = SignalSpec::new(
            track.codec_params.sample_rate.unwrap_or(44_100),
            track.codec_params.channels.unwrap_or_else(|| {
                symphonia::core::audio::Channels::FRONT_LEFT
                    | symphonia::core::audio::Channels::FRONT_RIGHT
            }),
        );

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("Unsupported codec: {e}"))?;

        Ok(Self {
            format: probed.format,
            decoder,
            track_id,
            spec,
            buffer: SampleBuffer::<i16>::new(units::Duration::from(0u64), spec),
            cursor: 0,
            buffer_capacity: 0,
            stale: true,
            sealed: false,
            duration,
        })
    }

    /// Decodes the next packet into `buffer`. Sets `sealed` when the stream is
    /// exhausted (or too damaged to continue), which ends iteration cleanly.
    fn pump(&mut self) {
        let mut decode_errors = 0usize;
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                // End of stream, a container that resets mid-stream, or a
                // non-fatal read error mid-file: stop playing rather than panic.
                Err(_) => {
                    self.sealed = true;
                    return;
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = decoded.spec().to_owned();
                    let cap = decoded.capacity();
                    if self.buffer_capacity < cap || self.spec != spec {
                        let duration = units::Duration::from(cap as u64);
                        self.buffer = SampleBuffer::<i16>::new(duration, spec);
                        self.buffer_capacity = cap;
                        self.spec = spec;
                    }
                    self.buffer.copy_interleaved_ref(decoded);
                    self.cursor = 0;
                    self.stale = false;
                    return;
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    decode_errors += 1;
                    if decode_errors > MAX_DECODE_RETRIES {
                        self.sealed = true;
                        return;
                    }
                }
                Err(_) => {
                    self.sealed = true;
                    return;
                }
            }
        }
    }

    /// Decodes and returns the next packet's interleaved samples.
    pub fn next_packet_samples(&mut self) -> Option<&[i16]> {
        if self.sealed {
            return None;
        }
        self.pump();
        if self.sealed || self.stale {
            None
        } else {
            Some(self.buffer.samples())
        }
    }
}

impl Iterator for FileSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        loop {
            if !self.stale && self.cursor < self.buffer.samples().len() {
                let sample = self.buffer.samples()[self.cursor];
                self.cursor += 1;
                return Some(sample);
            }
            if self.sealed {
                return None;
            }
            // `pump` clears `stale` on success, so an empty decoded frame just
            // loops back around and pumps again.
            self.pump();
        }
    }
}

impl Source for FileSource {
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.buffer.samples().len())
    }

    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        // Some decoders can only seek to just before the end of the stream.
        let target = match self.duration {
            Some(total) if pos >= total => (total.as_secs_f64() - 0.001).max(0.0),
            _ => pos.as_secs_f64(),
        };

        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: target.into(),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| {
                SeekError::Other(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Seek failed: {e}"),
                )))
            })?;

        self.decoder.reset();
        // The buffered frame belongs to the old position; pump() refills it.
        self.stale = true;
        self.cursor = 0;
        self.sealed = false;
        self.pump();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Live-level capture: a Source wrapper that records RMS amplitude as audio
// flows through it to the speaker, so the UI can show a truly live waveform.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Shared handle the UI timer reads to get the current live amplitude (0.0–1.0)
/// encoded as `f32` bits in an `AtomicU32` for lock-free access.
#[derive(Clone)]
pub struct LiveLevel(Arc<AtomicU32>);

impl LiveLevel {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU32::new(0u32)))
    }

    /// Current amplitude as 0.0–1.0.
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// How many samples to accumulate before updating the shared level.
/// At 44 100 Hz stereo this is ~23 ms — fast enough for 30 fps reads,
/// large enough that each RMS window is musically meaningful.
const LEVEL_WINDOW: usize = 2048;

/// A [`Source`] adapter that computes the running RMS of every `LEVEL_WINDOW`
/// samples passing through it and writes the result to a [`LiveLevel`].
/// The inner source is yielded unmodified — this is purely a measurement tap.
pub struct LevelCapture<S> {
    inner: S,
    level: LiveLevel,
    /// Sum of squares for the current window.
    sum_sq: f64,
    /// Samples counted in the current window.
    count: usize,
}

impl<S> LevelCapture<S> {
    pub fn new(inner: S, level: LiveLevel) -> Self {
        Self {
            inner,
            level,
            sum_sq: 0.0,
            count: 0,
        }
    }
}

impl<S: Iterator<Item = i16>> Iterator for LevelCapture<S> {
    type Item = i16;

    #[inline]
    fn next(&mut self) -> Option<i16> {
        let sample = self.inner.next()?;

        // Accumulate into the RMS window.
        let normalised = sample as f64 / i16::MAX as f64;
        self.sum_sq += normalised * normalised;
        self.count += 1;

        if self.count >= LEVEL_WINDOW {
            let rms = (self.sum_sq / self.count as f64).sqrt() as f32;
            // Perceptual scaling: convert to a dB-like curve so quiet sections
            // are visibly small and loud hits are visibly tall, instead of
            // everything saturating near 1.0 with a linear multiplier.
            //
            // -40 dB (rms ≈ 0.01) → 0.0, -6 dB (rms ≈ 0.5) → ~0.85,
            // 0 dB (rms = 1.0) → 1.0.
            let db = if rms > 1e-6 { 20.0 * rms.log10() } else { -60.0 };
            let visual = ((db + 40.0) / 40.0).clamp(0.0, 1.0);
            self.level.set(visual);
            self.sum_sq = 0.0;
            self.count = 0;
        }

        Some(sample)
    }
}

impl<S: Source<Item = i16>> Source for LevelCapture<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }
    fn channels(&self) -> u16 {
        self.inner.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.sum_sq = 0.0;
        self.count = 0;
        self.inner.try_seek(pos)
    }
}

/// Display-ready peak-per-chunk bars (0.12..1.0) for the seek bar's waveform.
/// Decoding runs through [`FileSource`], so compressed formats that rodio
/// cannot open (notably M4A) still produce a real waveform.
///
/// `num_bars` values are spread evenly across the **whole** file: bucket `i` is
/// the loudest sample in that slice of the track. The result is the track's real
/// loudness envelope, which the player uses both as the static shape of the
/// waveform and as the level the animated bars ride on.
pub fn waveform_bars(path: &Path, num_bars: usize, fallback: impl Fn() -> Vec<f32>) -> Vec<f32> {
    let Ok(mut source) = FileSource::open(path) else {
        return fallback();
    };
    let num_bars = num_bars.max(1);
    let rate_times_channels = (source.sample_rate() as f64) * (source.channels().max(1) as f64);
    // Interleaved sample index at the end of the stream, when the container
    // tells us how long the track is.
    let total_samples = source
        .total_duration()
        .map(|d| d.as_secs_f64() * rate_times_channels)
        .filter(|total| *total > 1.0);

    let mut peaks = vec![0.0f32; num_bars];

    match total_samples {
        // Known length: bucket samples by their position in the index range.
        // Decodes packet-by-packet with sample striding for fast envelope extraction.
        Some(total) => {
            let mut sample_index = 0usize;
            let bucket_scale = num_bars as f64 / total;
            while let Some(samples) = source.next_packet_samples() {
                // Stride by 4 samples: at 44.1kHz stereo, 4 samples is ~0.045 ms,
                // which easily catches all transients while cutting loop iterations by 4x.
                let stride = 4;
                for (chunk_idx, &sample) in samples.iter().enumerate().step_by(stride) {
                    let i = sample_index + chunk_idx;
                    let bucket = ((i as f64) * bucket_scale) as usize;
                    if bucket >= num_bars {
                        break;
                    }
                    let amplitude = (sample as f32).abs();
                    if amplitude > peaks[bucket] {
                        peaks[bucket] = amplitude;
                    }
                }
                sample_index += samples.len();
                if sample_index as f64 >= total {
                    break;
                }
            }
        }
        // Length unknown (some containers omit it): keep a strided run of
        // samples and split it into equal chunks afterwards.
        None => {
            let mut samples: Vec<f32> = Vec::with_capacity(200_000);
            while let Some(packet) = source.next_packet_samples() {
                for &sample in packet.iter().step_by(8) {
                    samples.push((sample as f32).abs());
                    if samples.len() >= 600_000 {
                        break;
                    }
                }
                if samples.len() >= 600_000 {
                    break;
                }
            }
            if samples.is_empty() {
                return fallback();
            }
            let chunk_size = (samples.len() / num_bars).max(1);
            for (index, chunk) in samples.chunks(chunk_size).enumerate() {
                if index >= num_bars {
                    break;
                }
                peaks[index] = chunk.iter().copied().fold(0.0f32, f32::max);
            }
        }
    }

    // Normalise to the loudest moment of *this* track, so a quietly mastered
    // song still shows its shape instead of a flat line. A file that decodes to
    // silence keeps every peak at 0 and ends up as the flat 0.15 rail, which is
    // the honest thing to draw.
    let max_peak = peaks.iter().copied().fold(0.001f32, f32::max);
    peaks
        .into_iter()
        .map(|p| (0.15 + 0.85 * (p / max_peak)).clamp(0.12, 1.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal 16-bit PCM WAV writer, so the decoder can be tested end to end
    /// without needing a real audio device or bundling a fixture.
    fn write_wav(path: &Path, sample_rate: u32, channels: u16, seconds: f32) {
        let frames = (sample_rate as f32 * seconds) as u32;
        let bits = 16u16;
        let block_align = channels * bits / 8;
        let data_len = frames * block_align as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for frame in 0..frames {
            // 440 Hz square-ish tone so every sample is non-zero.
            let value = if (frame / 10) % 2 == 0 {
                12_000i16
            } else {
                -12_000
            };
            for _ in 0..channels {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        std::fs::write(path, bytes).expect("write wav fixture");
    }

    #[test]
    fn decodes_a_wav_file_end_to_end() {
        let path = std::env::temp_dir().join("lyrics-desktop-file-source-test.wav");
        write_wav(&path, 8_000, 1, 0.5);

        let mut source = FileSource::open(&path).expect("open wav");
        assert_eq!(source.sample_rate(), 8_000);
        assert_eq!(source.channels(), 1);

        let samples: Vec<i16> = (&mut source).collect();
        assert_eq!(samples.len(), 4_000, "every sample should decode");
        assert!(
            samples.iter().any(|&s| s != 0),
            "samples should carry audio"
        );

        let duration = source.total_duration().expect("duration");
        assert!(
            (duration.as_secs_f64() - 0.5).abs() < 0.05,
            "duration {duration:?} should be about 0.5s"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn seek_moves_the_read_position() {
        let path = std::env::temp_dir().join("lyrics-desktop-file-source-seek-test.wav");
        write_wav(&path, 8_000, 1, 1.0);

        let mut source = FileSource::open(&path).expect("open wav");
        source
            .try_seek(Duration::from_millis(750))
            .expect("seek should succeed");
        let remaining = (&mut source).count();
        assert!(
            remaining < 4_000 && remaining > 0,
            "after seeking to 0.75s about 0.25s (2000 samples) should remain, got {remaining}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Minimal WAV whose first half is a tone and whose second half is silence,
    /// so a waveform that stops reading early is obvious.
    fn write_half_silent_wav(path: &Path, sample_rate: u32, seconds: f32) {
        let frames = (sample_rate as f32 * seconds) as u32;
        let data_len = frames * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for frame in 0..frames {
            let value = if frame < frames / 2 {
                if (frame / 10) % 2 == 0 {
                    12_000i16
                } else {
                    -12_000
                }
            } else {
                0
            };
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(path, bytes).expect("write wav fixture");
    }

    /// The seek bar draws the loudness of the *whole* track. The previous
    /// implementation stopped sampling after 120 000 samples — about 11 seconds
    /// of CD-quality stereo — and normalised that fragment as if it were the
    /// song, so everything after the opening bars was drawn wrong.
    #[test]
    fn waveform_covers_the_whole_file_not_just_its_start() {
        // Long enough that the old sample cap would have hidden the silence.
        let path = std::env::temp_dir().join("lyrics-desktop-waveform-coverage.wav");
        write_half_silent_wav(&path, 8_000, 130.0);

        let bars = waveform_bars(&path, 40, || vec![0.5; 40]);
        assert_eq!(bars.len(), 40);
        for &bar in &bars {
            assert!((0.12..=1.0).contains(&bar), "bar out of range: {bar}");
        }

        let loudest = bars[..8].iter().copied().fold(0.0f32, f32::max);
        let quietest = bars[32..].iter().copied().fold(0.0f32, f32::max);
        assert!(
            loudest > 0.8,
            "the opening tone should be near full scale: {loudest}"
        );
        assert!(
            quietest < 0.3,
            "the silent second half should be flat, got {quietest} — the envelope stopped early"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The regression this module exists for. Point `LYRICS_TEST_M4A` at a real
    /// M4A/AAC file (e.g. from your own library) to run it; without the variable
    /// it is skipped, so CI stays green on machines with no such file.
    ///
    /// Before the fix this aborted the whole process with rodio's
    /// `unreachable!("Seek errors should not occur during initialization")`.
    #[test]
    fn decodes_a_real_m4a_file() {
        let Ok(path) = std::env::var("LYRICS_TEST_M4A") else {
            eprintln!("skipping: set LYRICS_TEST_M4A=<file.m4a> to run");
            return;
        };
        let path = Path::new(&path);

        let mut source = FileSource::open(path).expect("M4A should open, not panic");
        assert!(source.sample_rate() >= 8_000, "plausible sample rate");
        assert!(source.channels() >= 1);
        assert!(
            source.total_duration().is_some_and(|d| d.as_secs() > 1),
            "a real track should report its length"
        );

        // Skip the priming silence AAC encoders put at the head of the stream.
        let head: Vec<i16> = (&mut source).skip(4_096).take(100_000).collect();
        assert!(
            head.iter().any(|&s| s != 0),
            "decoded audio should be non-silent"
        );

        // Seeking is the operation the MP4 demuxer needed `byte_len` for.
        source
            .try_seek(Duration::from_secs(10))
            .expect("seek inside an M4A should work");
        let after: Vec<i16> = (&mut source).take(4_096).collect();
        assert_eq!(after.len(), 4_096, "samples should continue after a seek");
        assert_ne!(head[..4_096], after[..], "seek should move playback");
    }

    #[test]
    fn unreadable_input_errors_instead_of_panicking() {
        assert!(FileSource::open(Path::new("definitely-not-a-real-file.mp3")).is_err());

        let path = std::env::temp_dir().join("lyrics-desktop-file-source-garbage.m4a");
        std::fs::write(&path, b"this is not audio at all").expect("write junk");
        assert!(
            FileSource::open(&path).is_err(),
            "garbage input must produce an error, never a panic"
        );
        let _ = std::fs::remove_file(&path);
    }
}
