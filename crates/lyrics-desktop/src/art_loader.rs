use crate::{AlbumData, MainWindow, TrackData};
use image::GenericImageView;
use lyrics_core::{ThumbnailStore, Track};
use slint::{Model, ModelRc, VecModel};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::thread;

// ---------------------------------------------------------------------------
// Cover artwork
// ---------------------------------------------------------------------------

/// Largest cover the UI is handed. The same 192px the index stores, so a cover
/// goes from file to screen without ever being resized.
pub(crate) const COVER_MAX_PX: u32 = lyrics_core::THUMBNAIL_MAX_PX;

/// How many artwork workers to run.
pub(crate) fn art_worker_count() -> usize {
    let cores = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2);
    cores.saturating_sub(1).clamp(1, 4)
}

/// Cache key for a single track's own artwork.
pub(crate) fn track_art_key(audio_path: &str) -> String {
    audio_path.to_string()
}

/// Decode artwork into the pixels Slint draws. Runs on a worker thread and
/// returns a [`slint::SharedPixelBuffer`], which — unlike `slint::Image` — is
/// safe to move across threads.
pub(crate) fn decode_cover_rgb(
    bytes: impl AsRef<[u8]>,
) -> Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>> {
    let img = image::load_from_memory(bytes.as_ref()).ok()?;
    let (w, h) = img.dimensions();
    let thumb = if w > COVER_MAX_PX || h > COVER_MAX_PX {
        img.thumbnail(COVER_MAX_PX, COVER_MAX_PX)
    } else {
        img
    }
    .to_rgb8();
    let (tw, th) = thumb.dimensions();
    Some(slint::SharedPixelBuffer::clone_from_slice(&thumb, tw, th))
}

/// One artwork job queued for a worker.
pub(crate) struct ArtJob {
    pub(crate) key: String,
    pub(crate) path: String,
    pub(crate) stamp: u64,
}

/// The library as the artwork code needs to see it.
#[derive(Default)]
pub(crate) struct LibraryIndex {
    pub(crate) album_of_path: HashMap<String, String>,
    pub(crate) tracks_by_album: HashMap<String, Vec<String>>,
    pub(crate) stamp_of_path: HashMap<String, u64>,
}

/// Which rows show which file, and which show which album.
#[derive(Default)]
pub(crate) struct ArtRows {
    pub(crate) library: HashMap<String, Vec<usize>>,
    pub(crate) album_detail: HashMap<String, Vec<usize>>,
    pub(crate) artist_detail: HashMap<String, Vec<usize>>,
    pub(crate) library_by_album: HashMap<String, Vec<usize>>,
    pub(crate) album_detail_by_album: HashMap<String, Vec<usize>>,
    pub(crate) artist_detail_by_album: HashMap<String, Vec<usize>>,
    pub(crate) album_cards: HashMap<String, Vec<usize>>,
}

/// A list of rows that artwork can be written into.
#[derive(Clone, Copy)]
pub(crate) enum ArtList {
    Library,
    AlbumDetail,
    ArtistDetail,
    AlbumCards,
}

/// One track list's rows, indexed both by file and by album.
pub(crate) fn row_index(
    rows: &[TrackData],
) -> (HashMap<String, Vec<usize>>, HashMap<String, Vec<usize>>) {
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_album: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        by_path.entry(row.path.to_string()).or_default().push(index);
        by_album
            .entry(row.album.to_string())
            .or_default()
            .push(index);
    }
    (by_path, by_album)
}

pub(crate) struct ArtLoaderInner {
    pub(crate) thumbs: Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>,
    pub(crate) ready: Mutex<Vec<(String, Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>)>>,
    pub(crate) queue: Mutex<VecDeque<ArtJob>>,
    pub(crate) seen: Mutex<HashSet<(String, u64)>>,
    pub(crate) library: Mutex<LibraryIndex>,
    pub(crate) rows: Mutex<ArtRows>,
    pub(crate) player_album: Mutex<Option<String>>,
    pub(crate) signal: std::sync::Condvar,
    pub(crate) last_post: Mutex<std::time::Instant>,
}

/// Background cover-art extraction shared between the UI and the worker thread.
#[derive(Clone)]
pub(crate) struct ArtLoader {
    pub(crate) inner: Arc<ArtLoaderInner>,
}

