# 🎵 Synced Lyrics GUI

A blazing-fast, lightweight native desktop application built with **Rust** and **Slint** featuring a modern pure-black UI for managing time-synchronized (`.lrc`) and plain-text lyrics for local music libraries.

Zero webview, zero electron/browser overhead, zero Python dependencies — pure native desktop performance.

---

## ✨ Features

- 🖤 **Pure-Black Modern UI**: Native desktop GUI built using [Slint](https://slint.dev/) with a clean, high-contrast dark theme inspired by LRCGET. Fluid 60fps performance and minimal memory footprint.
- 🎤 **Karaoke Player & Lyrics Viewer**: Built-in native audio engine (powered by [Rodio](https://github.com/RustAudio/rodio)) with auto-scrolling lyrics, active line highlighting, click-to-seek playback, volume control, and playback speed adjustment.
- 🔄 **5 Intelligent Download Modes**:
  - **Missing Only**: Searches lyrics only for tracks without any `.lrc` file.
  - **Smart Update**: Automatically downloads missing lyrics, upgrades plain text to time-synced lyrics, and audits suspicious tracks.
  - **Upgrade Plain → Synced**: Detects `.lrc` files without timestamps and searches for synchronized versions.
  - **Fix Suspicious**: Audits your library for mismatched lyrics (timestamps exceeding audio length, ending too early, or title discrepancies) and cross-checks all platforms for the best version.
  - **Replace All**: Force re-downloads and updates lyrics for all tracks.
- ⚡ **Multi-Platform Search & Native Lyrics Aggregation**:
  - Queries **LRCLib**, **Musixmatch**, **NetEase**, **Megalobiz**, and **Genius** in priority order.
  - Native Musixmatch token rotation for 100% full synced lyrics out-of-the-box without needing an API key.
  - Configurable self-hosted LRCLib instance support.
  - Adaptive per-provider rate limiting and exponential backoff on HTTP 429.
- 📊 **Live Download Dashboard & Progress**:
  - Real-time provider status pills (🔍 Searching, ✅ Synced found, 📝 Plain found, ❌ Not found, ⏸ Rate limited).
  - Activity log with full download details, ETA timer, and one-click log export.
  - Pause, resume, and cancel control at any time.
- 🗄️ **Persistent Library & Fast Incremental Scanning**:
  - Embedded SQLite database (`WAL` mode) tracking directories, tracks, lyrics status, and query history.
  - Fast incremental mtime diffing: skips unchanged files during directory rescans for near-instant library updates.
  - Album art from embedded tags *or* from an image next to the files — `cover.jpg`, `folder.jpg`, `<album>.jpg`, `<track>.jpg`, or the folder's only image — extracted in parallel across several worker threads.
  - Artwork is decoded once, shrunk to a 192px RGB thumbnail, and cached on disk — so a library that has been indexed once shows every cover immediately on the next launch.
  - One cover file per album is read and decoded once for the whole album, and the album's cover is shared with every row of it instead of being decoded again per row.
  - Artwork is written straight into the rows that display it as it arrives, so a library that is still indexing scrolls smoothly.
  - Re-indexed after every scan, because that is when covers appear, change or are removed — an index entry is keyed by the audio file *and* by the images beside it.
- 🌐 **Cross-Platform**: First-class support for Windows, macOS, and Linux.

---

## 🔑 API Keys & Provider Details

| Provider | Lyrics Type | Authentication | Default Speed | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **LRCLib** | Synced & Plain | None (Public) | ~60 req/min | Primary provider; high quality community database. Supports custom self-hosted instance URLs. |
| **Musixmatch** | Synced & Plain | Built-in or API Key | ~30 req/min | Built-in token rotation provides full synced lyrics out-of-the-box. Optional API key support. |
| **NetEase** | Synced & Plain | None | ~40 req/min | Excellent international and Asian music catalog coverage. |
| **Megalobiz** | Synced | None | ~30 req/min | Community LRC database. |
| **Genius** | Plain only | Built-in or Client Token | ~10 req/min | Final fallback for plain text when no synced lyrics exist. |

---

## 🚀 Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- **Windows**: Visual Studio C++ Build Tools
- **Linux**: ALSA development libraries (`sudo apt-get install libasound2-dev`)
- **macOS**: Xcode Command Line Tools

### Running from Source

1. **Clone the repository:**
   ```bash
   git clone https://github.com/Sandeep2062/Synced-Lyrics-GUI.git
   cd Synced-Lyrics-GUI
   ```

2. **Run tests:**
   ```bash
   cargo test --workspace
   ```

3. **Launch the desktop application:**
   ```bash
   cargo run -p lyrics-desktop
   ```

---

## 🛠️ Building for Release

To compile an optimized, standalone executable:

```bash
cargo build --release -p lyrics-desktop
```

The compiled binary will be located in:
- **Windows**: `target/release/lyrics-desktop.exe`
- **macOS / Linux**: `target/release/lyrics-desktop`

---

## 🏷️ Continuous Integration & Releases

Automated GitHub Actions workflow (`.github/workflows/rust.yml`) tests every pull request across Windows, macOS, and Linux.

Pushing a version tag (or triggering the workflow manually via `workflow_dispatch`) builds release packages for all three platforms and publishes them to GitHub Releases:

| Platform | Installer | Portable Package | Notes |
| :--- | :--- | :--- | :--- |
| **Windows** | `Synced-Lyrics-windows-x86_64-setup.exe` | `Synced-Lyrics-windows-x86_64-portable.zip` | Modern Inno Setup installer; embedded icon and metadata; console window suppressed. |
| **macOS** | `Synced-Lyrics-macos.dmg` | `Synced-Lyrics-macos-portable.tar.gz` | Drag-and-drop DMG with `/Applications` link; `.app` bundle with high-DPI icons. |
| **Linux** | `Synced-Lyrics-linux-amd64.deb` & `Synced-Lyrics-linux-x86_64.AppImage` | `Synced-Lyrics-linux-x86_64-portable.tar.gz` | Debian `.deb` package with desktop integration, universal `.AppImage`, and standalone tarball. |

### 🔒 User Data Retention & Upgrade Safety

- **Automatic Upgrade Retention**: Upgrading via installer or replacing binaries retains all settings, music directory paths, credentials, and SQLite track indices. Installers never wipe user data.
- **Legacy Python Migration**: Upgrading from older Python builds (`SyncedLyricsGUI`) automatically imports your configured music directories, settings, and library database on first launch.
- **Portable Mode**: In portable builds, creating a `data/` folder (or keeping `portable.dat`) directs the app to store all settings and the database locally next to the executable, ideal for USB drives.

```bash
git tag v1.0.0
git push origin v1.0.0
```


---

## 📄 License

MIT License. See [LICENSE](LICENSE) for details.
