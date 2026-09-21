use crate::fs::{write_atomic, FileStoreError};
use crate::model::{DownloadMode, Track};
use crate::providers::{LyricsProvider, ProviderError, ProviderLyrics, ProviderName};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadEvent {
    Started {
        title: String,
    },
    Searching {
        provider: ProviderName,
    },
    ProviderFound {
        provider: ProviderName,
        synced: bool,
    },
    ProviderNotFound {
        provider: ProviderName,
    },
    Retrying {
        provider: ProviderName,
        attempt: u8,
        delay_seconds: u64,
    },
    ProviderFailed {
        provider: ProviderName,
        message: String,
    },
    Written {
        provider: ProviderName,
        synced: bool,
    },
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadOutcome {
    Downloaded {
        provider: ProviderName,
        synced: bool,
    },
    Skipped,
    Cancelled,
    NotFound,
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("could not write lyrics: {0}")]
    File(#[from] FileStoreError),
}

pub fn download_track(
    track: &Track,
    mode: DownloadMode,
    providers: &[Box<dyn LyricsProvider>],
    cancelled: &AtomicBool,
    mut emit: impl FnMut(DownloadEvent),
) -> Result<DownloadOutcome, DownloadError> {
    emit(DownloadEvent::Started {
        title: track.display_title().to_string(),
    });
    if !track.should_download(mode) {
        emit(DownloadEvent::Skipped);
        return Ok(DownloadOutcome::Skipped);
    }

    let mut best: Option<ProviderLyrics> = None;
    for provider in providers {
        if cancelled.load(Ordering::Relaxed) {
            emit(DownloadEvent::Cancelled);
            return Ok(DownloadOutcome::Cancelled);
        }
        emit(DownloadEvent::Searching {
            provider: provider.name(),
        });
        match search_with_retry(provider.as_ref(), track, cancelled, &mut emit) {
            Ok(Some(result)) => {
                emit(DownloadEvent::ProviderFound {
                    provider: provider.name(),
                    synced: result.is_synced(),
                });
                if is_better(&result, best.as_ref()) {
                    let complete = result.synced.is_some() || result.plain.is_some();
                    if complete {
                        best = Some(result);
                    }
                }
            }
            Ok(None) | Err(ProviderError::NotFound) => {
                emit(DownloadEvent::ProviderNotFound {
                    provider: provider.name(),
                });
            }
            Err(error) => emit(DownloadEvent::ProviderFailed {
                provider: provider.name(),
                message: error.to_string(),
            }),
        }
    }

    let Some(result) = best else {
        return Ok(DownloadOutcome::NotFound);
    };
    if mode == DownloadMode::UpgradePlain && !result.is_synced() {
        return Ok(DownloadOutcome::NotFound);
    }
    let text = result.best_text().unwrap_or_default();
    write_atomic(&track.lrc_path, text)?;
    let synced = result.is_synced();
    emit(DownloadEvent::Written {
        provider: result.provider,
        synced,
    });
    Ok(DownloadOutcome::Downloaded {
        provider: result.provider,
        synced,
    })
}

fn search_with_retry(
    provider: &dyn LyricsProvider,
    track: &Track,
    cancelled: &AtomicBool,
    emit: &mut impl FnMut(DownloadEvent),
) -> Result<Option<ProviderLyrics>, ProviderError> {
    const MAX_RETRIES: u8 = 2;
    let mut attempt = 0;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match provider.search(track) {
            Ok(result) => return Ok(result),
            Err(error @ (ProviderError::Request(_) | ProviderError::RateLimited { .. }))
                if attempt < MAX_RETRIES =>
            {
                attempt += 1;
                let delay_seconds = match error {
                    ProviderError::RateLimited {
                        retry_after_seconds: Some(seconds),
                    } => seconds.min(60),
                    _ => 2_u64.pow(attempt.into()),
                };
                emit(DownloadEvent::Retrying {
                    provider: provider.name(),
                    attempt,
                    delay_seconds,
                });
                for _ in 0..delay_seconds {
                    if cancelled.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_better(candidate: &ProviderLyrics, current: Option<&ProviderLyrics>) -> bool {
    let Some(current) = current else {
        return true;
    };
    (candidate.is_synced(), candidate.confidence) > (current.is_synced(), current.confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderName;
    use std::sync::atomic::AtomicBool;

    struct FakeProvider {
        provider: ProviderName,
        result: Result<Option<ProviderLyrics>, ProviderError>,
    }

    struct FlakyProvider {
        attempts: std::sync::Mutex<u8>,
    }

    impl LyricsProvider for FlakyProvider {
        fn name(&self) -> ProviderName {
            ProviderName::Lrclib
        }

        fn search(&self, _track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
            let mut attempts = self.attempts.lock().unwrap();
            *attempts += 1;
            if *attempts == 1 {
                Err(ProviderError::Request("temporary failure".to_string()))
            } else {
                Ok(Some(ProviderLyrics {
                    provider: ProviderName::Lrclib,
                    synced: Some("[00:01]retried".to_string()),
                    plain: None,
                    confidence: 0.9,
                }))
            }
        }
    }

    impl LyricsProvider for FakeProvider {
        fn name(&self) -> ProviderName {
            self.provider
        }

        fn search(&self, _track: &Track) -> Result<Option<ProviderLyrics>, ProviderError> {
            self.result.clone()
        }
    }

    fn track(path: &str) -> Track {
        Track {
            audio_path: format!("{path}.mp3"),
            lrc_path: format!("{path}.lrc"),
            artist: "Artist".to_string(),
            title: "Song".to_string(),
            album: "Album".to_string(),
            duration_seconds: None,
            status: crate::model::LyricsStatus::Missing,
            mtime: None,
            last_checked: None,
        }
    }

    #[test]
    fn chooses_synced_result_and_writes_it() {
        let root =
            std::env::temp_dir().join(format!("synced-lyrics-download-{}", std::process::id()));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(Some(ProviderLyrics {
                    provider: ProviderName::Genius,
                    synced: None,
                    plain: Some("plain".to_string()),
                    confidence: 1.0,
                })),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(Some(ProviderLyrics {
                    provider: ProviderName::Lrclib,
                    synced: Some("[00:01]synced".to_string()),
                    plain: None,
                    confidence: 0.5,
                })),
            }),
        ];
        let events = std::cell::RefCell::new(Vec::new());

        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Downloaded {
                provider: ProviderName::Lrclib,
                synced: true
            }
        );
        assert_eq!(
            std::fs::read_to_string(&target.lrc_path).unwrap(),
            "[00:01]synced"
        );
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, DownloadEvent::Written { synced: true, .. })));
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn retries_temporary_provider_failures() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = track(root.to_str().unwrap());
        let providers: Vec<Box<dyn LyricsProvider>> = vec![Box::new(FlakyProvider {
            attempts: std::sync::Mutex::new(0),
        })];
        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::MissingOnly,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();
        assert!(matches!(outcome, DownloadOutcome::Downloaded { .. }));
        assert!(events
            .borrow()
            .iter()
            .any(|event| matches!(event, DownloadEvent::Retrying { attempt: 1, .. })));
        let _ = std::fs::remove_file(target.lrc_path);
    }

    #[test]
    fn upgrade_plain_ignores_plain_only_and_emits_provider_events() {
        let root = std::env::temp_dir().join(format!(
            "synced-lyrics-upgrade-plain-{}",
            std::process::id()
        ));
        let mut target = track(root.to_str().unwrap());
        target.status = crate::model::LyricsStatus::Plain;

        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(FakeProvider {
                provider: ProviderName::Genius,
                result: Ok(Some(ProviderLyrics {
                    provider: ProviderName::Genius,
                    synced: None,
                    plain: Some("plain lyrics only".to_string()),
                    confidence: 0.5,
                })),
            }),
            Box::new(FakeProvider {
                provider: ProviderName::Lrclib,
                result: Ok(None),
            }),
        ];

        let events = std::cell::RefCell::new(Vec::new());
        let outcome = download_track(
            &target,
            DownloadMode::UpgradePlain,
            &providers,
            &AtomicBool::new(false),
            |event| events.borrow_mut().push(event),
        )
        .unwrap();

        assert_eq!(outcome, DownloadOutcome::NotFound);
        assert!(!std::path::Path::new(&target.lrc_path).exists());
        assert!(events.borrow().iter().any(|e| matches!(
            e,
            DownloadEvent::ProviderFound {
                provider: ProviderName::Genius,
                synced: false
            }
        )));
        assert!(events.borrow().iter().any(|e| matches!(
            e,
            DownloadEvent::ProviderNotFound {
                provider: ProviderName::Lrclib
            }
        )));
    }
}