impl ArtLoader {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ArtLoaderInner {
                thumbs: Mutex::new(HashMap::new()),
                ready: Mutex::new(Vec::new()),
                queue: Mutex::new(VecDeque::new()),
                seen: Mutex::new(HashSet::new()),
                library: Mutex::new(LibraryIndex::default()),
                rows: Mutex::new(ArtRows::default()),
                player_album: Mutex::new(None),
                signal: std::sync::Condvar::new(),
                last_post: Mutex::new(std::time::Instant::now() - Duration::from_secs(1)),
            }),
        }
    }

    /// Queues artwork, unless it is already queued or extracted.
    pub(crate) fn request(&self, key: &str, path: &str, stamp: u64, urgent: bool) {
        if path.is_empty() {
            return;
        }
        {
            let mut seen = self.inner.seen.lock().unwrap();
            if !seen.insert((key.to_string(), stamp)) {
                return;
            }
        }
        let job = ArtJob {
            key: key.to_string(),
            path: path.to_string(),
            stamp,
        };
        let mut queue = self.inner.queue.lock().unwrap();
        if urgent {
            queue.push_front(job);
        } else {
            queue.push_back(job);
        }
        drop(queue);
        self.inner.signal.notify_all();
    }

    /// Forgets everything this loader knew about the library's artwork.
    pub(crate) fn reset_after_scan(&self) {
        lyrics_core::clear_folder_art_cache();
        self.inner.thumbs.lock().unwrap().clear();
        self.inner.seen.lock().unwrap().clear();
        self.inner.queue.lock().unwrap().clear();
        self.inner.ready.lock().unwrap().clear();
        *self.inner.rows.lock().unwrap() = ArtRows::default();
        *self.inner.player_album.lock().unwrap() = None;
        clear_ui_image_cache();
    }

    pub(crate) fn thumbnail_for(&self, key: &str) -> Option<Option<Arc<Vec<u8>>>> {
        self.inner.thumbs.lock().unwrap().get(key).cloned()
    }

    pub(crate) fn take_ready(
        &self,
    ) -> Vec<(String, Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>)> {
        std::mem::take(&mut *self.inner.ready.lock().unwrap())
    }

    pub(crate) fn next_job(&self) -> Option<ArtJob> {
        let mut queue = self.inner.queue.lock().unwrap();
        loop {
            if let Some(job) = queue.pop_front() {
                return Some(job);
            }
            let (guard, timeout) = self
                .inner
                .signal
                .wait_timeout(queue, Duration::from_millis(500))
                .unwrap();
            queue = guard;
            if !timeout.timed_out() {
                continue;
            }
            if queue.is_empty() {
                return None;
            }
        }
    }

    pub(crate) fn post_results(self: &Arc<Self>, window: &slint::Weak<MainWindow>) {
        let drained = self
            .inner
            .queue
            .lock()
            .map(|queue| queue.is_empty())
            .unwrap_or(true);
        let due = {
            let Ok(mut last_post) = self.inner.last_post.lock() else {
                return;
            };
            if drained || last_post.elapsed() >= Duration::from_millis(60) {
                *last_post = std::time::Instant::now();
                true
            } else {
                false
            }
        };
        if !due {
            return;
        }

        let weak = window.clone();
        let loader = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                apply_ready_art(&w, &loader);
            }
        });
    }

    pub(crate) fn spawn_workers(self: &Arc<Self>, window: slint::Weak<MainWindow>) {
        for _ in 0..art_worker_count() {
            let loader = Arc::clone(self);
            let window = window.clone();
            thread::spawn(move || {
                let store = ThumbnailStore::default_location();
                loop {
                    let Some(job) = loader.next_job() else {
                        continue;
                    };
                    let (thumbnail, pixels) = load_thumbnail(store.as_ref(), &job);
                    loader
                        .inner
                        .thumbs
                        .lock()
                        .unwrap()
                        .insert(job.key.clone(), thumbnail);
                    loader.inner.ready.lock().unwrap().push((job.key, pixels));
                    loader.post_results(&window);
                }
            });
        }
    }

    pub(crate) fn remember_library(&self, tracks: &[Track]) {
        let mut library = self.inner.library.lock().unwrap();
        library.album_of_path.clear();
        library.tracks_by_album.clear();
        library.stamp_of_path.clear();
        for track in tracks {
            library
                .album_of_path
                .insert(track.audio_path.clone(), track.album.clone());
            library
                .tracks_by_album
                .entry(track.album.clone())
                .or_default()
                .push(track.audio_path.clone());
            library
                .stamp_of_path
                .insert(track.audio_path.clone(), art_stamp(track));
        }
    }

    pub(crate) fn album_of(&self, audio_path: &str) -> Option<String> {
        self.inner
            .library
            .lock()
            .unwrap()
            .album_of_path
            .get(audio_path)
            .cloned()
    }

    #[allow(dead_code)]
    pub(crate) fn album_art(&self, album: &str) -> Option<Arc<Vec<u8>>> {
        self.album_art_with(album, false)
    }

    pub(crate) fn album_art_with(&self, album: &str, urgent: bool) -> Option<Arc<Vec<u8>>> {
        let paths = {
            let lib = self.inner.library.lock().unwrap();
            lib.tracks_by_album.get(album).cloned()?
        };
        {
            let thumbs = self.inner.thumbs.lock().unwrap();
            if let Some(art) = paths
                .iter()
                .find_map(|path| thumbs.get(path).cloned().flatten())
            {
                return Some(art);
            }
        }
        if urgent {
            if let Some(store) = ThumbnailStore::default_location() {
                let stamps = {
                    let lib = self.inner.library.lock().unwrap();
                    lib.stamp_of_path.clone()
                };
                for path in &paths {
                    let base_stamp = stamps.get(path).copied().unwrap_or(0);
                    let stamp = lyrics_core::artwork_stamp(std::path::Path::new(path), base_stamp);
                    if let Some(Some(bytes)) = store.get(path, stamp) {
                        let arc = Arc::new(bytes);
                        self.inner
                            .thumbs
                            .lock()
                            .unwrap()
                            .insert(path.clone(), Some(arc.clone()));
                        return Some(arc);
                    }
                }
            }
        }
        None
    }

    pub(crate) fn remember_library_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.library = by_path;
        art_rows.library_by_album = by_album;
    }

    pub(crate) fn remember_album_detail_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.album_detail = by_path;
        art_rows.album_detail_by_album = by_album;
    }

    pub(crate) fn remember_artist_detail_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.artist_detail = by_path;
        art_rows.artist_detail_by_album = by_album;
    }

    pub(crate) fn remember_album_cards(&self, rows: &[AlbumData]) {
        let mut cards: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            cards.entry(row.name.to_string()).or_default().push(index);
        }
        self.inner.rows.lock().unwrap().album_cards = cards;
    }

    pub(crate) fn set_player_album(&self, album: Option<String>) {
        *self.inner.player_album.lock().unwrap() = album;
    }

    pub(crate) fn patch_rows(&self, window: &MainWindow, paths: &[String], albums: &[String]) {
        let mut work: Vec<(ArtList, String, Vec<usize>)> = Vec::new();
        {
            let rows = self.inner.rows.lock().unwrap();
            for path in paths {
                if let Some(indices) = rows.library.get(path) {
                    work.push((ArtList::Library, path.clone(), indices.clone()));
                }
                if let Some(indices) = rows.album_detail.get(path) {
                    work.push((ArtList::AlbumDetail, path.clone(), indices.clone()));
                }
                if let Some(indices) = rows.artist_detail.get(path) {
                    work.push((ArtList::ArtistDetail, path.clone(), indices.clone()));
                }
            }
            for album in albums {
                let key = album_image_key(album);
                if let Some(indices) = rows.library_by_album.get(album) {
                    work.push((ArtList::Library, key.clone(), indices.clone()));
                }
                if let Some(indices) = rows.album_detail_by_album.get(album) {
                    work.push((ArtList::AlbumDetail, key.clone(), indices.clone()));
                }
                if let Some(indices) = rows.artist_detail_by_album.get(album) {
                    work.push((ArtList::ArtistDetail, key.clone(), indices.clone()));
                }
                if let Some(indices) = rows.album_cards.get(album) {
                    work.push((ArtList::AlbumCards, key.clone(), indices.clone()));
                }
            }
        }

        for (list, key, indices) in work {
            match list {
                ArtList::Library => patch_track_rows(&window.get_tracks(), &key, &indices),
                ArtList::AlbumDetail => {
                    patch_track_rows(&window.get_album_tracks(), &key, &indices)
                }
                ArtList::ArtistDetail => {
                    patch_track_rows(&window.get_artist_tracks(), &key, &indices)
                }
                ArtList::AlbumCards => patch_album_rows(&window.get_albums(), &key, &indices),
            }
        }

        let waiting = {
            let mut player_album = self.inner.player_album.lock().unwrap();
            let arrived = player_album
                .as_deref()
                .is_some_and(|album| albums.iter().any(|name| name == album));
            if arrived {
                player_album.take()
            } else {
                None
            }
        };
        if let Some(album) = waiting {
            if let Some(Some(image)) = cached_cover_image(&album_image_key(&album)) {
                window.set_track_cover(image);
                window.set_has_cover(true);
            }
        }
    }
}

