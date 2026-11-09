use crate::art_loader::{
    album_cover_image, album_cover_image_now, art_stamp, cover_for, index_library_art,
    track_art_key, track_cover, track_cover_now, ArtLoader,
};
use crate::{AlbumData, ArtistData, MainWindow, TrackData};
use lyrics_core::{app_data_dir, LibraryDb, LyricsStatus, Track};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

pub(crate) struct AppState {
    pub(crate) all_tracks: Vec<Track>,
    pub(crate) filtered_tracks: Vec<usize>, // indices into all_tracks
    pub(crate) current_filtered_row: Option<usize>,
    pub(crate) was_playing: bool,
    /// Track indices of the open album / artist, in the order the detail lists
    /// show them, so a click on row N can find its track.
    pub(crate) album_detail_tracks: Vec<usize>,
    pub(crate) artist_detail_tracks: Vec<usize>,
}

impl AppState {
    pub(crate) fn new() -> Self {
        Self {
            all_tracks: Vec::new(),
            filtered_tracks: Vec::new(),
            current_filtered_row: None,
            was_playing: false,
            album_detail_tracks: Vec::new(),
            artist_detail_tracks: Vec::new(),
        }
    }

    pub(crate) fn rebuild_filter(&mut self, search: &str, status_filter: i32) {
        let search_lower = search.to_ascii_lowercase();
        self.filtered_tracks = self
            .all_tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| {
                if !search_lower.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        track.title.to_ascii_lowercase(),
                        track.artist.to_ascii_lowercase(),
                        track.album.to_ascii_lowercase()
                    );
                    if !haystack.contains(&search_lower) {
                        return false;
                    }
                }
                match status_filter {
                    1 => track.status == LyricsStatus::Missing,
                    2 => track.status == LyricsStatus::Plain,
                    3 => track.status == LyricsStatus::Synced,
                    _ => true,
                }
            })
            .map(|(i, _)| i)
            .collect();
    }
}

// ---------------------------------------------------------------------------
// UI Library caching
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct UiLibrary {
    pub(crate) tracks: Vec<TrackData>,
    pub(crate) path_to_index: HashMap<String, usize>,
    pub(crate) album_to_indices: HashMap<String, Vec<usize>>,
}

