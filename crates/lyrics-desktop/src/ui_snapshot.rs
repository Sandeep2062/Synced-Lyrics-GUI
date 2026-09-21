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

use crate::ui_audit::{Frame, Island, Report, FAINT_LUMA};
use crate::{AlbumData, ArtistData, LyricLine, MainWindow, TrackData};
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
fn capture(name: &str, report: &mut Report) -> Frame {
    let window = window();
    let opaque_black = PremultipliedRgbaColor {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };
    let mut buffer = vec![opaque_black; (WIDTH * HEIGHT) as usize];

    slint::platform::update_timers_and_animations();
    let rendered = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(rendered, "{name}: window had no dirty region to render");

    let mut img = image::RgbaImage::new(WIDTH, HEIGHT);
    for (i, px) in buffer.iter().enumerate() {
        img.put_pixel(
            (i as u32) % WIDTH,
            (i as u32) / WIDTH,
            image::Rgba([px.red, px.green, px.blue, 255]),
        );
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&dir).expect("create snapshot dir");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("write snapshot");
    println!("wrote {}", path.display());

    let frame = Frame::new(&buffer, WIDTH as usize, HEIGHT as usize);

    // Every screen, in every state, must be drawn with the app's own theme. A
    // large uniform light-grey region means a platform-themed widget (LineEdit,
    // Slider, CheckBox, ...) leaked through — the defect class this catches.
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

    frame
}

