use crate::model::{LyricsStatus, Track};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "mp4", "m4b", "opus", "ogg", "oga", "wav", "wave", "wma", "aac", "aiff",
    "aif", "ape", "wv", "alac", "webm",
];

#[derive(Debug)]
pub enum ScanError {
    RootMissing(PathBuf),
    Io(io::Error),
}

impl From<io::Error> for ScanError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    pub total: usize,
    pub missing: usize,
    pub plain: usize,
    pub synced: usize,
    pub suspicious: usize,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IncrementalScanResult {
    pub all_tracks: Vec<Track>,
    pub summary: ScanSummary,
    pub changed_tracks: Vec<Track>,
}

pub fn scan_directory(root: impl AsRef<Path>) -> Result<(Vec<Track>, ScanSummary), ScanError> {
    let result = scan_directory_incremental(root, None)?;
    Ok((result.all_tracks, result.summary))
}

pub fn scan_directory_incremental(
    root: impl AsRef<Path>,
    existing: Option<&std::collections::HashMap<String, Track>>,
) -> Result<IncrementalScanResult, ScanError> {
    scan_directory_incremental_with_progress(root, existing, |_, _| {})
}

pub fn scan_directory_incremental_with_progress<F>(
    root: impl AsRef<Path>,
    existing: Option<&std::collections::HashMap<String, Track>>,
    on_progress: F,
) -> Result<IncrementalScanResult, ScanError>
where
    F: FnMut(usize, usize),
{
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(ScanError::RootMissing(root.to_path_buf()));
    }

    let mut paths = Vec::new();
    collect_audio_paths(root, &mut paths)?;
    paths.sort();

    Ok(scan_audio_paths_incremental(paths, existing, on_progress))
}

pub fn scan_audio_paths_incremental<F>(
    paths: Vec<PathBuf>,
    existing: Option<&std::collections::HashMap<String, Track>>,
    mut on_progress: F,
) -> IncrementalScanResult
where
    F: FnMut(usize, usize),
{
    let total = paths.len();
    let mut tracks = Vec::with_capacity(total);
    let mut changed_tracks = Vec::new();
    let mut summary = ScanSummary {
        total,
        ..ScanSummary::default()
    };

    for (index, path) in paths.into_iter().enumerate() {
        on_progress(index, total);

        let mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64());

        let lrc_path = path.with_extension("lrc");
        let path_str = path.to_string_lossy().into_owned();

        if let Some(prev_track) = existing.and_then(|map| map.get(&path_str)) {
            let audio_mtime_matches = match (mtime, prev_track.mtime) {
                (Some(curr), Some(prev)) => (curr - prev).abs() < 0.01,
                _ => false,
            };

            if audio_mtime_matches {
                let lrc_exists = lrc_path.is_file();
                let status = if !lrc_exists {
                    LyricsStatus::Missing
                } else {
                    lyric_status(&lrc_path, prev_track.duration_seconds, &prev_track.title)
                };

                let track_changed = status != prev_track.status;

                let track = Track {
                    audio_path: path_str,
                    lrc_path: lrc_path.to_string_lossy().into_owned(),
                    artist: prev_track.artist.clone(),
                    title: prev_track.title.clone(),
                    album: prev_track.album.clone(),
                    duration_seconds: prev_track.duration_seconds,
                    status,
                    mtime,
                    last_checked: prev_track.last_checked,
                };

                match status {
                    LyricsStatus::Missing => summary.missing += 1,
                    LyricsStatus::Plain => summary.plain += 1,
                    LyricsStatus::Synced => summary.synced += 1,
                    LyricsStatus::Suspicious => summary.suspicious += 1,
                }

                if track_changed {
                    changed_tracks.push(track.clone());
                }
                tracks.push(track);
                continue;
            }
        }

        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Unknown title");
        let (fallback_artist, fallback_title) = stem
            .split_once(" - ")
            .map(|(artist, title)| (artist.to_string(), title.to_string()))
            .unwrap_or_else(|| ("Unknown Artist".to_string(), stem.to_string()));

        let metadata = read_metadata(&path);
        let title = metadata.title.as_deref().unwrap_or(&fallback_title);
        let status = lyric_status(&lrc_path, metadata.duration_seconds, title);

        match status {
            LyricsStatus::Missing => summary.missing += 1,
            LyricsStatus::Plain => summary.plain += 1,
            LyricsStatus::Synced => summary.synced += 1,
            LyricsStatus::Suspicious => summary.suspicious += 1,
        }

        let track = Track {
            audio_path: path_str,
            lrc_path: lrc_path.to_string_lossy().into_owned(),
            artist: metadata.artist.unwrap_or(fallback_artist),
            title: metadata.title.unwrap_or(fallback_title),
            album: metadata
                .album
                .unwrap_or_else(|| "Unknown Album".to_string()),
            duration_seconds: metadata.duration_seconds,
            status,
            mtime,
            last_checked: None,
        };

        changed_tracks.push(track.clone());
        tracks.push(track);
    }

    on_progress(total, total);

    IncrementalScanResult {
        all_tracks: tracks,
        summary,
        changed_tracks,
    }
}