impl UiLibrary {
    pub(crate) fn set_tracks(&mut self, tracks: Vec<TrackData>) {
        let mut path_to_index = HashMap::with_capacity(tracks.len());
        let mut album_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, t) in tracks.iter().enumerate() {
            path_to_index.insert(t.path.to_string(), idx);
            album_to_indices
                .entry(t.album.to_string())
                .or_default()
                .push(idx);
        }
        self.tracks = tracks;
        self.path_to_index = path_to_index;
        self.album_to_indices = album_to_indices;
    }

    pub(crate) fn patch_covers(&mut self, paths: &[String], albums: &[String]) {
        for path in paths {
            if let Some(&idx) = self.path_to_index.get(path) {
                if let Some(Some(img)) = crate::art_loader::cached_cover_image(path) {
                    if let Some(track) = self.tracks.get_mut(idx) {
                        track.cover = img;
                        track.has_cover = true;
                    }
                }
            }
        }
        for album in albums {
            if let Some(indices) = self.album_to_indices.get(album) {
                if let Some(Some(img)) =
                    crate::art_loader::cached_cover_image(&crate::art_loader::album_image_key(album))
                {
                    for &idx in indices {
                        if let Some(track) = self.tracks.get_mut(idx) {
                            if !track.has_cover {
                                track.cover = img.clone();
                                track.has_cover = true;
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn get_filtered(&self, indices: &[usize]) -> Vec<TrackData> {
        indices
            .iter()
            .filter_map(|&i| self.tracks.get(i).cloned())
            .collect()
    }
}

thread_local! {
    pub(crate) static UI_LIBRARY: std::cell::RefCell<UiLibrary> =
        std::cell::RefCell::new(UiLibrary::default());
}

pub(crate) fn patch_ui_library_covers(paths: &[String], albums: &[String]) {
    UI_LIBRARY.with(|lib| {
        lib.borrow_mut().patch_covers(paths, albums);
    });
}

// ---------------------------------------------------------------------------
// Data conversion helpers
// ---------------------------------------------------------------------------

pub(crate) fn track_to_ui(track: &Track, cover_opt: Option<slint::Image>) -> TrackData {
    let duration_str = track
        .duration_seconds
        .map(|d| format!("{}:{:02}", d as u64 / 60, d as u64 % 60))
        .unwrap_or_else(|| "--:--".to_string());
    let has_cover = cover_opt.is_some();
    TrackData {
        path: track.audio_path.clone().into(),
        artist: track.artist.clone().into(),
        title: track.title.clone().into(),
        album: track.album.clone().into(),
        duration: duration_str.into(),
        status: SharedString::from(match track.status {
            LyricsStatus::Synced => "synced",
            LyricsStatus::Plain => "plain",
            LyricsStatus::Missing => "missing",
            LyricsStatus::Suspicious => "suspicious",
        }),
        has_lyrics: matches!(track.status, LyricsStatus::Synced | LyricsStatus::Plain),
        cover: cover_opt.unwrap_or_default(),
        has_cover,
    }
}

/// Cards per row in the album grid. Fixed so the view can compute which album
/// belongs in which cell without knowing the window width.
pub(crate) const ALBUM_GRID_COLUMNS: usize = 5;

/// Row indices for the album grid: one per row of `ALBUM_GRID_COLUMNS` cards.
pub(crate) fn album_rows(album_count: usize) -> Vec<i32> {
    let rows = album_count.div_ceil(ALBUM_GRID_COLUMNS);
    (0..rows as i32).collect()
}

pub(crate) fn build_album_data(tracks: &[Track], art_loader: &ArtLoader) -> Vec<AlbumData> {
    let mut albums = BTreeMap::<String, (String, usize, usize, Track)>::new();
    for track in tracks {
        let entry = albums
            .entry(track.album.clone())
            .or_insert_with(|| (track.artist.clone(), 0, 0, track.clone()));
        entry.1 += 1;
        if track.status == LyricsStatus::Synced {
            entry.2 += 1;
        }
    }
    albums
        .into_iter()
        .enumerate()
        .map(|(idx, (name, (artist, count, synced, first_track)))| {
            let cover_opt = if let Some(known) =
                crate::art_loader::cached_cover_image(&crate::art_loader::album_image_key(&name))
            {
                known
            } else if idx < 30 {
                album_cover_image_now(&name, art_loader).or_else(|| {
                    cover_for(
                        &track_art_key(&first_track.audio_path),
                        &first_track.audio_path,
                        art_stamp(&first_track),
                        art_loader,
                        true,
                    )
                })
            } else {
                album_cover_image(&name, art_loader)
            };
            let has_cover = cover_opt.is_some();
            AlbumData {
                path: first_track.audio_path.clone().into(),
                name: name.into(),
                artist: artist.into(),
                track_count: count as i32,
                synced_count: synced as i32,
                cover: cover_opt.unwrap_or_default(),
                has_cover,
            }
        })
        .collect()
}

pub(crate) fn build_artist_data(tracks: &[Track]) -> Vec<ArtistData> {
    let mut artists = BTreeMap::<String, (std::collections::BTreeSet<String>, usize, usize)>::new();
    for track in tracks {
        let entry = artists
            .entry(track.artist.clone())
            .or_insert_with(|| (std::collections::BTreeSet::new(), 0, 0));
        entry.0.insert(track.album.clone());
        entry.1 += 1;
        if track.status == LyricsStatus::Synced {
            entry.2 += 1;
        }
    }
    artists
        .into_iter()
        .map(|(name, (album_set, count, synced))| ArtistData {
            name: name.into(),
            track_count: count as i32,
            album_count: album_set.len() as i32,
            synced_count: synced as i32,
        })
        .collect()
}

pub(crate) fn build_summary(tracks: &[Track]) -> String {
    let (mut synced, mut plain, mut missing) = (0, 0, 0);
    for t in tracks {
        match t.status {
            LyricsStatus::Synced => synced += 1,
            LyricsStatus::Plain => plain += 1,
            _ => missing += 1,
        }
    }
    format!(
        "{} tracks | {} synced | {} plain | {} missing",
        tracks.len(),
        synced,
        plain,
        missing
    )
}

pub(crate) fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub(crate) fn current_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hour = (secs / 3600) % 24;
    let min = (secs / 60) % 60;
    let sec = secs % 60;
    format!("{:02}:{:02}:{:02}", hour, min, sec)
}

pub(crate) fn format_unix_timestamp(secs: i64) -> String {
    let secs = secs.max(0) as u64;
    let hour = (secs / 3600) % 24;
    let min = (secs / 60) % 60;
    let sec = secs % 60;
    format!("{:02}:{:02}:{:02}", hour, min, sec)
}

pub(crate) fn open_database() -> Result<LibraryDb, String> {
    let data_dir = app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    LibraryDb::open(data_dir.join("library.sqlite3")).map_err(|e| e.to_string())
}

pub(crate) fn load_library_into_ui(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &ArtLoader,
) {
    let Ok(db) = open_database() else { return };
    let Ok(all_tracks) = db.tracks() else { return };
    let Ok(dirs) = db.directories() else { return };

    art_loader.remember_library(&all_tracks);

    let albums = build_album_data(&all_tracks, art_loader);
    let artists = build_artist_data(&all_tracks);
    let summary = build_summary(&all_tracks);

    let old_covers: HashMap<String, (slint::Image, bool)> = UI_LIBRARY.with(|lib| {
        lib.borrow()
            .tracks
            .iter()
            .filter(|td| td.has_cover)
            .map(|td| (td.path.to_string(), (td.cover.clone(), td.has_cover)))
            .collect()
    });

    let ui_tracks: Vec<TrackData> = all_tracks
        .iter()
        .enumerate()
        .map(|(idx, t)| {
            if let Some((cover, has_cover)) = old_covers.get(&t.audio_path) {
                let mut data = track_to_ui(t, Some(cover.clone()));
                data.has_cover = *has_cover;
                data
            } else if idx < 30 {
                track_to_ui(t, track_cover_now(t, art_loader))
            } else {
                track_to_ui(t, track_cover(t, art_loader))
            }
        })
        .collect();

    UI_LIBRARY.with(|lib| {
        lib.borrow_mut().set_tracks(ui_tracks);
    });

    let search_text = window.get_search_text();
    let status_filter = window.get_status_filter();

    let (visible_tracks, display_summary) = if let Ok(mut state) = app_state.lock() {
        state.all_tracks = all_tracks.clone();
        state.rebuild_filter(search_text.as_str(), status_filter);
        let filtered = UI_LIBRARY.with(|lib| lib.borrow().get_filtered(&state.filtered_tracks));
        let sum_text = if status_filter == 0 && search_text.is_empty() {
            summary
        } else {
            format!("{} tracks (filtered)", state.filtered_tracks.len())
        };
        (filtered, sum_text)
    } else {
        let all_visible = UI_LIBRARY.with(|lib| lib.borrow().tracks.clone());
        (all_visible, summary)
    };

    art_loader.remember_library_rows(&visible_tracks);
    art_loader.remember_album_cards(&albums);
    window.set_tracks(ModelRc::new(VecModel::from(visible_tracks)));
    window.set_album_rows(ModelRc::new(VecModel::from(album_rows(albums.len()))));
    window.set_albums(ModelRc::new(VecModel::from(albums)));
    window.set_artists(ModelRc::new(VecModel::from(artists)));
    window.set_tracks_summary(display_summary.into());

    index_library_art(&all_tracks, art_loader);
    crate::load_health_into_ui(window, &all_tracks);

    let has_directories = !dirs.is_empty();
    window.set_settings_music_dirs(ModelRc::new(VecModel::from(
        dirs.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
    if has_directories {
        window.set_directories_open(false);
    }
}

pub(crate) fn wire_search_and_filters(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let loader = Arc::clone(art_loader);
        window.on_search_changed(move |text| {
            if let (Some(w), Ok(mut s)) = (weak.upgrade(), state.lock()) {
                s.rebuild_filter(text.as_str(), w.get_status_filter());
                let filtered: Vec<TrackData> =
                    UI_LIBRARY.with(|lib| lib.borrow().get_filtered(&s.filtered_tracks));
                loader.remember_library_rows(&filtered);
                w.set_tracks(ModelRc::new(VecModel::from(filtered)));
                let summary_text = if w.get_status_filter() == 0 && text.is_empty() {
                    build_summary(&s.all_tracks)
                } else {
                    format!("{} tracks (filtered)", s.filtered_tracks.len())
                };
                w.set_tracks_summary(summary_text.into());
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let loader = Arc::clone(art_loader);
        window.on_filter_changed(move |filter_idx| {
            if let (Some(w), Ok(mut s)) = (weak.upgrade(), state.lock()) {
                s.rebuild_filter(w.get_search_text().as_str(), filter_idx);
                let filtered: Vec<TrackData> =
                    UI_LIBRARY.with(|lib| lib.borrow().get_filtered(&s.filtered_tracks));
                loader.remember_library_rows(&filtered);
                w.set_tracks(ModelRc::new(VecModel::from(filtered)));
                let summary_text = if filter_idx == 0 && w.get_search_text().is_empty() {
                    build_summary(&s.all_tracks)
                } else {
                    format!("{} tracks (filtered)", s.filtered_tracks.len())
                };
                w.set_tracks_summary(summary_text.into());
            }
        });
    }
}

pub(crate) fn wire_album_and_artist_views(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &Arc<ArtLoader>,
) {
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        window.on_open_album(move |album_idx| {
            let Some(w) = weak.upgrade() else { return };
            let albums_model = w.get_albums();
            let Some(album) = albums_model.row_data(album_idx as usize) else {
                return;
            };
            let album_name = album.name.to_string();
            let mut album_cover_opt: Option<slint::Image> = if album.has_cover {
                Some(album.cover.clone())
            } else {
                album_cover_image_now(&album_name, &cache)
            };
            if let Ok(mut s) = state.lock() {
                let first_track = s.all_tracks.iter().find(|t| t.album == album_name).cloned();
                if album_cover_opt.is_none() {
                    if let Some(first_track) = &first_track {
                        album_cover_opt = track_cover_now(first_track, &cache);
                    }
                }
                let mut detail_rows: Vec<usize> = Vec::new();
                let album_tracks: Vec<TrackData> = s
                    .all_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.album == album_name)
                    .map(|(i, t)| {
                        detail_rows.push(i);
                        track_to_ui(t, track_cover_now(t, &cache))
                    })
                    .collect();
                s.album_detail_tracks = detail_rows;
                cache.remember_album_detail_rows(&album_tracks);
                w.set_album_tracks(ModelRc::new(VecModel::from(album_tracks)));
            }
            if let Some(cover) = album_cover_opt {
                w.set_selected_album_cover(cover);
                w.set_selected_album_has_cover(true);
            } else {
                w.set_selected_album_cover(slint::Image::default());
                w.set_selected_album_has_cover(false);
            }
            w.set_album_detail_mode(true);
            w.set_selected_album_index(album_idx);
        });
    }
    {
        let weak = window.as_weak();
        window.on_back_to_album_grid(move || {
            if let Some(w) = weak.upgrade() {
                w.set_album_detail_mode(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        window.on_download_album(move |_album_idx| {
            if let Some(w) = weak.upgrade() {
                load_library_into_ui(&w, &state, &cache);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        window.on_select_artist(move |artist_idx| {
            let Some(w) = weak.upgrade() else { return };
            let artists_model = w.get_artists();
            let Some(artist) = artists_model.row_data(artist_idx as usize) else {
                return;
            };
            let artist_name = artist.name.to_string();
            if let Ok(mut s) = state.lock() {
                let mut detail_rows: Vec<usize> = Vec::new();
                let artist_tracks: Vec<TrackData> = s
                    .all_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.artist == artist_name)
                    .map(|(i, t)| {
                        detail_rows.push(i);
                        track_to_ui(t, track_cover_now(t, &cache))
                    })
                    .collect();
                s.artist_detail_tracks = detail_rows;
                cache.remember_artist_detail_rows(&artist_tracks);
                w.set_artist_tracks(ModelRc::new(VecModel::from(artist_tracks)));
            }
            w.set_selected_artist_index(artist_idx);
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(app_state);
        let cache = Arc::clone(art_loader);
        window.on_download_artist(move |_artist_idx| {
            if let Some(w) = weak.upgrade() {
                load_library_into_ui(&w, &state, &cache);
            }
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn format_time_formats_correctly() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(3661.0), "61:01");
    }

    #[test]
    fn album_grid_rows_cover_every_album() {
        for count in [0usize, 1, 4, 5, 6, 12, 118] {
            let rows = album_rows(count);
            assert!(
                rows.len() * ALBUM_GRID_COLUMNS >= count,
                "{count} albums need at least {} rows, got {}",
                count.div_ceil(ALBUM_GRID_COLUMNS),
                rows.len()
            );
        }
        assert!(album_rows(0).is_empty());
        assert_eq!(album_rows(ALBUM_GRID_COLUMNS).len(), 1);
    }

    #[test]
    fn build_album_data_groups_correctly() {
        let tracks = vec![
            Track {
                audio_path: "a.mp3".into(),
                lrc_path: "a.lrc".into(),
                artist: "A".into(),
                title: "T1".into(),
                album: "Album1".into(),
                duration_seconds: Some(100.0),
                status: LyricsStatus::Synced,
                mtime: None,
                last_checked: None,
                suspicious_reason: None,
                lyrics_offset_ms: 0,
            },
            Track {
                audio_path: "b.mp3".into(),
                lrc_path: "b.lrc".into(),
                artist: "A".into(),
                title: "T2".into(),
                album: "Album1".into(),
                duration_seconds: Some(200.0),
                status: LyricsStatus::Missing,
                mtime: None,
                last_checked: None,
                suspicious_reason: None,
                lyrics_offset_ms: 0,
            },
        ];
        let loader = ArtLoader::new();
        let albums = build_album_data(&tracks, &loader);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].track_count, 2);
        assert_eq!(albums[0].synced_count, 1);
        assert!(!albums[0].has_cover);
    }

    #[test]
    fn build_artist_data_groups_correctly() {
        let tracks = vec![Track {
            audio_path: "a.mp3".into(),
            lrc_path: "a.lrc".into(),
            artist: "The Beatles".into(),
            title: "T1".into(),
            album: "Abbey Road".into(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Synced,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        }];
        let artists = build_artist_data(&tracks);
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "The Beatles");
        assert_eq!(artists[0].track_count, 1);
        assert_eq!(artists[0].album_count, 1);
    }
}