fn sample_tracks() -> Vec<TrackData> {
    let rows: [(&str, &str, &str, &str, &str); 12] = [
        (
            "WUZZUP (Prod. 808 Sensei)",
            "2. youngsch",
            "808 Sensei",
            "3:12",
            "missing",
        ),
        (
            "(Do The) Act Like You Never Met Me",
            "06. TV Girl",
            "Who Really Cares",
            "4:01",
            "synced",
        ),
        (
            "[Li Peep TYPE BEAT] M.O.S",
            "Unknown Artist",
            "Unknown Album",
            "2:48",
            "missing",
        ),
        (
            "(Nice Dream)",
            "06. Radiohead",
            "The Bends",
            "3:53",
            "synced",
        ),
        (
            "40",
            "08. Droppin So Pretty",
            "Unknown Album",
            "2:22",
            "synced",
        ),
        (
            "00101110 01100010 01100001",
            "11. OmenXIII",
            "Unknown Album",
            "2:05",
            "plain",
        ),
        (
            "01 Corrupted",
            "Unknown Artist",
            "Unknown Album",
            "3:30",
            "suspicious",
        ),
        (
            "01 Easy for You",
            "Unknown Artist",
            "Unknown Album",
            "2:55",
            "synced",
        ),
        (
            "01 Halloween",
            "Unknown Artist",
            "Unknown Album",
            "2:41",
            "synced",
        ),
        (
            "01 I'm Used to That",
            "Unknown Artist",
            "Unknown Album",
            "3:18",
            "synced",
        ),
        (
            "01 Koi no Uta (feat. Tsukasa Yuzaki)",
            "Unknown Artist",
            "Unknown Album",
            "2:47",
            "synced",
        ),
        (
            "01 Vengeance",
            "Unknown Artist",
            "Unknown Album",
            "2:36",
            "synced",
        ),
    ];
    rows.iter()
        .enumerate()
        .map(|(idx, (title, artist, album, dur, status))| TrackData {
            path: SharedString::from(format!("C:\\Music\\{artist} - {title}.mp3")),
            artist: SharedString::from(*artist),
            title: SharedString::from(*title),
            album: SharedString::from(*album),
            duration: SharedString::from(*dur),
            status: SharedString::from(*status),
            has_lyrics: *status != "missing",
            // The first screenful carries artwork so the snapshots prove covers
            // actually reach the list, and the rest stay as placeholders.
            cover: if idx < COVERED_ROWS {
                sample_cover()
            } else {
                slint::Image::default()
            },
            has_cover: idx < COVERED_ROWS,
        })
        .collect()
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

fn sample_albums() -> Vec<AlbumData> {
    const SEED: [(&str, &str, i32, i32); 24] = [
        ("The Bends", "Radiohead", 12, 11),
        ("Who Really Cares", "TV Girl", 10, 10),
        ("Native", "OneRepublic", 12, 9),
        ("In Rainbows", "Radiohead", 10, 10),
        ("A Brief Inquiry", "The 1975", 15, 12),
        ("Blonde", "Frank Ocean", 17, 4),
        ("Kid A", "Radiohead", 10, 8),
        ("French Exit", "TV Girl", 13, 13),
        ("Dreamland", "Glass Animals", 16, 2),
        ("Immunity", "Jon Hopkins", 8, 0),
        ("Vessel", "Twenty One Pilots", 12, 5),
        ("Currents", "Tame Impala", 13, 7),
        ("Untrue", "Burial", 13, 1),
        ("Circles", "Mac Miller", 12, 0),
        ("Sawayama", "Rina Sawayama", 13, 6),
        ("The Slow Rush", "Tame Impala", 12, 12),
        ("Good Kid, M.A.A.D City", "Kendrick Lamar", 12, 12),
        ("Igor", "Tyler, The Creator", 12, 3),
        ("Malibu", "Anderson .Paak", 16, 9),
        ("Awaken, My Love!", "Childish Gambino", 11, 11),
        ("Melodrama", "Lorde", 11, 6),
        ("SOS", "SZA", 23, 21),
        ("An Evening with Silk Sonic", "Silk Sonic", 9, 4),
        ("Cavalcade", "black midi", 9, 0),
    ];
    SEED.iter()
        .enumerate()
        .map(|(idx, (name, artist, count, synced))| AlbumData {
            path: SharedString::from(format!("C:\\Music\\{artist}\\{name}\\01.mp3")),
            name: SharedString::from(*name),
            artist: SharedString::from(*artist),
            track_count: *count,
            synced_count: *synced,
            cover: if idx < COVERED_ROWS {
                sample_cover()
            } else {
                slint::Image::default()
            },
            has_cover: idx < COVERED_ROWS,
        })
        .collect()
}

fn sample_artists() -> Vec<ArtistData> {
    const SEED: [(&str, i32, i32, i32); 10] = [
        ("Radiohead", 32, 3, 29),
        ("TV Girl", 23, 2, 23),
        ("Tame Impala", 25, 2, 19),
        ("OneRepublic", 12, 1, 9),
        ("The 1975", 15, 1, 12),
        ("Frank Ocean", 17, 1, 4),
        ("Glass Animals", 16, 1, 2),
        ("OmenXIII", 41, 4, 0),
        ("Burial", 13, 1, 1),
        ("Mac Miller", 12, 1, 0),
    ];
    SEED.iter()
        .map(|(name, tracks, albums, synced)| ArtistData {
            name: SharedString::from(*name),
            track_count: *tracks,
            album_count: *albums,
            synced_count: *synced,
        })
        .collect()
}

fn sample_lyrics() -> Vec<LyricLine> {
    [
        "I know you're somewhere out there",
        "Somewhere far away",
        "I want you back, I want you back",
        "My neighbors think I'm crazy",
        "But they don't understand",
        "You're all I had, you're all I had",
        "At night when the stars light up my room",
        "I sit by myself",
        "Talking to the moon",
    ]
    .iter()
    .enumerate()
    .map(|(i, text)| LyricLine {
        timestamp: i as f32 * 4.0,
        text: SharedString::from(*text),
    })
    .collect()
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
    ui.set_tracks(ModelRc::new(VecModel::from(sample_tracks())));
    ui.set_albums(ModelRc::new(VecModel::from(sample_albums())));
    ui.set_album_rows(ModelRc::new(VecModel::from(crate::album_rows(
        sample_albums().len(),
    ))));
    ui.set_artists(ModelRc::new(VecModel::from(sample_artists())));
    ui.set_tracks_summary("2059 tracks | 1208 synced | 198 plain | 653 missing".into());
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_settings_music_dirs(ModelRc::new(VecModel::from(vec![
        SharedString::from("C:\\Users\\Sandeep\\Music"),
        SharedString::from("D:\\Library\\Lossless"),
    ])));
    ui.set_settings_musixmatch_key("9f3a1c77b204e5d8".into());
    ui.set_settings_enable_lrclib(true);
    ui.set_settings_enable_musixmatch(true);
    ui.set_settings_enable_netease(true);
    ui.set_settings_cache_period_idx(1);
    ui.set_lyrics_lines(ModelRc::new(VecModel::from(sample_lyrics())));
    ui.set_active_line_index(4);
    ui.set_lyrics_status("synced".into());
    ui.set_track_title("Counting Stars".into());
    ui.set_track_subtitle("OneRepublic • Native".into());
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
    f(&ui);
}

#[test]
fn ui_snapshot() {
    let mut report = Report::default();

    with_app(5_000, |ui| {
        let tracks = capture("01-tracks", &mut report);
        check_header_tabs_centred(&tracks, &mut report);
        check_tracks_columns_line_up(&tracks, &mut report);
        check_player_bar_is_centred(&tracks, &mut report);
        check_status_markers_are_centred(&tracks, &mut report);
        check_header_action_icons_are_centred(&tracks, &mut report);

        ui.set_active_tab(1);
        let albums = capture("02-albums", &mut report);
        check_album_grid(&albums, &mut report);
        check_cover_art_is_painted(&tracks, &mut report, "01-tracks", (30, 120, 150, 640));
        check_cover_art_is_painted(&albums, &mut report, "02-albums", (40, 1240, 130, 680));
        ui.set_active_tab(2);
        capture("03-artists", &mut report);
        ui.set_active_tab(3);
        capture("04-settings", &mut report);
        ui.set_active_tab(0);
        ui.set_lyrics_visible(true);
        settle(600);
        capture("05-tracks-lyrics-panel", &mut report);

        // Auto-scroll: the active lyric line must stay centred as playback moves.
        ui.set_active_line_index(0);
        settle(600);
        capture("06-lyrics-first-line", &mut report);
        ui.set_active_line_index(8);
        settle(600);
        capture("07-lyrics-last-line", &mut report);

        // Enough lines to overflow the panel: the active line must be scrolled
        // to the middle rather than stuck where the list happens to start.
        ui.set_lyrics_lines(ModelRc::new(VecModel::from(
            (0..30)
                .map(|i| LyricLine {
                    timestamp: i as f32 * 4.0,
                    text: SharedString::from(format!("Line {i} of the lyrics")),
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_active_line_index(20);
        settle(600);
        let scrolled = capture("08-lyrics-scrolled", &mut report);
        check_lyrics_auto_scrolls(&scrolled, &mut report);

        // Real lyrics wrap. The active line has to make room for the second
        // line instead of clipping it away.
        ui.set_lyrics_lines(ModelRc::new(VecModel::from(vec![
            LyricLine {
                timestamp: 0.0,
                text: SharedString::from("A short line"),
            },
            LyricLine {
                timestamp: 4.0,
                text: SharedString::from(
                    "Okay, yeah I hit that, shawty, get back and never look at it again",
                ),
            },
            LyricLine {
                timestamp: 8.0,
                text: SharedString::from("Another short line"),
            },
        ])));
        ui.set_active_line_index(1);
        settle(600);
        let wrapped = capture("08b-lyrics-wrapped", &mut report);
        check_active_line_is_not_clipped(&wrapped, &mut report);
        ui.set_lyrics_lines(ModelRc::new(VecModel::from(sample_lyrics())));
        ui.set_active_line_index(4);
        settle(400);

        // Album drill-down is a separate branch from the album grid.
        ui.set_lyrics_visible(false);
        ui.set_active_tab(1);
        ui.set_album_detail_mode(true);
        ui.set_selected_album_index(0);
        ui.set_album_tracks(ModelRc::new(VecModel::from(sample_tracks())));
        settle(500);
        let album_detail = capture("09-albums-detail", &mut report);
        check_cover_art_is_painted(
            &album_detail,
            &mut report,
            "09-albums-detail",
            (30, 120, 250, 700),
        );

        // A selected artist renders the detail header plus its track list.
        ui.set_album_detail_mode(false);
        ui.set_active_tab(2);
        ui.set_selected_artist_index(0);
        ui.set_artist_tracks(ModelRc::new(VecModel::from(sample_tracks())));
        settle(500);
        capture("10-artist-detail", &mut report);

        // Empty library: every view must fall back to a real empty state.
        ui.set_selected_artist_index(-1);
        ui.set_tracks(ModelRc::new(VecModel::from(Vec::<TrackData>::new())));
        ui.set_albums(ModelRc::new(VecModel::from(Vec::<AlbumData>::new())));
        ui.set_album_rows(ModelRc::new(VecModel::from(Vec::<i32>::new())));
        ui.set_artists(ModelRc::new(VecModel::from(Vec::<ArtistData>::new())));
        ui.set_tracks_summary(SharedString::new());
        ui.set_active_tab(0);
        settle(400);
        let empty = capture("11-empty-tracks", &mut report);
        check_region_centred(&empty, &mut report, "11-empty-tracks", 0, W, 250, 520);

        // Filters active but nothing matches: the other empty state, with its
        // "Clear filters" affordance.
        ui.set_search_text("no such track".into());
        settle(400);
        let filtered = capture("11b-empty-filtered", &mut report);
        // Guards the `x: (parent.width - self.width) / 2` centring of the button.
        check_region_centred(&filtered, &mut report, "11b-empty-filtered", 0, W, 482, 545);
        ui.set_search_text(SharedString::new());

        ui.set_active_tab(1);
        settle(400);
        let no_albums = capture("12-empty-albums", &mut report);
        check_region_centred(&no_albums, &mut report, "12-empty-albums", 0, W, 250, 520);

        ui.set_active_tab(2);
        settle(400);
        let no_artists = capture("13-empty-artists", &mut report);
        // The artists empty state lives in the 340px list panel, not the window.
        check_region_centred(
            &no_artists,
            &mut report,
            "13-empty-artists",
            16,
            356,
            250,
            520,
        );
        ui.set_active_tab(0);
        ui.set_tracks(ModelRc::new(VecModel::from(sample_tracks())));
        ui.set_albums(ModelRc::new(VecModel::from(sample_albums())));
        ui.set_album_rows(ModelRc::new(VecModel::from(crate::album_rows(
            sample_albums().len(),
        ))));
        ui.set_artists(ModelRc::new(VecModel::from(sample_artists())));
        ui.set_tracks_summary("2059 tracks | 1208 synced | 198 plain | 653 missing".into());

        // Plain (unsynced) lyrics: the panel swaps the line list for plain text.
        ui.set_lyrics_status("plain".into());
        ui.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
        ui.set_lyrics_plain_text(
            "I know you're somewhere out there\nSomewhere far away\nI want you back, I want you back\n\nMy neighbors think I'm crazy\nBut they don't understand"
                .into(),
        );
        ui.set_lyrics_visible(true);
        settle(600);
        capture("15-lyrics-plain", &mut report);

        // The directory picker: a modal overlay, and the only screen a first run
        // can reach until a folder has been added.
        ui.set_directories_open(true);
        settle(500);
        let directories = capture("16-directories", &mut report);
        check_directories_dialog(&directories, &mut report);
        ui.set_directories_open(false);
        settle(300);

        // Batch download modal Stage 0 & Stage 1
        ui.set_batch_download_open(true);
        ui.set_batch_download_stage(0);
        ui.set_batch_total_count(2059);
        ui.set_batch_missing_count(653);
        ui.set_batch_plain_count(198);
        ui.set_batch_suspicious_count(12);
        settle(500);
        capture("17-batch-stage0", &mut report);

        ui.set_batch_download_stage(1);
        ui.set_batch_mode_title("Download Only for Missing".into());
        ui.set_batch_phase_text("Downloading track 45 of 653...".into());
        ui.set_batch_current_track("Adele — Hello".into());
        ui.set_batch_progress(0.45);
        ui.set_batch_progress_text("45 / 653 (45%)".into());
        ui.set_batch_stats(crate::BatchStats {
            total: 653,
            processed: 45,
            remaining: 608,
            synced: 38,
            plain: 4,
            not_found: 2,
            errors: 1,
        });
        ui.set_batch_is_running(true);
        let sample_logs = vec![
            crate::BatchLogEntry {
                timestamp: "14:22:05".into(),
                track_title: "Hello".into(),
                track_artist: "Adele".into(),
                details: "✓ LRCLib (synced)  •  ✓ Musixmatch (synced)".into(),
                action_text: "→ Saved synced lyrics from LRCLib".into(),
                badge_text: "SYNCED".into(),
                badge_type: "synced".into(),
            },
            crate::BatchLogEntry {
                timestamp: "14:22:07".into(),
                track_title: "Counting Stars".into(),
                track_artist: "OneRepublic".into(),
                details: "✗ LRCLib  •  ✓ Genius (plain)".into(),
                action_text: "→ Saved plain lyrics from Genius".into(),
                badge_text: "PLAIN".into(),
                badge_type: "plain".into(),
            },
            crate::BatchLogEntry {
                timestamp: "14:22:09".into(),
                track_title: "Unknown Intro".into(),
                track_artist: "Unknown Artist".into(),
                details: "✗ LRCLib  •  ✗ Musixmatch  •  ✗ NetEase  •  ✗ Megalobiz  •  ✗ Genius".into(),
                action_text: "→ No matching lyrics found".into(),
                badge_text: "NOT FOUND".into(),
                badge_type: "missing".into(),
            },
            crate::BatchLogEntry {
                timestamp: "14:22:11".into(),
                track_title: "Rescan Complete".into(),
                track_artist: "Library".into(),
                details: "Discovered 2,059 audio tracks across 2 folders".into(),
                action_text: "".into(),
                badge_text: "SCAN".into(),
                badge_type: "scan".into(),
            },
        ];
        ui.set_batch_logs(ModelRc::new(VecModel::from(sample_logs)));
        settle(500);
        capture("18-batch-stage1", &mut report);
        ui.set_batch_download_open(false);
        settle(300);

        // No lyrics at all: the panel's fetch prompt is the third panel branch.
        ui.set_lyrics_status("missing".into());
        ui.set_lyrics_plain_text(SharedString::new());
        settle(600);
        let no_lyrics = capture("14-lyrics-none", &mut report);
        // The panel is a 390px slide-over pinned to the right edge.
        check_region_centred(
            &no_lyrics,
            &mut report,
            "14-lyrics-none",
            W - 390,
            W,
            200,
            560,
        );
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
const TAB_COUNT: f64 = 4.0;
/// The indicator is `tab-width - 40px` wide.
const TAB_INDICATOR_WIDTH: usize = 56;

/// Fixed-size elements are placed by the layout engine, so they must be exact.
/// Text is deliberately not measured this precisely: its width depends on the
/// font stack of the machine running the test.
const EXACT_CENTRE: f64 = 2.0;

/// The tab row is centred in the window and the indicator sits under the first
/// tab. Catches a tab row that drifts off centre as well as a stray indicator.
fn check_header_tabs_centred(frame: &Frame, report: &mut Report) {
    let islands = frame.violet_islands(0, W, HEADER_HEIGHT - 12, HEADER_HEIGHT);
    let Some(indicator) = islands.first() else {
        report.require(
            false,
            "[01-tracks] header: active-tab indicator was not painted",
        );
        return;
    };

    let tabs_width = TAB_COUNT * (TAB_WIDTH + TAB_GAP) - TAB_GAP;
    let tab_row_left = (WIDTH as f64 - tabs_width) / 2.0;
    report.close_to(
        "[01-tracks] header: active-tab indicator centre",
        indicator.centre_x(),
        tab_row_left + TAB_WIDTH / 2.0,
        EXACT_CENTRE,
    );
    report.require(
        indicator.width().abs_diff(TAB_INDICATOR_WIDTH) <= 2,
        format!(
            "[01-tracks] header: active-tab indicator is {}px wide, expected {TAB_INDICATOR_WIDTH}px",
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
    // Faint threshold to catch the buttons' own chrome, a 2px gap so the 8px
    // spacing between them still splits them, and stop short of the header's
    // bottom divider (faint, and it would merge everything into one island).
    let buttons: Vec<Island> = frame
        .islands(900, W, 0, HEADER_HEIGHT - 2, FAINT_LUMA, 2)
        .into_iter()
        // The icon-only refresh button is self-centred and has no label.
        .filter(|island| island.width() > 60)
        .collect();

    report.require(
        !buttons.is_empty(),
        "[01-tracks] no labelled header actions were painted",
    );
    for (index, button) in buttons.iter().enumerate() {
        check_button_icon_is_centred(
            frame,
            report,
            &format!("01-tracks] header action {index}"),
            *button,
        );
    }
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
