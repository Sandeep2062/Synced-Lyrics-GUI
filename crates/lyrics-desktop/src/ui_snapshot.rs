//! Headless UI snapshot harness — the app's visual regression gate.
//!
//! Renders the real Slint window with representative sample data using the
//! software renderer, writes a PNG per screen to `target/ui-snapshots/` (so a
//! human can look at what the test saw), and then *asserts* on the rendered
//! pixels via [`crate::ui_audit`].
//!
//! Because CI runs `cargo test --workspace`, every assertion here is a build
//! gate: an unthemed widget or a drifted layout fails the build instead of
//! shipping quietly.
//!
//! Run with: `cargo test -p lyrics-desktop ui_snapshot -- --nocapture`

#![allow(dead_code)]

use crate::ui_audit::{Frame, Island, Rect, Report, FAINT_LUMA};
use crate::{
    AlbumData, ArtistData, BatchLogEntry, BatchStats, DownloadStats, HealthResultLog,
    HealthTrackRow, LogEntry, LyricLine, MainWindow, PreviewDiffItem, ProviderStatus, TrackData,
};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 820;
const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;

thread_local! {
    static WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}

fn window() -> Rc<MinimalSoftwareWindow> {
    WINDOW.with(|w| w.borrow().clone().expect("window not initialized"))
}

/// Virtual "time since start" so animation frames can be sampled deterministically.
static NOW_MS: AtomicU64 = AtomicU64::new(0);

struct SnapshotPlatform;

impl Platform for SnapshotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(window())
    }

    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(NOW_MS.load(Ordering::Relaxed))
    }
}

/// Advances the virtual clock so running animations reach (or approach) their
/// target value before the next capture.
fn settle(ms: u64) {
    NOW_MS.fetch_add(ms, Ordering::Relaxed);
    for _ in 0..4 {
        slint::platform::update_timers_and_animations();
    }
}