pub fn collect_audio_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), ScanError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_audio_paths(&path, paths)?;
        } else if is_supported_audio(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

struct AudioMetadata {
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    duration_seconds: Option<f64>,
}

fn read_metadata(path: &Path) -> AudioMetadata {
    let Ok(tagged_file) = Probe::open(path).and_then(|probe| probe.read()) else {
        return AudioMetadata {
            artist: None,
            title: None,
            album: None,
            duration_seconds: None,
        };
    };
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    AudioMetadata {
        artist: tag.and_then(|tag| tag.artist().map(|value| value.into_owned())),
        title: tag.and_then(|tag| tag.title().map(|value| value.into_owned())),
        album: tag.and_then(|tag| tag.album().map(|value| value.into_owned())),
        duration_seconds: Some(tagged_file.properties().duration().as_secs_f64())
            .filter(|duration| *duration > 0.0),
    }
}

fn lyric_status(path: &Path, duration_seconds: Option<f64>, title: &str) -> LyricsStatus {
    let Ok(contents) = fs::read_to_string(path) else {
        return LyricsStatus::Missing;
    };
    if contents.trim().is_empty() {
        LyricsStatus::Missing
    } else if crate::lrc::is_synced(&contents) {
        if let Some(issue) = crate::lrc::audit_lrc(&contents, duration_seconds, title) {
            match issue {
                crate::lrc::LrcIssue::TooFewTimestamps
                | crate::lrc::LrcIssue::TooLong
                | crate::lrc::LrcIssue::EndsTooEarly
                | crate::lrc::LrcIssue::TitleMismatch => LyricsStatus::Suspicious,
                _ => LyricsStatus::Synced,
            }
        } else {
            LyricsStatus::Synced
        }
    } else {
        LyricsStatus::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn scans_supported_files_and_falls_back_to_filename_metadata() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Artist - Song.mp3"), []).unwrap();
        fs::write(root.join("ignored.txt"), []).unwrap();
        fs::write(
            root.join("Artist - Song.lrc"),
            "[00:01]a\n[00:02]b\n[00:03]c",
        )
        .unwrap();

        let (tracks, summary) = scan_directory(&root).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artist, "Artist");
        assert_eq!(tracks[0].title, "Song");
        assert_eq!(summary.synced, 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_each_lyrics_status_in_summary() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-status-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Artist - Missing.mp3"), []).unwrap();
        fs::write(root.join("Artist - Plain.mp3"), []).unwrap();
        fs::write(root.join("Artist - Plain.lrc"), "plain lyrics").unwrap();
        fs::write(root.join("Artist - Synced.mp3"), []).unwrap();
        fs::write(
            root.join("Artist - Synced.lrc"),
            "[00:01]a\n[00:02]b\n[00:03]c",
        )
        .unwrap();

        let (_, summary) = scan_directory(&root).unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.plain, 1);
        assert_eq!(summary.synced, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_scan_skips_metadata_when_mtime_matches() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-mtime-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let audio_path = root.join("Artist - Song.mp3");
        fs::write(&audio_path, []).unwrap();
        let lrc_path = root.join("Artist - Song.lrc");
        fs::write(&lrc_path, "[00:01]a\n[00:02]b\n[00:03]c").unwrap();

        let mtime = std::fs::metadata(&audio_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let mut existing = std::collections::HashMap::new();
        existing.insert(
            audio_path.to_string_lossy().into_owned(),
            Track {
                audio_path: audio_path.to_string_lossy().into_owned(),
                lrc_path: lrc_path.to_string_lossy().into_owned(),
                artist: "PreExistingArtist".to_string(),
                title: "PreExistingTitle".to_string(),
                album: "PreExistingAlbum".to_string(),
                duration_seconds: Some(4.0),
                status: LyricsStatus::Synced,
                mtime: Some(mtime),
                last_checked: None,
            },
        );

        let result = scan_directory_incremental(&root, Some(&existing)).unwrap();
        assert_eq!(result.all_tracks.len(), 1);
        assert_eq!(result.all_tracks[0].artist, "PreExistingArtist");
        assert_eq!(result.all_tracks[0].album, "PreExistingAlbum");
        assert_eq!(result.all_tracks[0].status, LyricsStatus::Synced);
        assert!(result.changed_tracks.is_empty(), "Unchanged track must not be marked as changed!");

        fs::remove_dir_all(root).unwrap();
    }
}