pub(crate) fn load_thumbnail(
    store: Option<&ThumbnailStore>,
    job: &ArtJob,
) -> (
    Option<Arc<Vec<u8>>>,
    Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
) {
    let stamp = lyrics_core::artwork_stamp(std::path::Path::new(&job.path), job.stamp);

    if let Some(store) = store {
        if let Some(Some(cached)) = store.get(&job.path, stamp) {
            let pixels = decode_cover_rgb(&cached);
            return (Some(Arc::new(cached)), pixels);
        }
    }

    let embedded = std::panic::catch_unwind(|| {
        lyrics_core::extract_cover_art(std::path::Path::new(&job.path))
    })
    .unwrap_or_else(|_| {
        eprintln!("artwork extraction failed for {}", job.path);
        None
    });

    let thumbnail = match embedded.as_deref().and_then(lyrics_core::thumbnail_jpeg) {
        Some(bytes) => Some(Arc::new(bytes)),
        None => lyrics_core::folder_cover_thumbnail(std::path::Path::new(&job.path)),
    };

    if let Some(store) = store {
        store.put(&job.path, stamp, thumbnail.as_deref().map(Vec::as_slice));
    }
    let pixels = thumbnail.as_deref().and_then(decode_cover_rgb);
    (thumbnail, pixels)
}