/// Renders the window, writes the PNG for human inspection, audits the frame for
/// unthemed widgets, and hands the frame back for layout assertions.
fn capture_sized(name: &str, width: u32, height: u32, report: &mut Report) -> Frame {
    let window = window();
    window.set_size(slint::PhysicalSize::new(width, height));
    window.dispatch_event(slint::platform::WindowEvent::Resized {
        size: slint::LogicalSize::new(width as f32, height as f32),
    });
    window.request_redraw();
    settle(300);
    slint::platform::update_timers_and_animations();
    let opaque_black = PremultipliedRgbaColor {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };
    let mut buffer = vec![opaque_black; (width * height) as usize];

    slint::platform::update_timers_and_animations();
    let rendered = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, width as usize);
    });
    assert!(rendered, "{name}: window had no dirty region to render");

    let mut img = image::RgbaImage::new(width, height);
    for (i, px) in buffer.iter().enumerate() {
        img.put_pixel(
            (i as u32) % width,
            (i as u32) / width,
            image::Rgba([px.red, px.green, px.blue, 255]),
        );
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&dir).expect("create snapshot dir");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("write snapshot");
    println!("wrote {}", path.display());

    let frame = Frame::new(&buffer, width as usize, height as usize);

    // Every screen, in every state, must be drawn with the app's own theme. A
    // large uniform light-grey region means a platform-themed widget (LineEdit,
    // Slider, CheckBox, ...) leaked through — the defect class this catches.
    if !name.contains("light") {
        let blocks = frame.unthemed_blocks();
        report.require(
            blocks.is_empty(),
            format!(
                "[{name}] {} unthemed light block(s) — a platform-themed widget is bypassing the app theme: {}",
                blocks.len(),
                blocks
                    .iter()
                    .map(|block| block.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        );
    }

    frame
}

fn capture(name: &str, report: &mut Report) -> Frame {
    capture_sized(name, WIDTH, HEIGHT, report)
}

fn worst_case_tracks(count: usize) -> Vec<TrackData> {
    let templates: [(&str, &str, &str, &str, &str); 4] = [
        (
            // Exactly 90 chars: CJK + Emoji + Multilingual stress title
            "🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789",
            "Aimer (エメ) & International Symphony Orchestra featuring Koto & Sarangi Ensemble Multilingual 90",
            "Zankyo Sanka / Asa ga Kuru Deluxe Extended Edition with 100 Remastered Bonus Tracks Worldwide 90",
            "04:12",
            "synced",
        ),
        (
            // Exactly 90 chars: Devanagari + Emoji + Folk classical title
            "ॐ नमो भगवते वासुदेवाय - रेशम फिरिरी आधुनिक नेपाली साङ्गीतिक सिर्जना 🎧🚀 90-Character Title 123456789",
            "Narayan Gopal & Aruna Lama / Classical Nepali Folk Ensemble with Traditional Madal Instrument",
            "Geeti Yatra: Golden Memories of Nepali Classic Music - Complete 50th Anniversary Edition",
            "05:43",
            "plain",
        ),
        (
            // Exactly 90 chars: Classical European Long Movement
            "Supercalifragilisticexpialidocious Ultra Symphonic Movement No. 9 in D Minor 90-Chars 1234",
            "Ludwig van Beethoven conducted by Herbert von Karajan with Berlin Philharmonic Orchestra",
            "Beethoven: The Complete 9 Symphonies & Missa Solemnis Legendary Collector's Box Set 1977",
            "03:28",
            "missing",
        ),
        (
            // Exactly 90 chars: Japanese Pop / Light Novel Adaptation
            "🎉💥💖 夜に駆ける (Racing into the Night) YOASOBI Full Novel Music Extended Adaptation 90-Chars 123",
            "YOASOBI (Ayase & ikura) / Sony Music Entertainment Japan High-Resolution Audio Mastering",
            "THE BOOK (Limited Complete Production Edition with Special Binder and Lyrics Illustration)",
            "04:21",
            "suspicious",
        ),
    ];

    (0..count)
        .map(|idx| {
            let (t_title, t_artist, t_album, t_dur, t_status) = templates[idx % templates.len()];
            TrackData {
                path: SharedString::from(format!("C:\\Music\\{idx:05} - {t_artist} - {t_title}.mp3")),
                artist: SharedString::from(t_artist),
                title: SharedString::from(t_title),
                album: SharedString::from(t_album),
                duration: SharedString::from(t_dur),
                status: SharedString::from(t_status),
                has_lyrics: t_status != "missing",
                cover: if idx < COVERED_ROWS {
                    sample_cover()
                } else {
                    slint::Image::default()
                },
                has_cover: idx < COVERED_ROWS,
            }
        })
        .collect()
}

fn sample_tracks() -> Vec<TrackData> {
    worst_case_tracks(12)
}

/// Colour of the stand-in artwork. Deliberately unlike any theme colour, so
/// "is a cover painted here?" is a pixel test rather than a guess.
pub const COVER_TINT: [u8; 3] = [219, 39, 119];

/// How many rows of each list get stand-in artwork.
const COVERED_ROWS: usize = 8;

/// A solid-colour image standing in for embedded artwork.
fn sample_cover() -> slint::Image {
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(2, 2);
    for pixel in pixels.make_mut_slice() {
        *pixel = slint::Rgba8Pixel {
            r: COVER_TINT[0],
            g: COVER_TINT[1],
            b: COVER_TINT[2],
            a: 255,
        };
    }
    slint::Image::from_rgba8(pixels)
}

fn worst_case_albums() -> Vec<AlbumData> {
    let seeds = [
        (
            "🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 Deluxe Extended 90-Char Long Album Title Name 1234567890",
            "Aimer (エメ) & International Symphony Orchestra Multilingual 90",
            16,
            14,
        ),
        (
            "ॐ रेशम फिरिरी: नेपाली परम्परागत लोकभाका तथा आधुनिक धुन सङ्ग्रह स्वर्ण जयन्ती 90",
            "Narayan Gopal & Nepali Folk Ensemble with Traditional Madal Instrument",
            12,
            12,
        ),
        (
            "Supercalifragilisticexpialidocious Ultra Symphonic Movement No. 9 in D Minor 90-Chars 1234",
            "Ludwig van Beethoven / Herbert von Karajan Berlin Philharmonic",
            24,
            20,
        ),
        (
            "THE BOOK: Complete Novel-to-Music Audio Production Edition with Lyrics Collection 90-Chars",
            "YOASOBI (Ayase & ikura) / Sony Music Entertainment Japan Mastering",
            10,
            8,
        ),
    ];

    (0..24)
        .map(|idx| {
            let (name, artist, count, synced) = seeds[idx % seeds.len()];
            AlbumData {
                path: SharedString::from(format!("C:\\Music\\{artist}\\{name}\\01.mp3")),
                name: SharedString::from(name),
                artist: SharedString::from(artist),
                track_count: count,
                synced_count: synced,
                cover: if idx < COVERED_ROWS {
                    sample_cover()
                } else {
                    slint::Image::default()
                },
                has_cover: idx < COVERED_ROWS,
            }
        })
        .collect()
}

fn sample_albums() -> Vec<AlbumData> {
    worst_case_albums()
}

fn worst_case_artists() -> Vec<ArtistData> {
    let seeds = [
        (
            "🎵🔥 Aimer (エメ) & International Symphony Orchestra Multilingual Artist Name 90-Chars 1234",
            32,
            4,
            30,
        ),
        (
            "ॐ नारायण गोपाल (Narayan Gopal) - स्वर सम्राट तथा आधुनिक नेपाली साङ्गीतिक स्रष्टा 90",
            48,
            6,
            45,
        ),
        (
            "Ludwig van Beethoven & The Legendary Berlin Philharmonic Orchestra Under Karajan 90-Chars",
            120,
            15,
            110,
        ),
        (
            "YOASOBI (Ayase & ikura) / Sony Music Entertainment High-Resolution Master Audio 90-Chars",
            28,
            3,
            26,
        ),
    ];

    (0..10)
        .map(|idx| {
            let (name, tracks, albums, synced) = seeds[idx % seeds.len()];
            ArtistData {
                name: SharedString::from(name),
                track_count: tracks,
                album_count: albums,
                synced_count: synced,
            }
        })
        .collect()
}

fn sample_artists() -> Vec<ArtistData> {
    worst_case_artists()
}

fn worst_case_lyrics() -> Vec<LyricLine> {
    [
        "🎵 [00:00] 誰が袖に咲く幻花 ただそこに咲く幻花 殘響散歌 鬼滅の刃 遊郭編 90-Chars Title Line 1234",
        "ॐ [00:04] रेशम फिरिरी रेशम फिरिरी उडेर जाउँ कि डाँडामा भञ्ज्याङ रेशम फिरिरी आधुनिक नेपाली धुन",
        "★ [00:08] Supercalifragilisticexpialidocious Ultra Long Wrapped Symphonic Verse Lyric Line 90",
        "💖 [00:12] 沈むように溶けてゆくように 二人だけの空が広がる夜に 夜に駆ける YOASOBI Full Novel",
        "🚀 [00:16] Active Line: Multilingual High-Stress Lyric Line with Emoji 🎵🔥✨ and Devanagari ॐ",
        "🎶 [00:20] The rhythm flows through deep orchestral melodies and ancient Nepalese Sarangi songs",
        "🌟 [00:24] Continuing into the next movement of the composition with extended harmonious chords",
        "🔥 [00:28] Final cadence approaching with vibrant choral voices echoing across the stadium hall",
    ]
    .iter()
    .enumerate()
    .map(|(i, text)| LyricLine {
        timestamp: i as f32 * 4.0,
        text: SharedString::from(*text),
    })
    .collect()
}

fn sample_lyrics() -> Vec<LyricLine> {
    worst_case_lyrics()
}

fn worst_case_health_issues() -> Vec<HealthTrackRow> {
    vec![
        HealthTrackRow {
            path: SharedString::from("C:\\Music\\01.mp3"),
            title: SharedString::from("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789"),
            artist: SharedString::from("Aimer (エメ) & International Symphony Orchestra Multilingual 90"),
            album: SharedString::from("Zankyo Sanka Deluxe Extended Edition with 100 Bonus Tracks 90"),
            duration: SharedString::from("04:12"),
            status: SharedString::from("suspicious"),
            category: SharedString::from("wrong_song"),
            category_label: SharedString::from("Wrong Song"),
            reason: SharedString::from("Title mismatch: Tag '殘響散歌' vs LRCLib 'Zankyou Sanka' (Score 72)"),
            is_safe: true,
            suggested_offset_ms: 0,
        },
        HealthTrackRow {
            path: SharedString::from("C:\\Music\\02.mp3"),
            title: SharedString::from("ॐ नमो भगवते वासुदेवाय - रेशम फिरिरी आधुनिक नेपाली साङ्गीतिक सिर्जना 🎧🚀 90-Character Title 123456789"),
            artist: SharedString::from("Narayan Gopal & Classical Nepali Folk Ensemble"),
            album: SharedString::from("Geeti Yatra: Golden Memories Complete 50th Edition"),
            duration: SharedString::from("05:43"),
            status: SharedString::from("suspicious"),
            category: SharedString::from("offset"),
            category_label: SharedString::from("Offset Discrepancy"),
            reason: SharedString::from("LRC timestamps lead audio by ~1450ms across first 8 lyric lines"),
            is_safe: true,
            suggested_offset_ms: 1450,
        },
    ]
}

fn worst_case_health_logs() -> Vec<HealthResultLog> {
    vec![
        HealthResultLog {
            timestamp: SharedString::from("14:22:05"),
            title: SharedString::from("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789"),
            artist: SharedString::from("Aimer (エメ)"),
            provider: SharedString::from("LRCLib"),
            score_text: SharedString::from("98%"),
            selection_reason: SharedString::from("Exact matched duration (04:12) with high-confidence synced timestamp coverage"),
            status_text: SharedString::from("FIXED (SYNCED)"),
            status_type: SharedString::from("synced"),
        },
        HealthResultLog {
            timestamp: SharedString::from("14:22:08"),
            title: SharedString::from("ॐ नमो भगवते वासुदेवाय - रेशम फिरिरी 🎧🚀 90-Character Multilingual Extreme Stress Test Title 123456789"),
            artist: SharedString::from("Narayan Gopal"),
            provider: SharedString::from("Musixmatch"),
            score_text: SharedString::from("91%"),
            selection_reason: SharedString::from("Verified audio fingerprint and matching verse structure"),
            status_text: SharedString::from("FIXED (PLAIN)"),
            status_type: SharedString::from("plain"),
        },
    ]
}

fn worst_case_diff_items() -> Vec<PreviewDiffItem> {
    vec![
        PreviewDiffItem {
            path: SharedString::from("C:\\Music\\01.mp3"),
            title: SharedString::from("🎵🔥 殘響散歌 / 鬼滅の刃 90-Character Multilingual Title 123456789"),
            artist: SharedString::from("Aimer (エメ)"),
            issue_category: SharedString::from("Wrong Song"),
            action_label: SharedString::from("Replace with Verified Synced Lyrics"),
            provider: SharedString::from("LRCLib (Score 98%)"),
            score_text: SharedString::from("98%"),
            selection_reason: SharedString::from("Exact duration and metadata alignment"),
            diff_text: SharedString::from("--- Existing Lyrics (Mismatch)\n+++ Proposed Lyrics (LRCLib)\n@@ -1,5 +1,5 @@\n-[00:01.00] Wrong intro line\n+[00:01.20] 誰が袖に咲く幻花\n-[00:05.00] Incorrect chorus\n+[00:04.85] ただそこに咲く幻花\n"),
        }
    ]
}

fn worst_case_providers() -> Vec<ProviderStatus> {
    vec![
        ProviderStatus { name: SharedString::from("LRCLib"), status: SharedString::from("searching") },
        ProviderStatus { name: SharedString::from("Musixmatch"), status: SharedString::from("found") },
        ProviderStatus { name: SharedString::from("NetEase"), status: SharedString::from("searching") },
        ProviderStatus { name: SharedString::from("Megalobiz"), status: SharedString::from("not-found") },
        ProviderStatus { name: SharedString::from("Genius"), status: SharedString::from("rate-limited") },
    ]
}

fn worst_case_download_logs() -> Vec<SharedString> {
    vec![
        SharedString::from("[14:22:01] 🎵 Starting batch download for 10,000 tracks across 3 directories..."),
        SharedString::from("[14:22:03] ✓ LRCLib: Found synced lyrics for '🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 90-Character Multilingual Title'"),
        SharedString::from("[14:22:06] ⚠ Megalobiz: 429 Too Many Requests — backoff initiated"),
        SharedString::from("[14:22:09] ✓ Musixmatch: Token authenticated, downloaded lyrics for 'ॐ नमो भगवते वासुदेवाय'"),
    ]
}

fn worst_case_logs_entries() -> Vec<LogEntry> {
    vec![
        LogEntry {
            status: SharedString::from("Error"),
            track_info: SharedString::from("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789"),
            details: SharedString::from("Failed to retrieve lyrics from provider: RateLimit 429 on lrclib.net with 10 concurrent requests"),
        },
        LogEntry {
            status: SharedString::from("Synced"),
            track_info: SharedString::from("ॐ नमो भगवते वासुदेवाय - रेशम फिरिरी आधुनिक नेपाली साङ्गीतिक सिर्जना 🎧🚀 90-Character Title 123456789"),
            details: SharedString::from("Successfully downloaded and embedded synchronized lyrics from Musixmatch API token"),
        },
    ]
}

fn worst_case_directories() -> Vec<SharedString> {
    vec![
        SharedString::from("C:\\Music\\🎵 Library\\日本語フォルダー\\नेपाली सङ्ग्रह\\Supercalifragilisticexpialidocious Ultra Long Directory Path 90"),
        SharedString::from("D:\\Audio\\Lossless\\High-Resolution 24bit 192kHz FLAC Master Collection With Deeply Nested Subdirectories 90"),
    ]
}

fn worst_case_batch_logs() -> Vec<BatchLogEntry> {
    vec![
        BatchLogEntry {
            timestamp: SharedString::from("14:22:05"),
            track_title: SharedString::from("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789"),
            track_artist: SharedString::from("Aimer (エメ)"),
            details: SharedString::from("✓ LRCLib (synced)  •  ✓ Musixmatch (synced)"),
            action_text: SharedString::from("→ Saved synced lyrics from LRCLib"),
            badge_text: SharedString::from("SYNCED"),
            badge_type: SharedString::from("synced"),
        },
        BatchLogEntry {
            timestamp: SharedString::from("14:22:07"),
            track_title: SharedString::from("ॐ नमो भगवते वासुदेवाय - रेशम फिरिरी आधुनिक नेपाली साङ्गीतिक सिर्जना 🎧🚀 90-Character Title 123456789"),
            track_artist: SharedString::from("Narayan Gopal"),
            details: SharedString::from("✗ LRCLib  •  ✓ Genius (plain)"),
            action_text: SharedString::from("→ Saved plain lyrics from Genius"),
            badge_text: SharedString::from("PLAIN"),
            badge_type: SharedString::from("plain"),
        },
    ]
}

/// Boots the headless platform, fills the window with sample data, and lets the
/// caller capture screenshots. Only call this once per test binary.
fn with_app(time_ms: u64, f: impl FnOnce(&MainWindow)) {
    NOW_MS.store(time_ms, Ordering::Relaxed);
    WINDOW.with(|w| {
        *w.borrow_mut() = Some(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer));
    });
    slint::platform::set_platform(Box::new(SnapshotPlatform))
        .expect("set software-renderer platform");

    let ui = MainWindow::new().expect("create MainWindow");
    ui.set_tracks(ModelRc::new(VecModel::from(worst_case_tracks(10_000))));
    ui.set_albums(ModelRc::new(VecModel::from(worst_case_albums())));
    ui.set_album_rows(ModelRc::new(VecModel::from(crate::album_rows(
        worst_case_albums().len(),
    ))));
    ui.set_artists(ModelRc::new(VecModel::from(worst_case_artists())));
    ui.set_tracks_summary("10000 tracks | 5000 synced | 2000 plain | 3000 missing".into());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_settings_music_dirs(ModelRc::new(VecModel::from(worst_case_directories())));
    ui.set_settings_musixmatch_key("9f3a1c77b204e5d8".into());
    ui.set_settings_enable_lrclib(true);
    ui.set_settings_enable_musixmatch(true);
    ui.set_settings_enable_netease(true);
    ui.set_settings_cache_period_idx(1);
    ui.set_lyrics_lines(ModelRc::new(VecModel::from(worst_case_lyrics())));
    ui.set_active_line_index(4);
    ui.set_lyrics_status("synced".into());
    ui.set_track_title("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789".into());
    ui.set_track_subtitle("Aimer (エメ) & International Symphony Orchestra featuring Koto & Sarangi Ensemble Multilingual 90".into());
    ui.set_track_album("Zankyo Sanka / Asa ga Kuru Deluxe Extended Edition with 100 Remastered Bonus Tracks Worldwide 90".into());
    ui.set_has_track(true);
    ui.set_has_lyrics(true);
    ui.set_is_playing(true);
    ui.set_position_text("1:24".into());
    ui.set_duration_text("4:17".into());
    ui.set_seek_position(0.33);
    ui.set_volume(0.7);
    ui.set_waveform_data(ModelRc::new(VecModel::from(
        (0..crate::WAVEFORM_BARS)
            .map(|i| 0.2 + 0.7 * ((i as f32 * 0.2).sin().abs()))
            .collect::<Vec<f32>>(),
    )));

    ui.show().expect("show window");
    window().set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    window().dispatch_event(slint::platform::WindowEvent::Resized {
        size: slint::LogicalSize::new(WIDTH as f32, HEIGHT as f32),
    });
    f(&ui);
}

