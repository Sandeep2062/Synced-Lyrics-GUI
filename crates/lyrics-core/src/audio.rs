pub trait AudioEngine {
    fn play(&mut self, path: &str) -> Result<(), String>;
    fn toggle_pause(&mut self) -> Result<bool, String>;
    fn position_seconds(&self) -> Option<f64>;
    fn stop(&mut self);

    /// Seek to a specific position in seconds
    fn seek(&mut self, position_secs: f64) -> Result<(), String>;

    /// Set playback volume (0.0 to 1.0)
    fn set_volume(&mut self, volume: f32);

    /// Set playback speed multiplier (e.g., 0.5, 1.0, 1.5, 2.0)
    fn set_speed(&mut self, speed: f32);

    /// Get the total duration of the currently loaded track in seconds
    fn duration_seconds(&self) -> Option<f64>;

    /// Check if audio is currently playing (not paused, not stopped)
    fn is_playing(&self) -> bool;
}

use std::fs;
use std::path::PathBuf;

/// Persistent on-disk storage for decoded audio loudness envelopes.
/// Stored in the app data directory beside `covers/`. Each file is only
/// ~6 KB (1,500 f32 values) and loads in <0.1 ms, eliminating the several-second
/// delay when playing tracks or skipping songs.
#[derive(Clone, Debug)]
pub struct WaveformStore {
    directory: PathBuf,
}

impl WaveformStore {
    pub fn open(directory: PathBuf) -> Self {
        fs::create_dir_all(&directory).ok();
        Self { directory }
    }

    pub fn default_location() -> Option<Self> {
        crate::app_data_dir()
            .ok()
            .map(|directory| Self::open(directory.join("waveforms")))
    }

    fn entry_path(&self, audio_path: &str, stamp: u64) -> PathBuf {
        self.directory
            .join(format!("{:016x}.wf", crate::art::fingerprint(audio_path, stamp)))
    }

    pub fn get(&self, audio_path: &str, stamp: u64) -> Option<Vec<f32>> {
        let bytes = fs::read(self.entry_path(audio_path, stamp)).ok()?;
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            return None;
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        Some(values)
    }

    pub fn put(&self, audio_path: &str, stamp: u64, bars: &[f32]) {
        let mut bytes = Vec::with_capacity(bars.len() * 4);
        for &val in bars {
            bytes.extend_from_slice(&val.to_le_bytes());
        }
        let _ = fs::write(self.entry_path(audio_path, stamp), bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_store_round_trips_and_invalidates_by_stamp() {
        let temp_dir = std::env::temp_dir().join(format!("lyrics-wf-test-{}", std::process::id()));
        let store = WaveformStore::open(temp_dir.clone());

        let path = "C:/Music/test.mp3";
        let stamp1 = 12345;
        let stamp2 = 67890;
        let envelope = vec![0.1f32, 0.5f32, 0.85f32, 0.2f32];

        assert_eq!(store.get(path, stamp1), None);

        store.put(path, stamp1, &envelope);
        assert_eq!(store.get(path, stamp1), Some(envelope.clone()));

        // Different timestamp (file modified or re-encoded) must not match old cache
        assert_eq!(store.get(path, stamp2), None);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