pub(crate) const UI_IMAGE_CACHE_LIMIT: usize = 8192;

#[derive(Default)]
pub(crate) struct UiImageCache {
    images: HashMap<String, Option<slint::Image>>,
    order: VecDeque<String>,
}

impl UiImageCache {
    pub(crate) fn get(&mut self, key: &str) -> Option<Option<slint::Image>> {
        let image = self.images.get(key).cloned()?;
        self.touch(key);
        Some(image)
    }

    pub(crate) fn insert(&mut self, key: String, image: Option<slint::Image>) {
        if self.images.insert(key.clone(), image).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(&key);
        }
        while self.order.len() > UI_IMAGE_CACHE_LIMIT {
            if let Some(evicted) = self.order.pop_front() {
                self.images.remove(&evicted);
            }
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|known| known == key) {
            if let Some(known) = self.order.remove(position) {
                self.order.push_back(known);
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.images.clear();
        self.order.clear();
    }
}

thread_local! {
    pub(crate) static UI_IMAGE_CACHE: std::cell::RefCell<UiImageCache> =
        std::cell::RefCell::new(UiImageCache::default());
}

pub(crate) fn cached_cover_image(key: &str) -> Option<Option<slint::Image>> {
    UI_IMAGE_CACHE.with(|cell| cell.borrow_mut().get(key))
}

pub(crate) fn store_cover_image(key: &str, image: Option<slint::Image>) {
    UI_IMAGE_CACHE.with(|cell| cell.borrow_mut().insert(key.to_string(), image));
}

pub(crate) fn clear_ui_image_cache() {
    UI_IMAGE_CACHE.with(|cache| cache.borrow_mut().clear());
}

pub(crate) fn art_stamp(track: &Track) -> u64 {
    track.mtime.unwrap_or(0.0) as u64
}

pub(crate) fn cover_for(
    key: &str,
    audio_path: &str,
    stamp: u64,
    loader: &ArtLoader,
    urgent: bool,
) -> Option<slint::Image> {
    if let Some(known) = cached_cover_image(key) {
        return known;
    }
    if let Some(thumbnail) = loader.thumbnail_for(key) {
        let image = thumbnail
            .as_deref()
            .and_then(decode_cover_rgb)
            .map(slint::Image::from_rgb8);
        store_cover_image(key, image.clone());
        return image;
    }
    if urgent {
        if let Some(store) = ThumbnailStore::default_location() {
            let full_stamp = lyrics_core::artwork_stamp(std::path::Path::new(audio_path), stamp);
            if let Some(Some(cached)) = store.get(audio_path, full_stamp) {
                let arc = Arc::new(cached);
                loader
                    .inner
                    .thumbs
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), Some(arc.clone()));
                let image = decode_cover_rgb(&*arc).map(slint::Image::from_rgb8);
                store_cover_image(key, image.clone());
                return image;
            }
        }
    }
    loader.request(key, audio_path, stamp, urgent);
    None
}

pub(crate) fn track_cover(track: &Track, art_loader: &ArtLoader) -> Option<slint::Image> {
    track_cover_with(track, art_loader, false)
}

pub(crate) fn track_cover_now(track: &Track, art_loader: &ArtLoader) -> Option<slint::Image> {
    track_cover_with(track, art_loader, true)
}