fn audit_frame(frame: &Frame, name: &str, report: &mut Report) {
    let w = frame.width();
    let h = frame.height();

    // 1. Unthemed platform widget leaks
    if !name.contains("light") {
        let blocks = frame.unthemed_blocks();
        report.require(
            blocks.is_empty(),
            format!(
                "[{name}] {} unthemed light block(s) — a platform-themed widget is bypassing the app theme: {}",
                blocks.len(),
                blocks
                    .iter()
                    .map(|block| block.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        );
    }

    // 2. Window boundary safety (clipping without elide)
    let window_rect = Rect::new(0, w, 0, h);
    frame.check_not_clipped_at_boundary(window_rect, &format!("[{name}] window boundary"), report);

    // 3. PlayerBar layout (when player bar is visible and full now-playing stage is not)
    if !name.contains("mini") && !name.contains("now-playing") {
        let player_top = h.saturating_sub(PLAYER_HEIGHT);
        // Player bar left column (0..320) vs center transport
        let left_col = Rect::new(0, 320, player_top, h);
        let center_col = Rect::new(340, w.saturating_sub(300), player_top, h);
        frame.check_regions_no_overlap(
            left_col,
            center_col,
            10,
            &format!("[{name}] PlayerBar left vs center transport"),
            report,
        );
        // Center transport vs right actions
        let right_col = Rect::new(w.saturating_sub(290), w, player_top, h);
        frame.check_regions_no_overlap(
            center_col,
            right_col,
            10,
            &format!("[{name}] PlayerBar center vs right actions"),
            report,
        );
    }

    // 4. Header regions (when header tabs 0..4 are visible and not occluded by full modals)
    if (name.starts_with("view-01")
        || name.starts_with("view-02")
        || name.starts_with("view-03")
        || name.starts_with("view-04")
        || name.starts_with("view-05"))
        && !name.contains("now-playing")
        && !name.contains("mini")
        && !name.contains("directories")
        && !name.contains("batch")
    {
        check_header_regions_no_overlap(frame, name, report);
    }
}

const SIZES: [(u32, u32); 3] = [(960, 600), (1280, 820), (1920, 1080)];

#[test]
fn ui_snapshot() {
    let mut report = Report::default();

    with_app(5_000, |ui| {
        for &(width, height) in &SIZES {
            let w = width as usize;
            let h = height as usize;

            // ---------------------------------------------------------------
            // View 1: Tracks (10,000 tracks stress test & worst-case titles)
            // ---------------------------------------------------------------
            ui.set_active_tab(0);
            ui.set_lyrics_visible(false);
            ui.set_queue_visible(false);
            ui.set_now_playing_visible(false);
            ui.set_directories_open(false);
            ui.set_batch_download_open(false);
            ui.set_mini_player_mode(false);
            ui.set_tracks(ModelRc::new(VecModel::from(worst_case_tracks(10_000))));
            ui.set_tracks_summary("10000 tracks | 5000 synced | 2000 plain | 3000 missing".into());
            let name_tracks = format!("view-01-tracks-{width}");
            let f_tracks = capture_sized(&name_tracks, width, height, &mut report);
            audit_frame(&f_tracks, &name_tracks, &mut report);
            if width == 1280 {
                check_tracks_columns_line_up(&f_tracks, &mut report);
                check_player_bar_is_centred(&f_tracks, &mut report);
                check_status_markers_are_centred(&f_tracks, &mut report);
                check_header_action_icons_are_centred(&f_tracks, &mut report);
            }
            // Check that row title column does not overlap status column
            let row_y0 = 140;
            let row_y1 = 200.min(h - PLAYER_HEIGHT);
            let col1 = Rect::new(0, w - 310, row_y0, row_y1);
            let col2 = Rect::new(w - 305, w - 180, row_y0, row_y1);
            f_tracks.check_regions_no_overlap(col1, col2, 4, &format!("[{name_tracks}] tracks title vs status column"), &mut report);

            // View 1b: Tracks Empty
            ui.set_tracks(ModelRc::new(VecModel::from(Vec::<TrackData>::new())));
            ui.set_tracks_summary(SharedString::new());
            let name_tracks_empty = format!("view-01b-tracks-empty-{width}");
            let f_tracks_empty = capture_sized(&name_tracks_empty, width, height, &mut report);
            audit_frame(&f_tracks_empty, &name_tracks_empty, &mut report);
            let empty_y0 = 260;
            let empty_y1 = h.saturating_sub(100);
            check_region_centred(&f_tracks_empty, &mut report, &name_tracks_empty, 0, w, empty_y0, empty_y1);
            // Restore 10k tracks
            ui.set_tracks(ModelRc::new(VecModel::from(worst_case_tracks(10_000))));
            ui.set_tracks_summary("10000 tracks | 5000 synced | 2000 plain | 3000 missing".into());

            // ---------------------------------------------------------------
            // View 2: Albums (worst-case titles & CJK/Devanagari)
            // ---------------------------------------------------------------
            ui.set_active_tab(1);
            ui.set_album_detail_mode(false);
            ui.set_albums(ModelRc::new(VecModel::from(worst_case_albums())));
            ui.set_album_rows(ModelRc::new(VecModel::from(crate::album_rows(24))));
            let name_albums = format!("view-02-albums-{width}");
            let f_albums = capture_sized(&name_albums, width, height, &mut report);
            audit_frame(&f_albums, &name_albums, &mut report);
            if width == 1280 {
                check_album_grid(&f_albums, &mut report);
            }

            // View 2b: Albums Empty
            ui.set_albums(ModelRc::new(VecModel::from(Vec::<AlbumData>::new())));
            ui.set_album_rows(ModelRc::new(VecModel::from(Vec::<i32>::new())));
            let name_albums_empty = format!("view-02b-albums-empty-{width}");
            let f_albums_empty = capture_sized(&name_albums_empty, width, height, &mut report);
            audit_frame(&f_albums_empty, &name_albums_empty, &mut report);
            let empty_y0 = 260;
            let empty_y1 = h.saturating_sub(100);
            check_region_centred(&f_albums_empty, &mut report, &name_albums_empty, 0, w, empty_y0, empty_y1);
            ui.set_albums(ModelRc::new(VecModel::from(worst_case_albums())));
            ui.set_album_rows(ModelRc::new(VecModel::from(crate::album_rows(24))));

            // View 2c: Albums Detail
            ui.set_album_detail_mode(true);
            ui.set_selected_album_index(0);
            ui.set_album_tracks(ModelRc::new(VecModel::from(worst_case_tracks(12))));
            let name_album_detail = format!("view-02c-albums-detail-{width}");
            let f_album_detail = capture_sized(&name_album_detail, width, height, &mut report);
            audit_frame(&f_album_detail, &name_album_detail, &mut report);
            ui.set_album_detail_mode(false);

            // ---------------------------------------------------------------
            // View 3: Artists (worst-case names & multilingual text)
            // ---------------------------------------------------------------
            ui.set_active_tab(2);
            ui.set_selected_artist_index(0);
            ui.set_artists(ModelRc::new(VecModel::from(worst_case_artists())));
            ui.set_artist_tracks(ModelRc::new(VecModel::from(worst_case_tracks(12))));
            let name_artists = format!("view-03-artists-{width}");
            let f_artists = capture_sized(&name_artists, width, height, &mut report);
            audit_frame(&f_artists, &name_artists, &mut report);

            // View 3b: Artists Empty
            ui.set_selected_artist_index(-1);
            ui.set_artists(ModelRc::new(VecModel::from(Vec::<ArtistData>::new())));
            ui.set_artist_tracks(ModelRc::new(VecModel::from(Vec::<TrackData>::new())));
            let name_artists_empty = format!("view-03b-artists-empty-{width}");
            let f_artists_empty = capture_sized(&name_artists_empty, width, height, &mut report);
            audit_frame(&f_artists_empty, &name_artists_empty, &mut report);
            ui.set_artists(ModelRc::new(VecModel::from(worst_case_artists())));
            ui.set_selected_artist_index(0);
            ui.set_artist_tracks(ModelRc::new(VecModel::from(worst_case_tracks(12))));

            // ---------------------------------------------------------------
            // View 4: Health (Issues tab, Result Log tab, Diff Modal, Empty)
            // ---------------------------------------------------------------
            ui.set_active_tab(3);
            ui.set_health_active_view_tab(0);
            ui.set_health_total_count(10000);
            ui.set_health_synced_count(5000);
            ui.set_health_missing_count(3000);
            ui.set_health_suspicious_count(450);
            ui.set_health_plain_count(2000);
            ui.set_health_safe_issues_count(345);
            ui.set_health_issues(ModelRc::new(VecModel::from(worst_case_health_issues())));
            let name_health_issues = format!("view-04-health-issues-{width}");
            let f_health_issues = capture_sized(&name_health_issues, width, height, &mut report);
            audit_frame(&f_health_issues, &name_health_issues, &mut report);

            // View 4b: Health Result Logs
            ui.set_health_active_view_tab(1);
            ui.set_health_result_logs(ModelRc::new(VecModel::from(worst_case_health_logs())));
            let name_health_logs = format!("view-04b-health-logs-{width}");
            let f_health_logs = capture_sized(&name_health_logs, width, height, &mut report);
            audit_frame(&f_health_logs, &name_health_logs, &mut report);
            ui.set_health_active_view_tab(0);

            // View 4c: Health Diff Preview Modal
            ui.set_health_preview_modal_open(true);
            ui.set_health_preview_items(ModelRc::new(VecModel::from(worst_case_diff_items())));
            ui.set_health_preview_selected_index(0);
            let name_health_diff = format!("view-04c-health-diff-{width}");
            let f_health_diff = capture_sized(&name_health_diff, width, height, &mut report);
            audit_frame(&f_health_diff, &name_health_diff, &mut report);
            let modal_w = 840.min(w - 32);
            let modal_x0 = (w - modal_w) / 2;
            let modal_x1 = modal_x0 + modal_w;
            let view_h = h.saturating_sub(HEADER_HEIGHT + PLAYER_HEIGHT);
            let modal_h = 620.min(view_h.saturating_sub(32));
            let modal_y0 = HEADER_HEIGHT + (view_h.saturating_sub(modal_h)) / 2;
            let modal_y1 = modal_y0 + modal_h;
            f_health_diff.check_text_outside_container(Rect::new(modal_x0, modal_x1, modal_y0, modal_y1), 20, &format!("[{name_health_diff}] health diff modal bounds"), &mut report);
            ui.set_health_preview_modal_open(false);

            // View 4d: Health Empty
            ui.set_health_issues(ModelRc::new(VecModel::from(Vec::<HealthTrackRow>::new())));
            let name_health_empty = format!("view-04d-health-empty-{width}");
            let f_health_empty = capture_sized(&name_health_empty, width, height, &mut report);
            audit_frame(&f_health_empty, &name_health_empty, &mut report);
            ui.set_health_issues(ModelRc::new(VecModel::from(worst_case_health_issues())));

            // ---------------------------------------------------------------
            // View 5: Settings
            // ---------------------------------------------------------------
            ui.set_active_tab(4);
            let name_settings = format!("view-05-settings-{width}");
            let f_settings = capture_sized(&name_settings, width, height, &mut report);
            audit_frame(&f_settings, &name_settings, &mut report);

            // ---------------------------------------------------------------
            // View 6: Downloads
            // ---------------------------------------------------------------
            ui.set_active_tab(5);
            ui.set_downloads_progress(0.65);
            ui.set_downloads_progress_text("6500 / 10000 (65%)".into());
            ui.set_downloads_eta_text("ETA: 08:32".into());
            ui.set_downloads_current_track("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789".into());
            ui.set_downloads_providers(ModelRc::new(VecModel::from(worst_case_providers())));
            ui.set_downloads_logs(ModelRc::new(VecModel::from(worst_case_download_logs())));
            ui.set_downloads_stats(DownloadStats {
                synced: 5000,
                plain: 1200,
                kept: 300,
                not_found: 10,
                errors: 5,
            });
            ui.set_downloads_is_active(true);
            let name_downloads = format!("view-06-downloads-{width}");
            let f_downloads = capture_sized(&name_downloads, width, height, &mut report);
            audit_frame(&f_downloads, &name_downloads, &mut report);

            // View 6b: Downloads Empty
            ui.set_downloads_logs(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
            ui.set_downloads_is_active(false);
            ui.set_downloads_current_track(SharedString::new());
            let name_downloads_empty = format!("view-06b-downloads-empty-{width}");
            let f_downloads_empty = capture_sized(&name_downloads_empty, width, height, &mut report);
            audit_frame(&f_downloads_empty, &name_downloads_empty, &mut report);
            ui.set_downloads_logs(ModelRc::new(VecModel::from(worst_case_download_logs())));

            // ---------------------------------------------------------------
            // View 7: Logs
            // ---------------------------------------------------------------
            ui.set_active_tab(6);
            ui.set_logs_total_count(10000);
            ui.set_logs_missing_count(3000);
            ui.set_logs_suspicious_count(450);
            ui.set_logs_synced_count(5000);
            ui.set_logs_entries(ModelRc::new(VecModel::from(worst_case_logs_entries())));
            let name_logs = format!("view-07-logs-{width}");
            let f_logs = capture_sized(&name_logs, width, height, &mut report);
            audit_frame(&f_logs, &name_logs, &mut report);

            // View 7b: Logs Empty
            ui.set_logs_entries(ModelRc::new(VecModel::from(Vec::<LogEntry>::new())));
            let name_logs_empty = format!("view-07b-logs-empty-{width}");
            let f_logs_empty = capture_sized(&name_logs_empty, width, height, &mut report);
            audit_frame(&f_logs_empty, &name_logs_empty, &mut report);
            ui.set_logs_entries(ModelRc::new(VecModel::from(worst_case_logs_entries())));

            // ---------------------------------------------------------------
            // View 8: Now Playing Fullscreen Stage Overlay
            // ---------------------------------------------------------------
            ui.set_active_tab(0);
            ui.set_now_playing_visible(true);
            let name_now_playing = format!("view-08-now-playing-{width}");
            let f_now_playing = capture_sized(&name_now_playing, width, height, &mut report);
            audit_frame(&f_now_playing, &name_now_playing, &mut report);
            // Left column (28..408) vs right lyrics column (430..w-28)
            let left_np = Rect::new(28, 408, 60, h.saturating_sub(30));
            let lyrics_np = Rect::new(430, w.saturating_sub(28), 60, h.saturating_sub(30));
            f_now_playing.check_regions_no_overlap(left_np, lyrics_np, 10, &format!("[{name_now_playing}] now-playing left vs lyrics"), &mut report);
            ui.set_now_playing_visible(false);

            // ---------------------------------------------------------------
            // View 9: Queue Drawer
            // ---------------------------------------------------------------
            ui.set_queue_visible(true);
            ui.set_queue_tracks(ModelRc::new(VecModel::from(worst_case_tracks(12))));
            ui.set_current_queue_index(0);
            let name_queue = format!("view-09-queue-drawer-{width}");
            let f_queue = capture_sized(&name_queue, width, height, &mut report);
            audit_frame(&f_queue, &name_queue, &mut report);
            let queue_rect = Rect::new(w.saturating_sub(340), w, 0, h);
            f_queue.check_not_clipped_at_boundary(queue_rect, &format!("[{name_queue}] queue drawer right boundary"), &mut report);
            let q_item_y0 = 150;
            let q_item_y1 = 200;
            let q_title_rect = Rect::new(w.saturating_sub(340) + 50, w.saturating_sub(70), q_item_y0, q_item_y1);
            let q_dur_rect = Rect::new(w.saturating_sub(60), w.saturating_sub(10), q_item_y0, q_item_y1);
            f_queue.check_regions_no_overlap(q_title_rect, q_dur_rect, 4, &format!("[{name_queue}] queue item title vs duration"), &mut report);

            // View 9b: Queue Drawer Empty
            ui.set_queue_tracks(ModelRc::new(VecModel::from(Vec::<TrackData>::new())));
            let name_queue_empty = format!("view-09b-queue-empty-{width}");
            let f_queue_empty = capture_sized(&name_queue_empty, width, height, &mut report);
            audit_frame(&f_queue_empty, &name_queue_empty, &mut report);
            ui.set_queue_visible(false);

            // ---------------------------------------------------------------
            // View 10: Lyrics Drawers (Synced, Plain, None, Suggest, Suspicious)
            // ---------------------------------------------------------------
            ui.set_lyrics_visible(true);
            ui.set_lyrics_status("synced".into());
            ui.set_lyrics_lines(ModelRc::new(VecModel::from(worst_case_lyrics())));
            ui.set_active_line_index(4);
            let name_lyrics_synced = format!("view-10-lyrics-synced-{width}");
            let f_lyrics_synced = capture_sized(&name_lyrics_synced, width, height, &mut report);
            audit_frame(&f_lyrics_synced, &name_lyrics_synced, &mut report);
            let lyrics_rect = Rect::new(w.saturating_sub(390), w, 0, h);
            f_lyrics_synced.check_not_clipped_at_boundary(lyrics_rect, &format!("[{name_lyrics_synced}] lyrics panel right boundary"), &mut report);

            // View 10b: Lyrics Plain Text
            ui.set_lyrics_status("plain".into());
            ui.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
            ui.set_lyrics_plain_text("🎵 誰が袖に咲く幻花 ただそこに咲く幻花 殘響散歌\nॐ रेशम फिरिरी रेशम फिरिरी उडेर जाउँ कि डाँडामा भञ्ज्याङ\nSupercalifragilisticexpialidocious Ultra Long Wrapped Plain Lyric Text That Must Not Clip\n沈むように溶けてゆくように 二人だけの空が広がる夜に".into());
            let name_lyrics_plain = format!("view-10b-lyrics-plain-{width}");
            let f_lyrics_plain = capture_sized(&name_lyrics_plain, width, height, &mut report);
            audit_frame(&f_lyrics_plain, &name_lyrics_plain, &mut report);

            // View 10c: Lyrics None
            ui.set_lyrics_status("missing".into());
            ui.set_lyrics_plain_text(SharedString::new());
            let name_lyrics_none = format!("view-10c-lyrics-none-{width}");
            let f_lyrics_none = capture_sized(&name_lyrics_none, width, height, &mut report);
            audit_frame(&f_lyrics_none, &name_lyrics_none, &mut report);

            // View 10d: Lyrics Identification Suggestion
            ui.set_lyrics_status("missing".into());
            ui.set_lyrics_has_identification_suggestion(true);
            ui.set_lyrics_suggested_title("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789".into());
            ui.set_lyrics_suggested_artist("Aimer (エメ) & International Symphony Orchestra Multilingual 90".into());
            ui.set_lyrics_suggested_album("Zankyo Sanka Deluxe Extended Edition with 100 Bonus Tracks 90".into());
            ui.set_lyrics_suggestion_confidence(98);
            ui.set_lyrics_suggestion_reason("Exact acoustic fingerprint match from audio characteristics".into());
            let name_lyrics_suggest = format!("view-10d-lyrics-suggest-{width}");
            let f_lyrics_suggest = capture_sized(&name_lyrics_suggest, width, height, &mut report);
            audit_frame(&f_lyrics_suggest, &name_lyrics_suggest, &mut report);
            ui.set_lyrics_has_identification_suggestion(false);

            // View 10e: Lyrics Suspicious
            ui.set_lyrics_status("suspicious".into());
            ui.set_lyrics_is_suspicious(true);
            ui.set_lyrics_suspicious_reason("LRC timestamps lead audio by ~1450ms across first 8 lyric lines with 90-char title discrepancy".into());
            ui.set_lyrics_has_offset(true);
            ui.set_lyrics_estimated_offset(-1.45);
            let name_lyrics_suspicious = format!("view-10e-lyrics-suspicious-{width}");
            let f_lyrics_suspicious = capture_sized(&name_lyrics_suspicious, width, height, &mut report);
            audit_frame(&f_lyrics_suspicious, &name_lyrics_suspicious, &mut report);
            ui.set_lyrics_is_suspicious(false);
            ui.set_lyrics_has_offset(false);
            ui.set_lyrics_visible(false);

            // ---------------------------------------------------------------
            // View 11: Manage Directories Dialog
            // ---------------------------------------------------------------
            ui.set_directories_open(true);
            ui.set_settings_music_dirs(ModelRc::new(VecModel::from(worst_case_directories())));
            let name_directories = format!("view-11-manage-directories-{width}");
            let f_directories = capture_sized(&name_directories, width, height, &mut report);
            audit_frame(&f_directories, &name_directories, &mut report);
            let dir_card_w = 540;
            let dir_cx0 = (w.saturating_sub(dir_card_w)) / 2;
            let dir_cx1 = dir_cx0 + dir_card_w;
            let dir_card_h = 480.min(h.saturating_sub(72));
            let dir_cy0 = (h.saturating_sub(dir_card_h)) / 2;
            let dir_cy1 = dir_cy0 + dir_card_h;
            f_directories.check_text_outside_container(Rect::new(dir_cx0, dir_cx1, dir_cy0, dir_cy1), 30, &format!("[{name_directories}] directories dialog card"), &mut report);

            // View 11b: Manage Directories Empty
            ui.set_settings_music_dirs(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
            let name_directories_empty = format!("view-11b-manage-directories-empty-{width}");
            let f_directories_empty = capture_sized(&name_directories_empty, width, height, &mut report);
            audit_frame(&f_directories_empty, &name_directories_empty, &mut report);
            ui.set_directories_open(false);

            // ---------------------------------------------------------------
            // View 12: Batch Download Dialog (Stage 0 & Stage 1)
            // ---------------------------------------------------------------
            ui.set_batch_download_open(true);
            ui.set_batch_download_stage(0);
            ui.set_batch_total_count(10000);
            ui.set_batch_missing_count(3000);
            ui.set_batch_plain_count(2000);
            ui.set_batch_suspicious_count(450);
            let name_batch_stage0 = format!("view-12-batch-stage0-{width}");
            let f_batch_stage0 = capture_sized(&name_batch_stage0, width, height, &mut report);
            audit_frame(&f_batch_stage0, &name_batch_stage0, &mut report);
            let batch_card_w = 720.min(w.saturating_sub(32));
            let batch_cx0 = (w.saturating_sub(batch_card_w)) / 2;
            let batch_cx1 = batch_cx0 + batch_card_w;
            let batch_card_h = 620.min(h.saturating_sub(48));
            let batch_cy0 = (h.saturating_sub(batch_card_h)) / 2;
            let batch_cy1 = batch_cy0 + batch_card_h;
            f_batch_stage0.check_text_outside_container(Rect::new(batch_cx0, batch_cx1, batch_cy0, batch_cy1), 20, &format!("[{name_batch_stage0}] batch dialog card stage 0"), &mut report);

            // View 12b: Batch Download Dialog Stage 1
            ui.set_batch_download_stage(1);
            ui.set_batch_mode_title("Download Only for Missing (3000 Tracks)".into());
            ui.set_batch_phase_text("Downloading track 6500 of 10000...".into());
            ui.set_batch_current_track("🎵🔥 殘響散歌 / 鬼滅の刃 遊郭編 音楽鑑賞用超長尺曲名テスト 90-Character Multilingual Extreme Stress Test Title 123456789".into());
            ui.set_batch_progress(0.65);
            ui.set_batch_progress_text("6500 / 10000 (65%)".into());
            ui.set_batch_stats(BatchStats {
                total: 10000,
                processed: 6500,
                remaining: 3500,
                synced: 5000,
                plain: 1200,
                not_found: 200,
                errors: 100,
            });
            ui.set_batch_logs(ModelRc::new(VecModel::from(worst_case_batch_logs())));
            ui.set_batch_is_running(true);
            let name_batch_stage1 = format!("view-12b-batch-stage1-{width}");
            let f_batch_stage1 = capture_sized(&name_batch_stage1, width, height, &mut report);
            audit_frame(&f_batch_stage1, &name_batch_stage1, &mut report);
            f_batch_stage1.check_text_outside_container(Rect::new(batch_cx0, batch_cx1, 0, h), 20, &format!("[{name_batch_stage1}] batch dialog card stage 1"), &mut report);
            ui.set_batch_download_open(false);
        }
    });

    report.finish();
}

// ---------------------------------------------------------------------------
// Layout invariants
//
// The bugs that actually shipped were geometric, not logical: a fixed-size
// child of a layout is start-aligned, and a stretch spacer collapses under
// `alignment: start`. Neither is visible to a type checker, so they are pinned
// here against the real rendered pixels.
// ---------------------------------------------------------------------------

/// Mirrors `Theme.header-height` and `Theme.player-height`.
const HEADER_HEIGHT: usize = 54;
const PLAYER_HEIGHT: usize = 92;

/// Mirrors the tab geometry declared in `header.slint`.
const TAB_WIDTH: f64 = 96.0;
const TAB_GAP: f64 = 6.0;
const TAB_COUNT: f64 = 5.0;
/// The indicator is `tab-width - 40px` wide.
const TAB_INDICATOR_WIDTH: usize = 56;

const TAB_WIDTH_SHRUNK: f64 = 78.0;
const TAB_GAP_SHRUNK: f64 = 5.0;
const TAB_INDICATOR_WIDTH_SHRUNK: usize = 38;

/// Fixed-size elements are placed by the layout engine, so they must be exact.
/// Text is deliberately not measured this precisely: its width depends on the
/// font stack of the machine running the test.
const EXACT_CENTRE: f64 = 2.0;

/// The tab row is centred in the window and the indicator sits under the first
/// tab. Catches a tab row that drifts off centre as well as a stray indicator.
fn check_header_tabs_centred(frame: &Frame, report: &mut Report) {
    let islands = frame.violet_islands(0, frame.width(), HEADER_HEIGHT - 12, HEADER_HEIGHT);
    let Some(indicator) = islands.first() else {
        report.require(
            false,
            "[01-tracks] header: active-tab indicator was not painted",
        );
        return;
    };

    let tab_width = if frame.width() < 1100 { TAB_WIDTH_SHRUNK } else { TAB_WIDTH };
    let tab_gap = if frame.width() < 1100 { TAB_GAP_SHRUNK } else { TAB_GAP };
    let tabs_width = TAB_COUNT * (tab_width + tab_gap) - tab_gap;

    // Center tabs in the remaining space between left logo and right actions
    let logo_right = frame
        .ink_extent_x(0, 250, 0, HEADER_HEIGHT - 2)
        .map(|(_, x1)| x1 as f64)
        .unwrap_or(201.0);
    let actions_left = if frame.width() < 1440 {
        frame.width() as f64 - 130.0
    } else {
        frame.width() as f64 - 423.0
    };

    let remaining_width = actions_left - logo_right;
    let expected_tabs_left = logo_right + (remaining_width - tabs_width) / 2.0;

    report.close_to(
        "[01-tracks] header: active-tab indicator centre",
        indicator.centre_x(),
        expected_tabs_left + tab_width / 2.0,
        5.0,
    );
    let expected_indicator_width = if frame.width() < 1100 {
        TAB_INDICATOR_WIDTH_SHRUNK
    } else {
        TAB_INDICATOR_WIDTH
    };
    report.require(
        indicator.width().abs_diff(expected_indicator_width) <= 2,
        format!(
            "[01-tracks] header: active-tab indicator is {}px wide, expected {expected_indicator_width}px",
            indicator.width()
        ),
    );
}

/// Locates the table header and its TRACK / STATUS / ACTIONS column groups.
/// The toolbar sits above, so the first painted band below it is the header.
fn table_header_columns(frame: &Frame) -> Option<Vec<Island>> {
    let (top, bottom) = frame.first_ink_band(0, W, 104, 210)?;
    let columns = frame.ink_islands(0, W, top, bottom + 1);
    (columns.len() == 3).then_some(columns)
}

/// Table header labels and body cells must share one column geometry, otherwise
/// the list looks subtly broken no matter how good the colours are.
fn check_tracks_columns_line_up(frame: &Frame, report: &mut Report) {
    let Some(header_columns) = table_header_columns(frame) else {
        report.require(
            false,
            "[01-tracks] table header TRACK/STATUS/ACTIONS columns were not painted",
        );
        return;
    };
    let header = frame
        .first_ink_band(0, W, 104, 210)
        .expect("table_header_columns found a header band");
    let Some(row) = frame.first_ink_band(0, W, header.1 + 10, header.1 + 90) else {
        report.require(false, "[01-tracks] no track row was painted");
        return;
    };

    // Faint, so the row's right-alignment is measured at the edge of the action
    // buttons' boxes rather than at the 13px glyph inside them.
    let row_columns = frame.faint_islands(0, W, row.0, row.1 + 1);
    if row_columns.is_empty() {
        report.require(false, "[01-tracks] the first track row painted nothing");
        return;
    }

    // A row paints several groups (cover, title, status, duration and one per
    // action button), so match the header labels to the row by position instead
    // of by index.
    let status = header_columns[1].centre_x();
    let nearest = row_columns
        .iter()
        .min_by(|a, b| {
            (a.centre_x() - status)
                .abs()
                .total_cmp(&(b.centre_x() - status).abs())
        })
        .expect("row has at least one painted group");
    report.close_to(
        "[01-tracks] STATUS column centre (header vs row)",
        status,
        nearest.centre_x(),
        EXACT_CENTRE,
    );

    // ACTIONS is right-aligned, so the header label must end exactly where the
    // row's rightmost group does.
    let actions_right = row_columns.last().expect("row is non-empty").x1;
    report.require(
        header_columns[2].x1.abs_diff(actions_right) <= 3,
        format!(
            "[01-tracks] ACTIONS right edge: header x{} vs row x{}",
            header_columns[2].x1, actions_right
        ),
    );
}

/// The transport cluster and the waveform/scrubber both sit in the player's
/// centre column, so their centres must agree with each other.
fn check_player_bar_is_centred(frame: &Frame, report: &mut Report) {
    let top = H - PLAYER_HEIGHT;
    // The 44px play/pause disc is the widest violet element in the upper half.
    let play = frame
        .violet_islands(0, W, top, top + 44)
        .into_iter()
        .find(|island| island.width() >= 40);
    // Measured with the faint threshold: the unplayed waveform bars are dimmer
    // than ordinary content, and they share the centre column with the disc.
    let waveform = frame
        .faint_islands(0, W, top + 44, H)
        .into_iter()
        .max_by_key(Island::width);

    let (Some(play), Some(waveform)) = (play, waveform) else {
        report.require(
            false,
            "[01-tracks] player bar: transport or waveform was not painted",
        );
        return;
    };

    report.close_to(
        "[01-tracks] player bar: transport centre vs waveform centre",
        play.centre_x(),
        waveform.centre_x(),
        EXACT_CENTRE,
    );
}

/// The current lyric line must be scrolled into the middle of the panel. Before
/// the panel laid its lines out explicitly, the built-in scroll view owned
/// `content-y` and this silently never happened.
///
/// The line list itself starts below the panel's own track header: 14px padding,
/// a 44px header row, a 12px gap, a 1px divider and another 12px gap.
const LYRICS_VIEWPORT_TOP: usize = HEADER_HEIGHT + 14 + 44 + 12 + 1 + 12;
/// The panel keeps its 14px bottom padding inside the player bar's height.
const LYRICS_VIEWPORT_BOTTOM: usize = H - PLAYER_HEIGHT - 14;

fn check_lyrics_auto_scrolls(frame: &Frame, report: &mut Report) {
    let panel_left = W - 390;

    // The active line carries its emphasis in the type itself: it is the only
    // near-white text in the list. There is no highlight box painted behind it,
    // which is also what keeps a long line from being clipped by its own frame.
    let Some(highlight) = frame
        .bright_islands(panel_left, W, LYRICS_VIEWPORT_TOP, LYRICS_VIEWPORT_BOTTOM)
        .into_iter()
        .max_by_key(|island| island.pixels)
    else {
        report.require(
            false,
            "[08-lyrics-scrolled] no near-white active lyric line was painted",
        );
        return;
    };

    let viewport_centre = (LYRICS_VIEWPORT_TOP + LYRICS_VIEWPORT_BOTTOM) as f64 / 2.0;
    report.close_to(
        "[08-lyrics-scrolled] active lyric line centre (auto-scroll)",
        highlight.centre_y(),
        viewport_centre,
        // The glyph band sits inside a line box that is centred exactly; the
        // slack is for font metrics differing between machines.
        40.0,
    );
}

/// The directory picker is a modal overlay covering the whole window. Its only
/// always-present control is the violet "Add Music Folder" pill, so finding that
/// pill where the card's footer belongs proves the overlay rendered on top of
/// everything else — and that its geometry follows the declared card size.
fn check_directories_dialog(frame: &Frame, report: &mut Report) {
    // Mirrors `ManageDirectoriesDialog`: a 540x480 card centred in the window,
    // with a 168x34 pill in its 20px-padded footer.
    const CARD_WIDTH: usize = 540;
    const CARD_HEIGHT: usize = 480;
    const PADDING: usize = 20;
    const PILL_WIDTH: usize = 168;

    let card_left = (W - CARD_WIDTH) / 2;
    let card_right = card_left + CARD_WIDTH;
    let card_bottom = (H + CARD_HEIGHT) / 2;

    let Some(add) = frame
        .violet_islands(
            card_left,
            card_right,
            card_bottom - PADDING - 34,
            card_bottom - PADDING,
        )
        .into_iter()
        .max_by_key(|island| island.width())
    else {
        report.require(
            false,
            "[16-directories] the dialog's \"Add Music Folder\" button was not painted",
        );
        return;
    };

    report.close_to(
        "[16-directories] \"Add Music Folder\" width",
        add.width() as f64,
        PILL_WIDTH as f64,
        2.0,
    );
    report.close_to(
        "[16-directories] \"Add Music Folder\" right edge",
        add.x1 as f64,
        (card_right - PADDING) as f64,
        3.0,
    );
}

/// A lyric long enough to wrap must be drawn in full. When the active line was a
/// fixed-height box, a second line fell outside it, was clipped, and simply
/// disappeared — the text looked broken in exactly the way the user saw.
fn check_active_line_is_not_clipped(frame: &Frame, report: &mut Report) {
    let Some(highlight) = frame
        .bright_islands(W - 390, W, LYRICS_VIEWPORT_TOP, LYRICS_VIEWPORT_BOTTOM)
        .into_iter()
        .max_by_key(|island| island.pixels)
    else {
        report.require(
            false,
            "[08b-lyrics-wrapped] the active lyric line was not painted",
        );
        return;
    };

    // One line of 19px text paints a glyph band around 15px tall; two lines
    // paint roughly 38px. A clipped second line leaves this near 15px.
    report.require(
        highlight.height() >= 26,
        format!(
            "[08b-lyrics-wrapped] the wrapped active lyric is only {}px tall — its second line was cut off",
            highlight.height()
        ),
    );
}

/// Each status pill pairs a coloured marker with a label. The marker is a 6px
/// square inside a 22px pill, so when a layout start-aligns it the marker rides
/// ~7px high and the pill reads as broken while every other check still passes.
/// Measured as asymmetry of the pill's *interior* ink about its centre line.
fn check_status_markers_are_centred(frame: &Frame, report: &mut Report) {
    let Some(header_columns) = table_header_columns(frame) else {
        report.require(
            false,
            "[01-tracks] cannot measure status markers without the table header",
        );
        return;
    };
    let status_centre = header_columns[1].centre_x();

    // One band per visible track row.
    let rows = frame.ink_bands(0, W, 148, H - PLAYER_HEIGHT);
    let mut measured = 0;
    for (band_top, band_bottom) in rows {
        // The pill is the row's column group sitting under the STATUS header.
        let Some(pill) = frame
            .faint_islands(0, W, band_top, band_bottom + 1)
            .into_iter()
            .filter(|island| island.width() > 40)
            .min_by(|a, b| {
                (a.centre_x() - status_centre)
                    .abs()
                    .total_cmp(&(b.centre_x() - status_centre).abs())
            })
        else {
            continue;
        };

        let Some((pill_top, pill_bottom)) =
            frame.ink_extent_y(pill.x0, pill.x1 + 1, band_top, band_bottom + 1)
        else {
            continue;
        };
        let centre = (pill_top + pill_bottom) as f64 / 2.0;

        // Inset past the 1px border and its 11px corner radius: with the inset at
        // 14px no border pixel can fall inside, so this is marker + label only.
        let Some((inner_top, inner_bottom)) = frame.ink_extent_y(
            pill.x0 + 14,
            pill.x1.saturating_sub(13),
            pill_top + 3,
            pill_bottom.saturating_sub(1),
        ) else {
            continue;
        };

        let asymmetry = (inner_top as f64 - centre) + (inner_bottom as f64 - centre);
        report.close_to(
            &format!("[01-tracks] status marker centre (row at y{band_top})"),
            asymmetry,
            0.0,
            EXACT_CENTRE,
        );
        measured += 1;
    }

    report.require(
        measured > 0,
        "[01-tracks] no status pills were measured in the track list",
    );
}

/// Artwork has to reach the list. This is the regression the views originally
/// shipped with: a `cover` field nothing ever populated, so every row drew its
/// placeholder forever. The harness gives the first rows a solid stand-in colour,
/// so "is a cover painted here?" is exactly a pixel test.
fn check_cover_art_is_painted(
    frame: &Frame,
    report: &mut Report,
    name: &str,
    region: (usize, usize, usize, usize),
) {
    let (x0, x1, y0, y1) = region;
    let painted = (y0..y1.min(frame.height()))
        .any(|y| (x0..x1.min(frame.width())).any(|x| frame.pixel(x, y) == COVER_TINT));
    report.require(
        painted,
        format!("[{name}] no cover artwork was painted in {region:?}"),
    );
}

/// The album tab is a card grid, and the grid is geometry the type checker
/// cannot see: a broken `grid-columns`/`album-rows` pairing silently drops cards
/// off the end of a row or stacks them in one column.
fn check_album_grid(frame: &Frame, report: &mut Report) {
    // Artwork bands are the ones whose ink groups are tall (the cover art, or its
    // placeholder glyph); the name/artist/status bands are one text line each, so
    // this skips them without needing to know the card's internal spacing.
    let mut artwork_rows: Vec<Vec<Island>> = Vec::new();
    for (top, bottom) in frame.ink_bands(0, W, 120, H - PLAYER_HEIGHT) {
        let islands = frame.ink_islands(0, W, top, bottom + 1);
        if islands.len() >= crate::ALBUM_GRID_COLUMNS && islands.iter().all(|i| i.height() >= 24) {
            artwork_rows.push(islands);
        }
    }

    let Some(first) = artwork_rows.first() else {
        report.require(
            false,
            "[02-albums] found no row of album artwork — the grid did not render",
        );
        return;
    };

    report.close_to(
        &format!(
            "[02-albums] covers per row (grid-columns = {})",
            crate::ALBUM_GRID_COLUMNS
        ),
        first.len() as f64,
        crate::ALBUM_GRID_COLUMNS as f64,
        0.5,
    );

    // Evenly spaced cards: consecutive centre-to-centre gaps must match.
    let centres: Vec<f64> = first.iter().map(|i| i.centre_x()).collect();
    let gaps: Vec<f64> = centres.windows(2).map(|w| w[1] - w[0]).collect();
    if let (Some(min), Some(max)) = (
        gaps.iter()
            .cloned()
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.min(b)))),
        gaps.iter()
            .cloned()
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b)))),
    ) {
        report.close_to("[02-albums] cards are evenly spaced", max, min, 2.0);
    }

    // Rows share their columns: the second row's covers sit under the first's.
    report.require(
        artwork_rows.len() >= 2,
        format!(
            "[02-albums] expected a multi-row grid, measured {} artwork row(s)",
            artwork_rows.len()
        ),
    );
    for (row_idx, row) in artwork_rows.iter().enumerate().skip(1) {
        let centres: Vec<f64> = row.iter().map(|i| i.centre_x()).collect();
        for (col, centre) in centres.iter().enumerate().take(crate::ALBUM_GRID_COLUMNS) {
            let Some(expected) = first.get(col).map(|i| i.centre_x()) else {
                continue;
            };
            report.close_to(
                &format!("[02-albums] row {row_idx} column {col} aligns with row 0"),
                *centre,
                expected,
                2.0,
            );
        }
    }
}

