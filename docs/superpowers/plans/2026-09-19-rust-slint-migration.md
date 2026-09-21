# Rust Slint LRCGET-Style Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the Python desktop application with a cross-platform Rust + Slint application that preserves the existing lyrics workflow and adds a focused LRCGET-style library experience.

**Architecture:** `lyrics-core` owns portable domain logic, persistence, scanning, providers, downloads, and playback contracts. `lyrics-desktop` owns Slint presentation state and dispatches background work without blocking the UI thread. The Python application remains until parity and packaging checks pass.

**Tech Stack:** Rust 2021, Slint 1.9, SQLite, asynchronous worker tasks, platform-neutral audio/provider traits, GitHub Actions.

## Global Constraints

- Support Windows, macOS, and Linux from the start.
- Preserve LRCLib, Musixmatch, NetEase, Megalobiz, and Genius provider coverage.
- Preserve missing-only, smart-update, upgrade-plain, fix-suspicious, and replace-all modes.
- Do not copy LRCGET source code, branding, or assets.
- Do not delete Python sources until replacement verification is complete.

### Task 1: Workspace and lyric domain

**Files:** `Cargo.toml`, `crates/lyrics-core/Cargo.toml`, `crates/lyrics-core/src/lib.rs`, `crates/lyrics-core/src/model.rs`, `crates/lyrics-core/src/lrc.rs`

- [x] Add the Rust workspace and two crate boundaries.
- [x] Port timestamp parsing, synchronized/plain classification, title matching, and audit outcomes.
- [x] Add download-mode decisions to the track model.
- [ ] Run `cargo test -p lyrics-core` after installing the Rust toolchain.

### Task 2: Slint desktop shell

**Files:** `crates/lyrics-desktop/Cargo.toml`, `crates/lyrics-desktop/build.rs`, `crates/lyrics-desktop/src/main.rs`, `crates/lyrics-desktop/ui/app.slint`

- [x] Add a buildable Slint entry point and initial navigation shell.
- [x] Add folder selection, scanning state, and an empty-library state.
- [x] Persist scanned folders and track records in the platform application-data SQLite database.
- [x] Bind scanned track rows to the Slint library view.
- [x] Load existing persisted tracks when the application starts.
- [x] Make Tracks, Albums, Artists, Downloads, Logs, and Settings navigation reachable.
- [x] Bind album and artist rows to persisted track state.
- [x] Bind download progress and history logs to core state.
- [x] Persist provider enablement settings and hydrate the Settings view at startup.
- [x] Add secure Musixmatch and Genius credentials and bind their provider settings.

### Task 3: Library persistence and scanner

**Files:** `crates/lyrics-core/src/db.rs`, `crates/lyrics-core/src/scanner.rs`, `crates/lyrics-core/src/config.rs`

- [x] Add SQLite schema for directories, tracks, history, and lyric status.
- [x] Scan supported audio extensions recursively and classify LRC status.
- [x] Add metadata fallback from `Artist - Title` filenames and deterministic ordering.
- [ ] Test migrations, rescan behavior, grouping, and status summaries.
- [x] Read embedded audio tags and duration with filename fallback during scanning.
- [x] Test scanner status summaries.

### Task 4: Provider and download services

**Files:** `crates/lyrics-core/src/providers/`, `crates/lyrics-core/src/download.rs`, `crates/lyrics-core/src/fs.rs`

- [x] Define provider request/result traits for all five existing providers.
- [x] Add the first live adapter for LRCLib with typed JSON parsing and HTTP error mapping.
- [x] Add the NetEase search/lyrics adapter with typed JSON parsing and Settings integration.
- [x] Add the optional Musixmatch official API adapter with typed subtitle/plain parsing.
- [x] Add the optional Genius search/page adapter with typed search parsing and secure token settings.
- [x] Preserve Genius line breaks during HTML lyric extraction.
- [x] Add expiring provider-result cache persistence to SQLite.
- [x] Read provider cache entries before network searches and write fresh results back with a seven-day TTL.
- [x] Store the Musixmatch credential in the OS keyring instead of settings JSON.
- [x] Persist successful download status and provider source after writing lyrics.
- [x] Add bounded request retries, rate-limit backoff events, and cancellation checks.
- [x] Add desktop pause/resume and cancel controls backed by shared worker state.
- [x] Add clickable track selection and parsed timestamped lyrics in the desktop drawer.
- [x] Add a platform-neutral audio contract and Rodio-backed local play/pause controls.
- [x] Add playback position polling and synchronized active-line feedback.
- [x] Add cancellation and structured per-provider progress events.
- [x] Write lyrics atomically and select synchronized results over plain fallback.
- [ ] Add Megalobiz adapter when a stable supported endpoint is selected; connect cache reads to provider orchestration.
- [ ] Test fixture parsing, mode selection, failure isolation, and atomic replacement.

### Task 5: Playback and complete views

**Files:** `crates/lyrics-core/src/audio.rs`, `crates/lyrics-desktop/src/`, `crates/lyrics-desktop/ui/`

- [x] Define the portable audio engine and implement local playback controls.
- [x] Add Tracks, Albums, Artists, Downloads, Logs, Settings, and lyric drawer views.
- [x] Deliver worker events to Slint models and keep network/scanning off the UI thread.
- [ ] Add desktop smoke coverage for navigation and operation state transitions.
- [x] Add desktop library-model smoke coverage for grouped rows and summaries.

### Task 6: Packaging, documentation, and cleanup

**Files:** `.github/workflows/`, `README.md`, legacy Python files after verification

- [x] Add Rust CI tests for Windows, macOS, and Linux.
- [x] Add tagged Rust release builds and platform binary artifacts.
- [x] Document Rust setup and release commands.
- [x] Produce portable platform archives for each platform.
- [ ] Produce installer-grade native packages for each platform.
- [ ] Remove only superseded Python/build files after parity, sample-library, provider, playback, and packaging checks pass.