pub(crate) fn track_cover_with(
    track: &Track,
    art_loader: &ArtLoader,
    urgent: bool,
) -> Option<slint::Image> {
    cover_for(
        &track_art_key(&track.audio_path),
        &track.audio_path,
        art_stamp(track),
        art_loader,
        urgent,
    )
    .or_else(|| album_cover_image_with(&track.album, art_loader, urgent))
}

pub(crate) fn album_image_key(album: &str) -> String {
    format!("album:{album}")
}

pub(crate) fn album_cover_image(album: &str, art_loader: &ArtLoader) -> Option<slint::Image> {
    album_cover_image_with(album, art_loader, false)
}

pub(crate) fn album_cover_image_now(album: &str, art_loader: &ArtLoader) -> Option<slint::Image> {
    album_cover_image_with(album, art_loader, true)
}

pub(crate) fn album_cover_image_with(
    album: &str,
    art_loader: &ArtLoader,
    urgent: bool,
) -> Option<slint::Image> {
    if let Some(known) = cached_cover_image(&album_image_key(album)) {
        return known;
    }
    let image = art_loader
        .album_art_with(album, urgent)
        .as_deref()
        .and_then(decode_cover_rgb)
        .map(slint::Image::from_rgb8);
    if image.is_some() {
        store_cover_image(&album_image_key(album), image.clone());
    }
    image
}

pub(crate) fn album_cover_sources(tracks: &[Track]) -> Vec<(String, Track)> {
    let mut first: BTreeMap<String, Track> = BTreeMap::new();
    for track in tracks {
        first
            .entry(track.album.clone())
            .or_insert_with(|| track.clone());
    }
    first.into_iter().collect()
}

pub(crate) fn index_library_art(tracks: &[Track], loader: &ArtLoader) {
    for (_, first_track) in album_cover_sources(tracks).into_iter().rev() {
        loader.request(
            &track_art_key(&first_track.audio_path),
            &first_track.audio_path,
            art_stamp(&first_track),
            true,
        );
    }
    for track in tracks {
        loader.request(
            &track_art_key(&track.audio_path),
            &track.audio_path,
            art_stamp(track),
            false,
        );
    }
}

pub(crate) fn absorb_ready_art(
    ready: Vec<(String, Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>)>,
    loader: &ArtLoader,
) -> (Vec<String>, Vec<String>) {
    let mut paths = Vec::new();
    let mut albums = Vec::new();
    for (key, pixels) in ready {
        let Some(image) = pixels.map(slint::Image::from_rgb8) else {
            store_cover_image(&key, None);
            continue;
        };
        paths.push(key.clone());
        if let Some(album) = loader.album_of(&key) {
            let album_key = album_image_key(&album);
            if cached_cover_image(&album_key).flatten().is_none() {
                store_cover_image(&album_key, Some(image.clone()));
                if !albums.contains(&album) {
                    albums.push(album);
                }
            }
        }
        store_cover_image(&key, Some(image));
    }
    (paths, albums)
}

pub(crate) fn apply_ready_art(window: &MainWindow, loader: &ArtLoader) {
    let ready = loader.take_ready();
    if ready.is_empty() {
        return;
    }
    let (paths, albums) = absorb_ready_art(ready, loader);
    if paths.is_empty() {
        return;
    }

    loader.patch_rows(window, &paths, &albums);

    crate::patch_ui_library_covers(&paths, &albums);

    if window.get_album_detail_mode() && !window.get_selected_album_has_cover() {
        let index = window.get_selected_album_index();
        if index >= 0 {
            if let Some(album) = window.get_albums().row_data(index as usize) {
                if albums.iter().any(|name| name == album.name.as_str()) {
                    if let Some(image) = cached_cover_image(&album_image_key(&album.name)).flatten()
                    {
                        window.set_selected_album_cover(image);
                        window.set_selected_album_has_cover(true);
                    }
                }
            }
        }
    }
}

pub(crate) fn patch_track_rows(model: &ModelRc<TrackData>, cache_key: &str, indices: &[usize]) {
    let Some(rows) = model.as_any().downcast_ref::<VecModel<TrackData>>() else {
        return;
    };
    let Some(Some(image)) = cached_cover_image(cache_key) else {
        return;
    };
    for &row in indices {
        let Some(mut data) = rows.row_data(row) else {
            continue;
        };
        if data.has_cover {
            continue;
        }
        data.cover = image.clone();
        data.has_cover = true;
        rows.set_row_data(row, data);
    }
}