/// A labelled button centres its icon on its label. The icon is drawn with a
/// fixed size inside a horizontal layout, so it needs the same explicit `y`.
fn check_button_icon_is_centred(frame: &Frame, report: &mut Report, name: &str, button: Island) {
    // The icon occupies the button's 12px left padding plus its 16px box; the
    // label follows at the 7px spacing.
    let icon = frame.centroid_y(button.x0 + 8, button.x0 + 32, button.y0, button.y1 + 1);
    let label = frame.centroid_y(
        button.x0 + 36,
        button.x1.saturating_sub(4),
        button.y0,
        button.y1 + 1,
    );

    match (icon, label) {
        (Some((icon, _)), Some((label, _))) => report.close_to(
            &format!("[{name}] button icon centre vs label centre"),
            icon,
            label,
            EXACT_CENTRE,
        ),
        _ => report.require(
            false,
            format!("[{name}] could not measure the button at {button:?} — icon or label missing"),
        ),
    }
}

/// The header's labelled actions (Add Folder, Download All) sit at the right of
/// the app header.
fn check_header_action_icons_are_centred(frame: &Frame, report: &mut Report) {
    if frame.width() >= 1440 {
        let buttons: Vec<Island> = frame
            .islands(frame.width() - 450, frame.width(), 0, HEADER_HEIGHT - 2, FAINT_LUMA, 2)
            .into_iter()
            .filter(|island| island.width() > 60)
            .collect();

        report.require(
            !buttons.is_empty(),
            &format!("[{}] no labelled header actions were painted", frame.width()),
        );
        for (index, button) in buttons.iter().enumerate() {
            check_button_icon_is_centred(
                frame,
                report,
                &format!("header action {index}"),
                *button,
            );
        }
    } else {
        // Below 1440px: ToolbarButtons are icon-only (34px wide)
        let buttons: Vec<Island> = frame
            .islands(frame.width() - 160, frame.width(), 0, HEADER_HEIGHT - 2, FAINT_LUMA, 2)
            .into_iter()
            .filter(|island| island.width() >= 28 && island.width() <= 45)
            .collect();

        report.require(
            buttons.len() == 3,
            format!(
                "[{}] expected 3 icon-only header actions, found {}",
                frame.width(),
                buttons.len()
            ),
        );
        for (index, button) in buttons.iter().enumerate() {
            let icon = frame.centroid_y(button.x0, button.x1 + 1, button.y0, button.y1 + 1);
            if let Some((icon_y, _)) = icon {
                report.close_to(
                    &format!("[header action {index}] button vertical centre"),
                    icon_y,
                    HEADER_HEIGHT as f64 / 2.0,
                    2.0,
                );
            }
        }
    }
}

