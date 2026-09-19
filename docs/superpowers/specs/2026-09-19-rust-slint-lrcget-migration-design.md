# Rust + Slint LRCGET-Style Migration Design

## Goal

Replace the Python/CustomTkinter application with an original Rust + Slint desktop application for Windows, macOS, and Linux. The result follows LRCGET's workflow and visual style while retaining this application's multi-provider downloading, lyric auditing, local-library browsing, playback, and synced-lyrics experience.

The implementation must not copy LRCGET source code, branding, or assets.

## Scope

The completed replacement provides:

- Music-folder selection and incremental local-library scanning.
- Track, Album, Artist, Downloads, Logs, and Settings views.
- Search and filtering of the local library.
- Lyrics downloads through LRCLib, Musixmatch, NetEase, Megalobiz, and Genius.
- Missing-only, smart-update, plain-to-synced upgrade, suspicious-lyrics repair, and replace-all download modes.
- Per-track and per-provider progress, pause, resume, cancellation, retry/backoff, and persisted logs/history.
- LRC parsing, validation, quality auditing, atomic writes, source attribution, and cache-based retry avoidance.
- Local playback, seek controls, and a persistent synchronized-lyrics drawer.
- Native packages for Windows, macOS, and Linux built in continuous integration.

## Architecture

Use a Rust workspace with two primary crates.

### `crates/lyrics-core`

This crate has no Slint dependency. It contains domain types and services for configuration, application-data paths, SQLite persistence, media scanning, audio-tag reading, LRC parsing and validation, audit rules, provider selection, rate limiting, download orchestration, and structured activity events.

Provider adapters implement a common asynchronous trait returning synchronized lyrics, plain lyrics, confidence, source, and diagnostic information. The download coordinator accepts a cancellation token and emits progress events; it writes output through a filesystem service that performs atomic replacement.

### `crates/lyrics-desktop`

This crate owns the Slint shell, presentation state, command dispatch, platform audio backend selection, and packaging integration. It exposes the LRCGET-style navigation and persistent player surface while keeping network, scan, database, and playback work off Slint's UI thread. It receives typed events from core and updates UI models on the Slint event loop.

The desktop crate depends on a narrow `AudioEngine` interface. The first implementation supports common local media, play/pause/seek/volume, current position, and duration. The view layer consumes that interface rather than a platform-specific backend.

## User Experience

The desktop window uses a dark, dense LRCGET-inspired layout with original assets and copy:

- Header navigation for Tracks, Albums, Artists, Downloads, Logs, and Settings.
- Folder-add and library-refresh controls plus a prominent bulk-download action.
- A table/list track browser with lyric state, metadata, artwork where available, filtering, selection, and single-track actions.
- Album and artist browsing derived from stored track metadata.
- A Downloads view with current operation, aggregate counters, per-track/provider states, and pause/resume/cancel actions.
- A persistent now-playing bar, seek/volume controls, and a toggleable lyrics drawer that highlights the active timestamped line and permits line-based seeking.

## Data, Tasks, and Failure Handling

Application state is stored under the operating system's standard application-data location. SQLite stores directories, tracks, lyric status/source, download history, cache entries, and migrations. User settings include directories, window state, view preference, playback volume, retry policy, and optional provider credentials. Secrets are never written to logs.

Long-running scans and downloads run in asynchronous worker tasks. Their messages contain an operation id, time, level, track identity, provider where applicable, and an actionable status. UI actions can cancel or pause downloads. Network errors, rate limits, missing/bad metadata, unsupported files, malformed LRC text, I/O errors, and database errors surface as recoverable operation results and logs; a single track failure never aborts the batch.

## Validation

The workspace uses Rust unit and integration tests for LRC timestamp parsing and validation, title matching, auditing, settings migration, SQLite operations, scanner classification, provider parsers using response fixtures, rate limiting, and download-mode selection. Desktop-level tests or smoke checks cover navigation, task state, and UI-thread event delivery.

Continuous integration builds and tests the workspace on Windows, macOS, and Linux. Release workflows produce installable artifacts appropriate to each platform.

## Migration and Deletion Boundary

Build the Rust replacement in milestones and retain the Python implementation until the following are verified: the workspace builds and tests pass; a sample library scans; providers download valid lyrics; the player and lyrics drawer work; and the three platform packages are produced.

After those checks, remove only superseded artifacts in a dedicated cleanup change: `app/`, `tests/`, `requirements.txt`, `build.spec`, and the Python-specific CI configuration. Retain the license, update the README for Rust installation/release workflows, and retain or re-export only icon assets used by the new application.