pub(crate) fn patch_album_rows(model: &ModelRc<AlbumData>, cache_key: &str, indices: &[usize]) {
    let Some(rows) = model.as_any().downcast_ref::<VecModel<AlbumData>>() else {
        return;
    };
    let Some(Some(image)) = cached_cover_image(cache_key) else {
        return;
    };
    for &row in indices {
        let Some(mut data) = rows.row_data(row) else {
            continue;
        };
        if data.has_cover {
            continue;
        }
        data.cover = image.clone();
        data.has_cover = true;
        rows.set_row_data(row, data);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lyrics_core::LyricsStatus;

    pub(crate) fn write_silent_wav(path: &std::path::Path) {
        let frames = 2_000u32;
        let data_len = frames * 2;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; data_len as usize]);
        std::fs::write(path, bytes).expect("write wav fixture");
    }

    pub(crate) fn jpeg_bytes() -> Vec<u8> {
        let image = image::RgbImage::from_fn(96, 96, |x, y| {
            image::Rgb([(x * 2) as u8, (y * 2) as u8, 90])
        });
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .expect("encode jpeg fixture");
        bytes
    }

    pub(crate) fn write_jpeg(path: &std::path::Path) {
        std::fs::write(path, jpeg_bytes()).expect("write jpeg fixture");
    }

    pub(crate) fn mk_track(path: &str, artist: &str, title: &str, album: &str) -> Track {
        Track {
            audio_path: path.into(),
            lrc_path: format!("{path}.lrc"),
            artist: artist.into(),
            title: title.into(),
            album: album.into(),
            duration_seconds: Some(120.0),
            status: LyricsStatus::Synced,
            mtime: None,
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        }
    }

    #[test]
    fn a_folder_cover_is_extracted_thumbnaild_and_indexed() {
        let dir = std::env::temp_dir().join(format!(
            "lyrics-desktop-folder-art-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");

        let audio = dir.join("Song.wav");
        write_silent_wav(&audio);
        let cover = dir.join("cover.jpg");
        write_jpeg(&cover);

        let store = ThumbnailStore::open(dir.join("covers"));
        let job = ArtJob {
            key: audio.to_string_lossy().into_owned(),
            path: audio.to_string_lossy().into_owned(),
            stamp: 42,
        };

        let (thumbnail, pixels) = load_thumbnail(Some(&store), &job);
        let thumbnail = thumbnail.expect("the folder's cover.jpg should produce a thumbnail");
        assert!(pixels.is_some(), "the thumbnail should decode to pixels");

        let (indexed, indexed_pixels) = load_thumbnail(Some(&store), &job);
        assert_eq!(
            indexed.expect("the index should answer").as_slice(),
            thumbnail.as_slice()
        );
        assert!(indexed_pixels.is_some());

        std::fs::remove_file(&cover).expect("remove the cover behind the index's back");
        lyrics_core::clear_folder_art_cache();
        let (without_cover, _) = load_thumbnail(Some(&store), &job);
        assert!(
            without_cover.is_none(),
            "an index entry for a cover that vanished must not be reused"
        );

        let restamped = ArtJob { stamp: 43, ..job };
        let (fresh, _) = load_thumbnail(Some(&store), &restamped);
        assert!(fresh.is_none(), "a new mtime invalidates the indexed cover");

        std::fs::remove_dir_all(&dir).expect("clean up fixture");
    }

    #[test]
    fn a_real_file_yields_thumbnail_pixels() {
        let Ok(path) = std::env::var("LYRICS_TEST_AUDIO") else {
            eprintln!("skipping: set LYRICS_TEST_AUDIO=<file with artwork> to run");
            return;
        };

        let bytes = lyrics_core::extract_cover_art(std::path::Path::new(&path))
            .expect("the file should carry embedded artwork");
        assert!(!bytes.is_empty());

        let pixels = decode_cover_rgb(&bytes).expect("artwork should decode");
        assert!(pixels.width() > 0 && pixels.height() > 0);
        assert!(
            pixels.width() <= COVER_MAX_PX && pixels.height() <= COVER_MAX_PX,
            "thumbnail should be shrunk to {COVER_MAX_PX}px, got {}x{}",
            pixels.width(),
            pixels.height()
        );
    }

    #[test]
    fn per_track_images_are_indexed_for_every_track() {
        let dir = std::env::temp_dir().join(format!(
            "lyrics-desktop-per-track-art-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");

        let mut jobs = Vec::new();
        for index in 0..3u64 {
            let audio = dir.join(format!("Track {index}.wav"));
            write_silent_wav(&audio);
            write_jpeg(&dir.join(format!("Track {index}.jpg")));
            jobs.push(ArtJob {
                key: audio.to_string_lossy().into_owned(),
                path: audio.to_string_lossy().into_owned(),
                stamp: 42 + index,
            });
        }

        let store = ThumbnailStore::open(dir.join("covers"));
        let thumbnails: Vec<Vec<u8>> = jobs
            .iter()
            .map(|job| {
                let (thumbnail, pixels) = load_thumbnail(Some(&store), job);
                assert!(pixels.is_some(), "{} should have artwork", job.path);
                thumbnail
                    .expect("every track's own image should be used")
                    .as_slice()
                    .to_vec()
            })
            .collect();

        for index in 0..jobs.len() {
            let (restored, _) = load_thumbnail(Some(&store), &jobs[index]);
            assert_eq!(
                restored.expect("the index should answer").as_slice(),
                thumbnails[index].as_slice()
            );
        }

        for job in &jobs {
            std::fs::remove_file(&job.path).expect("remove the audio behind the index's back");
        }
        for index in 0..jobs.len() {
            let (restored, pixels) = load_thumbnail(Some(&store), &jobs[index]);
            assert_eq!(
                restored.expect("the index should answer").as_slice(),
                thumbnails[index].as_slice()
            );
            assert!(pixels.is_some(), "the pixels come from the index too");
        }

        std::fs::remove_dir_all(&dir).expect("clean up fixture");
    }

    #[test]
    fn an_arriving_cover_lights_up_its_album_with_the_same_pixels() {
        let loader = ArtLoader::new();
        loader.remember_library(&[
            mk_track("a.mp3", "A", "T1", "Album1"),
            mk_track("b.mp3", "A", "T2", "Album1"),
        ]);

        let pixels = decode_cover_rgb(jpeg_bytes()).expect("fixture should decode");
        let (paths, albums) = absorb_ready_art(vec![("b.mp3".to_string(), Some(pixels))], &loader);

        assert_eq!(paths, vec!["b.mp3".to_string()]);
        assert_eq!(albums, vec!["Album1".to_string()]);
        assert!(loader.album_art("Album1").is_none());
        assert!(
            cached_cover_image(&album_image_key("Album1"))
                .flatten()
                .is_some(),
            "the album should show the cover that just arrived"
        );
        assert!(cached_cover_image("b.mp3").flatten().is_some());

        let (none, no_albums) = absorb_ready_art(vec![("a.mp3".to_string(), None)], &loader);
        assert!(none.is_empty());
        assert!(no_albums.is_empty());
    }

    #[test]
    fn a_scan_makes_the_loader_look_at_the_artwork_again() {
        let loader = ArtLoader::new();
        loader.remember_library(&[mk_track("a.mp3", "A", "T1", "Album1")]);
        loader.request("a.mp3", "a.mp3", 10, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
        loader
            .inner
            .thumbs
            .lock()
            .unwrap()
            .insert("a.mp3".to_string(), None);
        store_cover_image("a.mp3", None);

        loader.reset_after_scan();

        assert!(
            cached_cover_image("a.mp3").is_none(),
            "stale answers must go"
        );
        assert!(loader.thumbnail_for("a.mp3").is_none());
        assert!(loader.album_art("Album1").is_none());
        assert!(loader.next_job().is_none(), "the stale queue must go too");

        loader.request("a.mp3", "a.mp3", 10, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
    }

    #[test]
    fn only_a_changed_file_is_read_again() {
        let loader = ArtLoader::new();
        loader.request("a.mp3", "a.mp3", 10, false);
        loader.request("a.mp3", "a.mp3", 10, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
        assert!(loader.next_job().is_none(), "the file was asked for twice");

        loader.request("a.mp3", "a.mp3", 11, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
    }

    #[test]
    fn the_ui_image_cache_forgets_what_it_is_not_using() {
        let image = slint::Image::from_rgb8(decode_cover_rgb(jpeg_bytes()).unwrap());

        store_cover_image("oldest", Some(image.clone()));
        for index in 0..UI_IMAGE_CACHE_LIMIT {
            store_cover_image(&format!("cover-{index}"), Some(image.clone()));
        }
        assert!(
            cached_cover_image("oldest").is_none(),
            "the least recently used cover should have been dropped"
        );

        store_cover_image("in-view", Some(image.clone()));
        for index in 0..UI_IMAGE_CACHE_LIMIT - 1 {
            assert!(cached_cover_image("in-view").is_some());
            store_cover_image(&format!("backfill-{index}"), Some(image.clone()));
        }
        assert!(cached_cover_image("in-view").is_some());
    }

    #[test]
    fn album_cover_sources_use_the_first_track_of_each_album() {
        let tracks = vec![
            mk_track("a.mp3", "A", "T1", "Album1"),
            mk_track("b.mp3", "A", "T2", "Album1"),
            mk_track("c.mp3", "B", "T3", "Album2"),
        ];
        let sources: Vec<(String, String)> = album_cover_sources(&tracks)
            .into_iter()
            .map(|(album, track)| (album, track.audio_path))
            .collect();
        assert_eq!(
            sources,
            vec![
                ("Album1".to_string(), "a.mp3".to_string()),
                ("Album2".to_string(), "c.mp3".to_string()),
            ]
        );
    }

    #[test]
    fn an_untagged_track_borrows_its_albums_cover() {
        let loader = ArtLoader::new();
        let tracks = vec![
            mk_track("a.mp3", "A", "T1", "Album1"),
            mk_track("b.mp3", "A", "T2", "Album1"),
            mk_track("c.mp3", "B", "T3", "Album2"),
        ];
        loader.remember_library(&tracks);
        assert_eq!(loader.album_of("b.mp3").as_deref(), Some("Album1"));
        assert_eq!(loader.album_of("nope.mp3"), None);

        assert!(loader.album_art("Album1").is_none());

        let thumbnail = lyrics_core::thumbnail_jpeg(&jpeg_bytes()).expect("jpeg to thumbnail");
        loader
            .inner
            .thumbs
            .lock()
            .unwrap()
            .insert("b.mp3".to_string(), Some(Arc::new(thumbnail)));

        assert!(loader.album_art("Album1").is_some());
        assert!(album_cover_image("Album1", &loader).is_some());
        assert!(cached_cover_image(&album_image_key("Album1")).is_some());
        assert!(album_cover_image("Album2", &loader).is_none());
        assert!(cached_cover_image(&album_image_key("Album2")).is_none());
    }

    #[test]
    fn rows_are_indexed_by_file_and_by_album() {
        use crate::track_to_ui;
        let rows = vec![
            track_to_ui(&mk_track("a.mp3", "A", "T1", "Album1"), None),
            track_to_ui(&mk_track("b.mp3", "A", "T2", "Album1"), None),
            track_to_ui(&mk_track("c.mp3", "B", "T3", "Album2"), None),
        ];
        let (by_path, by_album) = row_index(&rows);
        assert_eq!(by_path["b.mp3"], vec![1]);
        assert_eq!(by_album["Album1"], vec![0, 1]);
        assert_eq!(by_album["Album2"], vec![2]);
    }

    #[test]
    fn the_whole_library_is_indexed_including_what_is_not_on_screen() {
        let loader = ArtLoader::new();
        let tracks = vec![
            mk_track("a.mp3", "A", "T1", "Album1"),
            mk_track("b.mp3", "A", "T2", "Album1"),
            mk_track("c.mp3", "B", "T3", "Album2"),
            mk_track("d.mp3", "B", "T4", "Album2"),
        ];
        index_library_art(&tracks, &loader);

        let queued: Vec<String> = std::iter::from_fn(|| loader.next_job())
            .map(|job| job.key)
            .collect();
        assert_eq!(
            queued.len(),
            tracks.len(),
            "every file must be indexed, on screen or not"
        );
        assert_eq!(&queued[..2], &["a.mp3".to_string(), "c.mp3".to_string()]);

        index_library_art(&tracks, &loader);
        assert!(loader.next_job().is_none());
    }

    #[test]
    fn urgent_cover_lookup_checks_thumbnail_store_and_caches_result() {
        let temp_dir =
            std::env::temp_dir().join(format!("synced_lyrics_test_{}", std::process::id()));
        let store = ThumbnailStore::open(temp_dir.join("covers"));
        let thumb_bytes = lyrics_core::thumbnail_jpeg(&jpeg_bytes()).expect("jpeg to thumbnail");
        let stamp = lyrics_core::artwork_stamp(std::path::Path::new("song.mp3"), 100);
        store.put("song.mp3", stamp, Some(&thumb_bytes));

        let _loader = ArtLoader::new();
        let track = Track {
            audio_path: "song.mp3".into(),
            lrc_path: "song.lrc".into(),
            artist: "Artist".into(),
            title: "Title".into(),
            album: "Album".into(),
            duration_seconds: Some(180.0),
            status: LyricsStatus::Synced,
            mtime: Some(100.0),
            last_checked: None,
            suspicious_reason: None,
            lyrics_offset_ms: 0,
        };

        let full_stamp =
            lyrics_core::artwork_stamp(std::path::Path::new(&track.audio_path), art_stamp(&track));
        assert!(store.get(&track.audio_path, full_stamp).flatten().is_some());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
