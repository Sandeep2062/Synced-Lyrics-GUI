# 🎵 Synced Lyrics GUI

A modern desktop application built in pure Python with a pure-black UI for managing time-synchronized (`.lrc`) and plain-text lyrics for local music libraries.

Powered by multi-provider lyrics aggregation ([LRCLib](https://lrclib.net), [Musixmatch](https://musixmatch.com), [NetEase](https://music.163.com), [Megalobiz](https://megalobiz.com), and [Genius](https://genius.com)), intelligent rate-limiting, and quality auditing.

---

## ✨ Features

- 🖤 **Pure-Black Modern UI**: Native desktop application built using [CustomTkinter](https://customtkinter.tomschimansky.com) with a clean dark theme. Zero HTML, zero webview, zero browser bloat.
- 🎤 **Karaoke Player & Lyrics Viewer**: Built-in audio playback engine with auto-scrolling lyrics, active line highlighting, and click-to-seek navigation.
- 🔄 **5 Intelligent Download Modes**:
  - **Missing Only**: Searches lyrics only for tracks without any `.lrc` file.
  - **Smart Update**: Automatically downloads missing lyrics, upgrades plain text to time-synced lyrics, and audits suspicious tracks.
  - **Upgrade Plain → Synced**: Detects `.lrc` files without timestamps and searches for synchronized versions.
  - **Fix Suspicious**: Audits your library for mismatched lyrics (timestamps exceeding audio length, ending too early, or title discrepancies) and cross-checks all platforms for the best version.
  - **Replace All**: Force re-downloads and updates lyrics for all tracks in the folder.
- ⚡ **Multi-Platform Search & Adaptive Rate Limiting**:
  - Queries **LRCLib**, **Musixmatch**, **NetEase**, **Megalobiz**, and **Genius** in priority order.
  - Spaces requests with polite intervals to avoid getting blocked.
  - Automatically backs off on HTTP `429 Too Many Requests` and resumes gracefully.
- 📊 **Live Per-Song & Per-Platform Progress**:
  - Real-time status indicators (🔍 Searching, ✅ Synced found, 📝 Plain found, ❌ Not found, ⏸ Rate limited, ⏭ Skipped).
  - Activity log with full download details and elapsed/ETA timers.
- 🗄️ **Persistent Library & History**:
  - SQLite database storing tracks, lyrics sources, timestamps, and scan directories.
  - Cache table to remember recently queried tracks and prevent repeated API hits.
- ⚙️ **API Configuration & Guides**:
  - Run out-of-the-box with built-in token rotation and public endpoints.
  - Optional: add your own custom API keys for Musixmatch and Genius directly in the Settings tab with inline guides.
- 🎬 **FFmpeg Audio Engine**: Auto-downloads and configures FFmpeg binaries in the application folder on first run.
- 🚀 **Built-in Auto-Updater**: One-click updates checking GitHub Releases directly from the Settings view.
- 📦 **Automated GitHub Actions CI/CD**: Pushing a version tag (e.g. `git tag v1.0.0 && git push origin v1.0.0`) automatically builds a standalone Windows `.exe` and attaches it to a GitHub release.

---

## 🚀 Getting Started

### Prerequisites
- Windows 10 or 11
- Python 3.11+ (if running from source)

### Running from Source

1. **Clone the repository:**
   ```bash
   git clone https://github.com/Sandeep2062/Synced-Lyrics-GUI.git
   cd Synced-Lyrics-GUI
   ```

2. **Install dependencies:**
   ```bash
   pip install -r requirements.txt
   ```

3. **Launch the application:**
   ```bash
   python -m app
   ```

---

## 🔑 API Keys & Provider Details

| Provider | Lyrics Type | Authentication | Limits | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **LRCLib** | Synced & Plain | None (Public) | ~60 req/min | Primary provider; high quality community database. |
| **Musixmatch** | Synced & Plain | Built-in or API Key | 2,000 req/day (Free API) | Leave blank in Settings to use built-in token rotation for full synced lyrics. |
| **NetEase** | Synced & Plain | None | ~40 req/min | Excellent international & Asian music coverage. |
| **Megalobiz** | Synced | None | ~30 req/min | Community LRC provider. |
| **Genius** | Plain only | Built-in or Client Token | ~5 req/min | Final fallback for plain text when no synced lyrics exist. |

### Adding Custom Keys:
Navigate to **Settings ➔ API Keys & Integrations**:
- **Musixmatch**: Get an API key from [developer.musixmatch.com](https://developer.musixmatch.com). *(Note: Official free API tier returns 30-40% previews; leaving this empty uses full synced lyrics).*
- **Genius**: Create an application at [genius.com/api-clients](https://genius.com/api-clients) and paste your **Client Access Token**.

---

## 🛠️ Building Standalone `.exe`

To build the single-file executable locally:

```bash
pip install pyinstaller
pyinstaller build.spec
```

The output executable will be created in `dist/SyncedLyricsGUI.exe`.

---

## 🏷️ Releasing via GitHub Actions

The repository includes a GitHub Actions workflow (`.github/workflows/release.yml`) that automatically compiles `SyncedLyricsGUI.exe` and creates a GitHub release whenever you push a version tag:

```bash
git tag v1.0.0
git push origin v1.0.0
```

---

## 📄 License

MIT License. See [LICENSE](LICENSE) for details.