/// Asserts that none of the three header regions (Left logo, Center tabs, Right actions)
/// overlap each other at the given resolution.
fn check_header_regions_no_overlap(frame: &Frame, name: &str, report: &mut Report) {
    let w = frame.width();

    let tab_idx = if name.starts_with("view-01") {
        0
    } else if name.starts_with("view-02") {
        1
    } else if name.starts_with("view-03") {
        2
    } else if name.starts_with("view-04") {
        3
    } else if name.starts_with("view-05") {
        4
    } else {
        return;
    };

    // 1. Left logo + title region
    let Some(logo_extent) = frame.ink_extent_x(0, 250, 0, HEADER_HEIGHT - 2) else {
        report.record_failure(format!("[{name}] left logo and title must be painted"));
        return;
    };
    let logo_right = logo_extent.1;

    // 2. Center nav tabs region
    let violet = frame.violet_islands(0, w, HEADER_HEIGHT - 12, HEADER_HEIGHT);
    let Some(indicator) = violet.first() else {
        // Tab indicator only exists when one of the tabs 0..5 is active
        return;
    };
    let tab_width = if w < 1100 { TAB_WIDTH_SHRUNK } else { TAB_WIDTH };
    let tab_gap = if w < 1100 { TAB_GAP_SHRUNK } else { TAB_GAP };
    let tabs_width = 5.0 * tab_width + 4.0 * tab_gap;

    let tabs_left = indicator.centre_x() - tab_width / 2.0 - (tab_idx as f64) * (tab_width + tab_gap);
    let tabs_right = tabs_left + tabs_width;

    // 3. Right action toolbar buttons region
    let search_start = if w < 1440 { w - 160 } else { w - 450 };
    let buttons = frame.islands(search_start, w, 0, HEADER_HEIGHT - 2, FAINT_LUMA, 2);
    report.require(
        !buttons.is_empty(),
        format!("[{name}] right toolbar buttons were not painted"),
    );
    let actions_left = buttons.first().map(|b| b.x0).unwrap_or(w - 130);

    // Assert: No overlap between adjacent regions
    report.require(
        logo_right < tabs_left as usize,
        format!(
            "[{name}] Left logo (x={logo_right}) overlaps with tabs (x={tabs_left:.1})"
        ),
    );
    report.require(
        (tabs_right as usize) < actions_left,
        format!(
            "[{name}] Center tabs (x={tabs_right:.1}) overlaps with right actions (x={actions_left}) - overlap is {:.1}px",
            tabs_right - actions_left as f64
        ),
    );

    // Clearance assertion: at least 16px clearance between adjacent regions
    let left_clearance = tabs_left - logo_right as f64;
    let right_clearance = actions_left as f64 - tabs_right;
    report.require(
        left_clearance >= 16.0,
        format!("[{name}] Left clearance too small: {left_clearance:.1}px (expected >= 16px)"),
    );
    report.require(
        right_clearance >= 16.0,
        format!("[{name}] Right clearance too small: {right_clearance:.1}px (expected >= 16px)"),
    );
}

/// The first painted thing inside an empty state is its icon (or its call to
/// action), and it must be centred in the view that owns it.
fn check_region_centred(
    frame: &Frame,
    report: &mut Report,
    name: &str,
    view_x0: usize,
    view_x1: usize,
    y0: usize,
    y1: usize,
) {
    let Some((top, bottom)) = frame.first_ink_band(view_x0, view_x1, y0, y1) else {
        report.require(
            false,
            format!(
                "[{name}] nothing painted between y{y0} and y{y1}, expected a centred empty state"
            ),
        );
        return;
    };

    let Some((centre, _)) = frame.centroid_x(view_x0, view_x1, top, bottom + 1) else {
        report.require(
            false,
            format!("[{name}] could not measure the painted region"),
        );
        return;
    };

    report.close_to(
        &format!("[{name}] empty state centre"),
        centre,
        (view_x0 + view_x1) as f64 / 2.0,
        EXACT_CENTRE,
    );
}
