#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod audio_source;


use image::GenericImageView;
use lofty::prelude::AudioFile;
use lyrics_core::{
    app_data_dir, download_track, load_settings, parse_lines, save_settings,
    scan_directory_incremental, AppSettings, AudioEngine, DownloadEvent, DownloadMode,
    DownloadOutcome, GeniusProvider, LibraryDb, LrcLine, LrclibProvider, LyricsProvider,
    LyricsStatus, MegalobizProvider, MusixmatchProvider, NeteaseProvider, ProviderLyrics,
    ProviderName, ThumbnailStore, Track, WaveformStore,
};

struct ActiveBatchJob {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}
use rodio::Source;
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

struct RodioAudioEngine {
    _stream: Option<rodio::OutputStream>,
    stream_handle: Option<rodio::OutputStreamHandle>,
    sink: Option<rodio::Sink>,
    current_path: Option<String>,
    duration: Option<f64>,
    volume: f32,
    speed: f32,
    /// Live RMS amplitude from the audio stream, updated ~every 2048 samples.
    live_level: audio_source::LiveLevel,
}

impl RodioAudioEngine {
    fn new() -> Self {
        let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
            Ok((s, h)) => (Some(s), Some(h)),
            Err(e) => {
                eprintln!("Audio output device notice: {e}");
                (None, None)
            }
        };
        Self {
            _stream,
            stream_handle,
            sink: None,
            current_path: None,
            duration: None,
            volume: 0.7,
            speed: 1.0,
            live_level: audio_source::LiveLevel::new(),
        }
    }

    fn ensure_handle(&mut self) -> Result<rodio::OutputStreamHandle, String> {
        if let Some(ref h) = self.stream_handle {
            return Ok(h.clone());
        }
        let (s, h) =
            rodio::OutputStream::try_default().map_err(|e| format!("Audio device error: {e}"))?;
        self._stream = Some(s);
        self.stream_handle = Some(h.clone());
        Ok(h)
    }

    fn read_duration(path: &str) -> Option<f64> {
        let tagged = lofty::probe::Probe::open(path).ok()?.read().ok()?;
        let dur = tagged.properties().duration().as_secs_f64();
        (dur > 0.0).then_some(dur)
    }
}

impl AudioEngine for RodioAudioEngine {
    fn play(&mut self, path: &str) -> Result<(), String> {
        self.stop();
        let handle = self.ensure_handle()?;
        // Our own symphonia source, not `rodio::Decoder`: the latter panics on
        // M4A/AAC files (see `audio_source` for the gory details).
        let source = audio_source::FileSource::open(std::path::Path::new(path))?;
        let decoded_duration = source.total_duration().map(|d| d.as_secs_f64());
        // Wrap in LevelCapture so we can read live amplitude from the UI timer.
        let live = audio_source::LiveLevel::new();
        self.live_level = live.clone();
        let captured = audio_source::LevelCapture::new(source, live);
        let sink = rodio::Sink::try_new(&handle).map_err(|e| format!("Sink error: {e}"))?;
        sink.set_volume(self.volume);
        sink.set_speed(self.speed);
        sink.append(captured);
        sink.play();
        self.duration = Self::read_duration(path).or(decoded_duration);
        self.sink = Some(sink);
        self.current_path = Some(path.to_string());
        Ok(())
    }

    fn toggle_pause(&mut self) -> Result<bool, String> {
        if let Some(ref sink) = self.sink {
            if sink.is_paused() {
                sink.play();
                Ok(false)
            } else {
                sink.pause();
                Ok(true)
            }
        } else if let Some(path) = self.current_path.clone() {
            self.play(&path)?;
            Ok(false)
        } else {
            Err("No track selected".to_string())
        }
    }

    fn position_seconds(&self) -> Option<f64> {
        self.sink.as_ref().map(|s| s.get_pos().as_secs_f64())
    }

    fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn seek(&mut self, position_secs: f64) -> Result<(), String> {
        let sink = self.sink.as_ref().ok_or("no track loaded")?;
        sink.try_seek(Duration::from_secs_f64(position_secs))
            .map_err(|e| e.to_string())
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(sink) = &self.sink {
            sink.set_volume(volume);
        }
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        if let Some(sink) = &self.sink {
            sink.set_speed(speed);
        }
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.duration
    }

    fn is_playing(&self) -> bool {
        self.sink
            .as_ref()
            .map(|s| !s.is_paused() && !s.empty())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Cover artwork
// ---------------------------------------------------------------------------
//
// Reading artwork means opening a file and parsing its tags: a few milliseconds
// per track. Doing that while building the track list froze the window for
// seconds on a real library, and opening an album re-read a dozen files before
// it would draw — which is what made albums feel like they "take time to load".
//
// So extraction happens on worker threads. The UI asks for a cover and gets
// whatever is cached right now (`None` → the view draws its placeholder); rows
// are filled in as results arrive.
//
// Three things make that fast enough on a real library:
//
// * Several workers, because the work is per-file and independent. One thread
//   reading 2 000 files is where "the covers take forever" came from.
// * A thumbnail index on disk ([`ThumbnailStore`]), so the second launch reads
//   small JPEGs instead of opening and decoding every audio file again.
// * Targeted updates: a finished cover is written to the one row that shows it,
//   rather than rescanning every model in the app.

/// Largest cover the UI is handed. The same 192px the index stores, so a cover
/// goes from file to screen without ever being resized: a resize per cover is
/// one more full pass over the pixels for no visible gain.
const COVER_MAX_PX: u32 = lyrics_core::THUMBNAIL_MAX_PX;

/// How many artwork workers to run.
///
/// The work is per-file and dominated by reading and decoding image data, so
/// several in parallel turn a minutes-long first index into a few seconds. It is
/// deliberately capped below the core count: this runs *while* the user is
/// scrolling and playing music, and the UI thread must not be competing with six
/// image decoders for a core.
fn art_worker_count() -> usize {
    let cores = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2);
    cores.saturating_sub(1).clamp(1, 4)
}

/// Cache key for a single track's own artwork. Artwork lives inside the audio
/// file, so the file's own path is the natural key.
fn track_art_key(audio_path: &str) -> String {
    audio_path.to_string()
}

// There is deliberately no separate key for album artwork. An album has no file
// of its own: its cover *is* the cover of its first track, so the album is keyed
// by that track's path too. The previous `album:<name>` key had to agree with
// itself in three different places (queueing, the grid, the detail header), and
// the rule for unnamed albums ("Unknown Album", which is most of a real library)
// differed between them — so those albums never found their artwork. Keying by
// path also means an album's cover arrives with the art of a track that is
// already on screen, instead of behind a duplicate job for the same file.

/// Decode artwork into the pixels Slint draws. Runs on a worker thread and
/// returns a [`slint::SharedPixelBuffer`], which — unlike `slint::Image` — is
/// safe to move across threads.
///
/// RGB, not RGBA: a cover is opaque, and the alpha channel nobody looks at is a
/// quarter of every cover the app keeps in memory. On an album grid of a
/// thousand covers that is the difference between a smooth scroll and a stutter.
///
/// Takes anything that looks like bytes (`&[u8]`, `&Vec<u8>`, `Vec<u8>`) so it
/// can be handed straight to `and_then` without a closure at every call site.
fn decode_cover_rgb(bytes: impl AsRef<[u8]>) -> Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>> {
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
struct ArtJob {
    /// Cache key; the audio path, so the artwork of a track and of its album are
    /// the same entry.
    key: String,
    /// File to read the artwork from.
    path: String,
    /// The audio file's modification time. Part of the index key, so re-tagging
    /// a file invalidates its cached thumbnail.
    stamp: u64,
}

/// The library as the artwork code needs to see it.
#[derive(Default)]
struct LibraryIndex {
    /// Audio path -> album name.
    album_of_path: HashMap<String, String>,
    /// Album name -> that album's audio paths, in library order.
    tracks_by_album: HashMap<String, Vec<String>>,
    /// Audio path -> audio file timestamp.
    stamp_of_path: HashMap<String, u64>,
}

/// Which rows show which file, and which show which album.
///
/// A finished thumbnail is written straight into the rows that are waiting for
/// it. Before this existed, every arrival walked every row of every list and
/// cloned its data just to ask whether it needed a cover: over a few thousand
/// tracks that is millions of clones, and it is what made the album grid stutter
/// while the artwork streamed in behind it.
#[derive(Default)]
struct ArtRows {
    /// The library list, the open album's list and the open artist's list, each
    /// keyed by the file the row shows.
    library: HashMap<String, Vec<usize>>,
    album_detail: HashMap<String, Vec<usize>>,
    artist_detail: HashMap<String, Vec<usize>>,
    /// The same lists keyed by album, for the album-cover fallback: a track
    /// whose own tags carry no picture still shows its album's.
    library_by_album: HashMap<String, Vec<usize>>,
    album_detail_by_album: HashMap<String, Vec<usize>>,
    artist_detail_by_album: HashMap<String, Vec<usize>>,
    /// The album grid's cards, by album name.
    album_cards: HashMap<String, Vec<usize>>,
}

/// A list of rows that artwork can be written into.
#[derive(Clone, Copy)]
enum ArtList {
    Library,
    AlbumDetail,
    ArtistDetail,
    AlbumCards,
}

/// One track list's rows, indexed both by file and by album.
fn row_index(rows: &[TrackData]) -> (HashMap<String, Vec<usize>>, HashMap<String, Vec<usize>>) {
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

struct ArtLoaderInner {
    /// key -> thumbnail JPEG bytes; `None` records "this file has none", so we
    /// never read it again. Doubles as the done-list.
    thumbs: Mutex<HashMap<String, Option<Arc<Vec<u8>>>>>,
    /// Decoded thumbnails waiting to be turned into `slint::Image`s, which can
    /// only be created on the UI thread.
    ready: Mutex<Vec<(String, Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>)>>,
    /// Queued work, most urgent first.
    queue: Mutex<std::collections::VecDeque<ArtJob>>,
    /// Keys queued or already extracted, with the stamp they were asked for, so
    /// the same file is never read twice — but a file that changed *is* read
    /// again.
    seen: Mutex<std::collections::HashSet<(String, u64)>>,
    /// Which album each track is on, and which files make up each album.
    library: Mutex<LibraryIndex>,
    /// Which rows show which file or album.
    rows: Mutex<ArtRows>,
    /// The album the player bar is showing while it is still waiting for a
    /// cover, so the artwork can be filled in when it turns up.
    player_album: Mutex<Option<String>>,
    /// Wakes the workers when work is added.
    signal: std::sync::Condvar,
    /// When the workers last handed results to the UI. Shared, so that several
    /// workers still post at most a handful of batches a second between them.
    last_post: Mutex<std::time::Instant>,
}

/// Background cover-art extraction shared between the UI and the worker thread.
#[derive(Clone)]
struct ArtLoader {
    inner: Arc<ArtLoaderInner>,
}

impl ArtLoader {
    fn new() -> Self {
        Self {
            inner: Arc::new(ArtLoaderInner {
                thumbs: Mutex::new(HashMap::new()),
                ready: Mutex::new(Vec::new()),
                queue: Mutex::new(std::collections::VecDeque::new()),
                seen: Mutex::new(std::collections::HashSet::new()),
                library: Mutex::new(LibraryIndex::default()),
                rows: Mutex::new(ArtRows::default()),
                player_album: Mutex::new(None),
                signal: std::sync::Condvar::new(),
                last_post: Mutex::new(std::time::Instant::now() - Duration::from_secs(1)),
            }),
        }
    }

    /// Queues artwork, unless it is already queued or extracted. `urgent` puts
    /// the job at the front: something the user just asked for (the album they
    /// opened) should not wait behind a whole-library backfill.
    fn request(&self, key: &str, path: &str, stamp: u64, urgent: bool) {
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
        // Wake *every* worker, not just one: the library is queued in a single
        // burst, and waking one worker per push leaves the others asleep for the
        // whole run-up, which is most of why covers used to trickle in.
        self.inner.signal.notify_all();
    }

    /// Forgets everything this loader knew about the library's artwork, because
    /// the files may have changed underneath it.
    ///
    /// A scan is when covers appear next to tracks, files get re-tagged and
    /// folders get rearranged — every one of which can turn an answer we have
    /// already given into a lie. Without this, a row remembered as "no artwork
    /// here" keeps its placeholder for the rest of the session, and a re-tagged
    /// file keeps its old cover, which is the other half of why part of a
    /// library showed artwork and the rest showed placeholders.
    ///
    /// Runs on the UI thread (the image cache is thread-local) and is followed
    /// by a reload, which queues the whole library again — this time against an
    /// index whose keys have changed for every file that did.
    fn reset_after_scan(&self) {
        lyrics_core::clear_folder_art_cache();
        self.inner.thumbs.lock().unwrap().clear();
        self.inner.seen.lock().unwrap().clear();
        self.inner.queue.lock().unwrap().clear();
        self.inner.ready.lock().unwrap().clear();
        *self.inner.rows.lock().unwrap() = ArtRows::default();
        *self.inner.player_album.lock().unwrap() = None;
        UI_IMAGE_CACHE.with(|cache| cache.borrow_mut().clear());
    }

    /// Already-extracted thumbnail bytes for `key`. `None` means "not extracted
    /// yet", `Some(None)` means "extracted, this file has no artwork".
    fn thumbnail_for(&self, key: &str) -> Option<Option<Arc<Vec<u8>>>> {
        self.inner.thumbs.lock().unwrap().get(key).cloned()
    }

    fn take_ready(&self) -> Vec<(String, Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>)> {
        std::mem::take(&mut *self.inner.ready.lock().unwrap())
    }

    /// Waits for the next job, or for the shutter to come back up. `None` means
    /// the queue was drained, so there is nothing to do but wait again.
    fn next_job(&self) -> Option<ArtJob> {
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

    /// Hands finished thumbnails to the UI thread, in batches: this keeps a
    /// 2 000-track library from posting thousands of event-loop messages.
    fn post_results(self: &Arc<Self>, window: &slint::Weak<MainWindow>) {
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

    /// Spawns the artwork workers. They share the queue, so they stay busy until
    /// the whole library has been looked at.
    fn spawn_workers(self: &Arc<Self>, window: slint::Weak<MainWindow>) {
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

    /// Remembers which tracks make up each album.
    ///
    /// Album artwork is not a thing a file has: it is a property of the album,
    /// and a real library is half untagged. Knowing the album of every track is
    /// what lets a cover found on one file be shown on all of them, instead of
    /// leaving gaps wherever the tags happen to be empty.
    fn remember_library(&self, tracks: &[Track]) {
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

    /// The album a file belongs to.
    fn album_of(&self, audio_path: &str) -> Option<String> {
        self.inner
            .library
            .lock()
            .unwrap()
            .album_of_path
            .get(audio_path)
            .cloned()
    }

    /// An album's artwork, found through whichever of its tracks turned out to
    /// have some. `None` means every one of the album's files is either still
    /// unexamined or genuinely has no picture.
    #[allow(dead_code)]
    fn album_art(&self, album: &str) -> Option<Arc<Vec<u8>>> {
        self.album_art_with(album, false)
    }

    fn album_art_with(&self, album: &str, urgent: bool) -> Option<Arc<Vec<u8>>> {
        let paths = {
            let lib = self.inner.library.lock().unwrap();
            lib.tracks_by_album.get(album).cloned()?
        };
        {
            let thumbs = self.inner.thumbs.lock().unwrap();
            if let Some(art) = paths.iter().find_map(|path| thumbs.get(path).cloned().flatten()) {
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

    /// Records where the library list shows each file. The list is rebuilt on
    /// every search keystroke, so this runs with it.
    fn remember_library_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.library = by_path;
        art_rows.library_by_album = by_album;
    }

    /// The open album's track list. The whole list is one album, so its rows are
    /// indexed by album too — that is what fills in the rows of files that carry
    /// no picture of their own.
    fn remember_album_detail_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.album_detail = by_path;
        art_rows.album_detail_by_album = by_album;
    }

    fn remember_artist_detail_rows(&self, rows: &[TrackData]) {
        let (by_path, by_album) = row_index(rows);
        let mut art_rows = self.inner.rows.lock().unwrap();
        art_rows.artist_detail = by_path;
        art_rows.artist_detail_by_album = by_album;
    }

    /// The album grid's cards.
    fn remember_album_cards(&self, rows: &[AlbumData]) {
        let mut cards: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            cards.entry(row.name.to_string()).or_default().push(index);
        }
        self.inner.rows.lock().unwrap().album_cards = cards;
    }

    /// The album the player bar is showing, while it is still waiting for a
    /// cover. `None` once it has one.
    fn set_player_album(&self, album: Option<String>) {
        *self.inner.player_album.lock().unwrap() = album;
    }

    /// Writes the covers that just became available into the rows that show
    /// them — and only into those rows.
    fn patch_rows(&self, window: &MainWindow, paths: &[String], albums: &[String]) {
        // Collected under the lock and applied after it is dropped: writing a
        // row notifies the UI, and no lock should be held across that.
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

        // The player bar may have been waiting for exactly this album's cover.
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

/// The thumbnail for one track: from the on-disk index when it is there,
/// otherwise read the artwork out of the file (or from the folder beside it),
/// decode it, and record the result for the next launch.
fn load_thumbnail(
    store: Option<&ThumbnailStore>,
    job: &ArtJob,
) -> (
    Option<Arc<Vec<u8>>>,
    Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
) {
    // The key covers the audio file *and* the images beside it, so a `cover.jpg`
    // dropped into a folder invalidates exactly the entries it should — and a
    // track the index remembered as having no artwork is looked at again.
    let stamp = lyrics_core::artwork_stamp(std::path::Path::new(&job.path), job.stamp);

    if let Some(store) = store {
        if let Some(Some(cached)) = store.get(&job.path, stamp) {
            let pixels = decode_cover_rgb(&cached);
            return (Some(Arc::new(cached)), pixels);
        }
    }

    // A single bad file must not take the worker down with it: a panic here used
    // to end that thread, and every cover after it would never be extracted.
    let embedded = std::panic::catch_unwind(|| {
        lyrics_core::extract_cover_art(std::path::Path::new(&job.path))
    })
    .unwrap_or_else(|_| {
        eprintln!("artwork extraction failed for {}", job.path);
        None
    });

    // A file whose tags carry no picture still has one when the folder does —
    // and every track on an album shares that same file, so it is read and
    // decoded once for the album rather than once per song.
    let thumbnail = match embedded.as_deref().and_then(lyrics_core::thumbnail_jpeg) {
        Some(bytes) => Some(Arc::new(bytes)),
        None => lyrics_core::folder_cover_thumbnail(std::path::Path::new(&job.path)),
    };

    // Store the *thumbnail*, not the original artwork: a couple of dozen
    // kilobytes per track instead of the hundreds the tags usually carry.
    if let Some(store) = store {
        store.put(&job.path, stamp, thumbnail.as_deref().map(Vec::as_slice));
    }
    let pixels = thumbnail.as_deref().and_then(decode_cover_rgb);
    (thumbnail, pixels)
}

/// How many decoded covers the UI keeps ready to hand out.
///
/// Each one is a small RGB buffer, but a library of a few thousand albums has a
/// few thousand of them, and there is no point holding artwork the user has
/// scrolled past long ago: unbounded, this cache is what makes an album grid
/// heavy to scroll. A cover that falls off the end is decoded again when a row
/// asks for it, which costs a millisecond — less than a dropped frame.
const UI_IMAGE_CACHE_LIMIT: usize = 8192;

/// UI-thread image cache: key -> `Option<Image>`, where a key that is not there
/// means "not looked up yet" and `None` means "no artwork".
///
/// `slint::Image` belongs to the UI thread, so this cannot be shared with the
/// workers. Least-recently-used order matters here: the covers in view have to
/// survive a backfill of the ones that are not.
#[derive(Default)]
struct UiImageCache {
    images: HashMap<String, Option<slint::Image>>,
    /// Keys, least recently used first.
    order: std::collections::VecDeque<String>,
}

impl UiImageCache {
    fn get(&mut self, key: &str) -> Option<Option<slint::Image>> {
        let image = self.images.get(key).cloned()?;
        self.touch(key);
        Some(image)
    }

    fn insert(&mut self, key: String, image: Option<slint::Image>) {
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

    fn clear(&mut self) {
        self.images.clear();
        self.order.clear();
    }
}

#[derive(Default)]
struct UiLibrary {
    tracks: Vec<TrackData>,
    path_to_index: HashMap<String, usize>,
    album_to_indices: HashMap<String, Vec<usize>>,
}

impl UiLibrary {
    fn set_tracks(&mut self, tracks: Vec<TrackData>) {
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

    fn patch_covers(&mut self, paths: &[String], albums: &[String]) {
        for path in paths {
            if let Some(&idx) = self.path_to_index.get(path) {
                if let Some(Some(img)) = cached_cover_image(path) {
                    if let Some(track) = self.tracks.get_mut(idx) {
                        track.cover = img;
                        track.has_cover = true;
                    }
                }
            }
        }
        for album in albums {
            if let Some(indices) = self.album_to_indices.get(album) {
                if let Some(Some(img)) = cached_cover_image(&album_image_key(album)) {
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

    fn get_filtered(&self, indices: &[usize]) -> Vec<TrackData> {
        indices
            .iter()
            .filter_map(|&i| self.tracks.get(i).cloned())
            .collect()
    }
}

thread_local! {
    static UI_IMAGE_CACHE: std::cell::RefCell<UiImageCache> =
        std::cell::RefCell::new(UiImageCache::default());
    static UI_LIBRARY: std::cell::RefCell<UiLibrary> =
        std::cell::RefCell::new(UiLibrary::default());
}

fn cached_cover_image(key: &str) -> Option<Option<slint::Image>> {
    UI_IMAGE_CACHE.with(|cell| cell.borrow_mut().get(key))
}

fn store_cover_image(key: &str, image: Option<slint::Image>) {
    UI_IMAGE_CACHE.with(|cell| cell.borrow_mut().insert(key.to_string(), image));
}

/// The stamp that ties a cached thumbnail to the file it was read from.
fn art_stamp(track: &Track) -> u64 {
    track.mtime.unwrap_or(0.0) as u64
}

/// The one way views ask for artwork. Returns immediately: cached art if we have
/// it, otherwise a placeholder plus a queued extraction job.
fn cover_for(
    key: &str,
    audio_path: &str,
    stamp: u64,
    loader: &ArtLoader,
    urgent: bool,
) -> Option<slint::Image> {
    if let Some(known) = cached_cover_image(key) {
        return known;
    }
    // Already extracted, just not decoded for the UI yet.
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

/// Folds a batch of freshly decoded covers into the UI cache and reports what
/// has to be repainted: the files whose own artwork arrived, and the albums that
/// gained a cover because of it.
///
/// Only arrivals that actually produced a picture can change a row. Most batches
/// on a library full of untagged files are "no artwork here", and those cost
/// nothing here.
///
/// Nothing is decoded on this thread. The worker that read a cover has already
/// turned it into pixels, and those same pixels become the album's cover too:
/// decoding the album's picture a second time — once for the row that arrived,
/// once for the grid card — stalls the UI thread exactly while a library is
/// streaming in behind it.
fn absorb_ready_art(
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
        // The first file of an album to produce a picture gives the album its
        // cover — the very same image, shared rather than decoded again — and
        // with it every one of the album's other rows.
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

/// [absorb_ready_art] decides what a batch of arrivals means; this puts the
/// result on screen. They are split because the decision — which rows a cover
/// belongs to, and whether the pixels can be shared — is the part with the
/// performance in it, and it is worth testing without a window.
fn apply_ready_art(window: &MainWindow, loader: &ArtLoader) {
    let ready = loader.take_ready();
    if ready.is_empty() {
        return;
    }
    let (paths, albums) = absorb_ready_art(ready, loader);
    if paths.is_empty() {
        return;
    }

    loader.patch_rows(window, &paths, &albums);

    UI_LIBRARY.with(|lib| {
        lib.borrow_mut().patch_covers(&paths, &albums);
    });

    // The open album's large cover, if it was still missing when it opened.
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

/// Fills in covers for the rows of a track list that are waiting for one, and
/// only for those rows.
fn patch_track_rows(model: &ModelRc<TrackData>, cache_key: &str, indices: &[usize]) {
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
        // A row that already has a cover keeps it: its own artwork, or the first
        // of its album's to arrive, whichever got there first.
        if data.has_cover {
            continue;
        }
        data.cover = image.clone();
        data.has_cover = true;
        rows.set_row_data(row, data);
    }
}

/// The same for the album grid, whose cards show the album's artwork.
fn patch_album_rows(model: &ModelRc<AlbumData>, cache_key: &str, indices: &[usize]) {
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

// ---------------------------------------------------------------------------
// Waveform extraction, caching, and sound-reactive playback (Send-safe)
// ---------------------------------------------------------------------------
//
// Two things are drawn from one array. The array is the track's real loudness
// envelope — `ENVELOPE_BUCKETS` peaks spread evenly over the whole file, which
// is what [`audio_source::waveform_bars`] returns. From it we derive:
//
// * the static shape of the waveform (`WAVEFORM_BARS` bars, one per drawn bar), and
// * the live level at the playhead, which is what makes the bars move *with the
//   music* rather than to a sine wave that only pretends to.
//
// Both are recomputed on every animation tick, so the drawing follows playback
// instead of restarting a canned animation each time.

/// Decoded per-track envelopes, keyed by audio path. Shared through an `Arc` so
/// the playback timer can read one 30 times a second without copying it.
type WaveformCacheMap = Arc<Mutex<HashMap<String, Arc<Vec<f32>>>>>;

/// Bars drawn in the seek bar. Fixed: the model is written in place every tick,
/// so its length must never change while the UI is bound to it.
pub(crate) const WAVEFORM_BARS: usize = 72;
/// Loudness buckets kept per track — about 120 ms each on a 3-minute song, which
/// is fine enough for the bars to react to a drum hit.
const ENVELOPE_BUCKETS: usize = 1500;
/// How far behind the playhead (in track fraction) the live level is applied.
const PULSE_WIDTH: f32 = 0.12;

/// A clean, flat baseline shape used before a track's envelope has been decoded
/// (and for files that cannot be decoded at all).
fn synthetic_shape(count: usize) -> Vec<f32> {
    vec![0.15f32; count]
}

fn default_waveform() -> Vec<f32> {
    synthetic_shape(WAVEFORM_BARS)
}

/// Peak of each display bar's window of `envelope`. Taking the peak rather than
/// the mean keeps short, loud hits visible after down-sampling.
fn envelope_bars(envelope: &[f32], bars: usize) -> Vec<f32> {
    if envelope.is_empty() {
        return synthetic_shape(bars);
    }
    (0..bars)
        .map(|i| {
            let low = i * envelope.len() / bars;
            let high = (((i + 1) * envelope.len() / bars).max(low + 1)).min(envelope.len());
            envelope[low..high].iter().copied().fold(0.0f32, f32::max)
        })
        .collect()
}

/// Writes `values` into the waveform model, touching only the rows that actually
/// changed. Replacing the whole model (or re-setting every row) on every frame is
/// what made the old animation stutter: each write invalidated all 72 bars.
fn write_waveform_bars(model: &VecModel<f32>, values: &[f32]) {
    for (index, &value) in values.iter().enumerate() {
        if index >= model.row_count() {
            break;
        }
        let changed = model
            .row_data(index)
            .map(|current| (current - value).abs() > 0.004)
            .unwrap_or(true);
        if changed {
            model.set_row_data(index, value);
        }
    }
}

/// The bars to draw for `progress` (0..1) through the track. `level` is the
/// live RMS amplitude from the audio thread (0.0–1.0), already on a perceptual
/// (dB) curve.
///
/// Every bar is modulated by the live level — the whole waveform breathes with
/// the music. Bars near the playhead get a stronger push so the waveform also
/// reads as a progress indicator. When paused, the static envelope shape is
/// returned as-is.
fn reactive_bars(envelope: &[f32], progress: f32, level: f32, playing: bool) -> Vec<f32> {
    let mut bars = envelope_bars(envelope, WAVEFORM_BARS);
    if !playing {
        return bars;
    }
    // Square the level for a punchier feel: quiet is *very* small, loud is big.
    let intensity = level * level;

    for (index, bar) in bars.iter_mut().enumerate() {
        let fraction = (index as f32 + 0.5) / WAVEFORM_BARS as f32;
        let base = *bar;

        // Global breath: the bar swings between a tiny floor and its full
        // envelope height. The floor is deliberately very low so that during
        // quiet passages the bars visibly shrink toward the centre line.
        let floor = 0.08;
        let height = floor + (base - floor).max(0.0) * intensity;

        // Local pulse near the playhead: a bump that makes the current
        // position obvious even when the music is quiet.
        let dist = (progress - fraction).abs();
        let near = (1.0 - dist / PULSE_WIDTH).clamp(0.0, 1.0);
        let pulse = near * near * level * 0.5;

        *bar = (height + pulse).clamp(0.06, 1.0);
    }
    bars
}

/// Pushes `values` into the seek bar's waveform model in place. The model is only
/// ever mutated, never replaced, so the bars keep their identity between frames
/// and Slint can animate the change instead of rebuilding the row.
fn set_waveform_bars(window: &MainWindow, values: &[f32]) {
    let model = window.get_waveform_data();
    match model.as_any().downcast_ref::<VecModel<f32>>() {
        Some(vec) => write_waveform_bars(vec, values),
        // The model was replaced wholesale (only tests do this): fall back.
        None => window.set_waveform_data(ModelRc::new(VecModel::from(values.to_vec()))),
    }
}

fn compute_audio_waveform(audio_path: &str) -> Vec<f32> {
    // Decoded through our own source so formats rodio cannot open (M4A) still
    // get a real envelope instead of the synthetic fallback.
    audio_source::waveform_bars(std::path::Path::new(audio_path), ENVELOPE_BUCKETS, || {
        synthetic_shape(ENVELOPE_BUCKETS)
    })
}

// ---------------------------------------------------------------------------
// Cached provider
// ---------------------------------------------------------------------------

struct CachedLyricsProvider {
    lyrics: ProviderLyrics,
}

impl LyricsProvider for CachedLyricsProvider {
    fn name(&self) -> ProviderName {
        self.lyrics.provider
    }
    fn search(&self, _track: &Track) -> Result<Option<ProviderLyrics>, lyrics_core::ProviderError> {
        Ok(Some(self.lyrics.clone()))
    }
}

// ---------------------------------------------------------------------------
// Shared application state
// ---------------------------------------------------------------------------

struct AppState {
    all_tracks: Vec<Track>,
    filtered_tracks: Vec<usize>, // indices into all_tracks
    current_filtered_row: Option<usize>,
    was_playing: bool,
    /// Track indices of the open album / artist, in the order the detail lists
    /// show them, so a click on row N can find its track.
    album_detail_tracks: Vec<usize>,
    artist_detail_tracks: Vec<usize>,
}

impl AppState {
    fn new() -> Self {
        Self {
            all_tracks: Vec::new(),
            filtered_tracks: Vec::new(),
            current_filtered_row: None,
            was_playing: false,
            album_detail_tracks: Vec::new(),
            artist_detail_tracks: Vec::new(),
        }
    }

    fn rebuild_filter(&mut self, search: &str, status_filter: i32) {
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
// Data conversion helpers
// ---------------------------------------------------------------------------

fn track_to_ui(track: &Track, cover_opt: Option<slint::Image>) -> TrackData {
    let duration_str = track
        .duration_seconds
        .map(|d| format!("{}:{:02}", d as u64 / 60, d as u64 % 60))
        .unwrap_or_else(|| "--:--".to_string());
    let has_cover = cover_opt.is_some();
    TrackData {
        // Carried so a row knows which file it belongs to. The artwork of a
        // finished job is written to exactly the rows that name it, which is why
        // the path travels with the row instead of being looked up again.
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
/// belongs in which cell without knowing the window width, and so the cards can
/// keep a constant size the list can measure.
pub(crate) const ALBUM_GRID_COLUMNS: usize = 5;

/// Row indices for the album grid: one per row of `ALBUM_GRID_COLUMNS` cards.
pub(crate) fn album_rows(album_count: usize) -> Vec<i32> {
    let rows = album_count.div_ceil(ALBUM_GRID_COLUMNS);
    (0..rows as i32).collect()
}

/// Cover lookup for one track: cached art, or a queued job. Never reads a file,
/// so it is safe to call while building a list.
fn track_cover(track: &Track, art_loader: &ArtLoader) -> Option<slint::Image> {
    track_cover_with(track, art_loader, false)
}

/// The same, for artwork that has to be there now: it jumps the queue.
fn track_cover_now(track: &Track, art_loader: &ArtLoader) -> Option<slint::Image> {
    track_cover_with(track, art_loader, true)
}

/// A track's own artwork, and failing that the artwork of its album.
///
/// A file whose tags carry no picture is still on an album that has one — the
/// cover of a record does not stop being its cover because one rip of it lost
/// its tags — and showing it is what the user means by "the cover".
fn track_cover_with(track: &Track, art_loader: &ArtLoader, urgent: bool) -> Option<slint::Image> {
    cover_for(
        &track_art_key(&track.audio_path),
        &track.audio_path,
        art_stamp(track),
        art_loader,
        urgent,
    )
    .or_else(|| album_cover_image_with(&track.album, art_loader, urgent))
}

/// UI-cache key for an album's cover. Deliberately not a job key: what gets
/// extracted is always a file, and the album is only how the result is
/// remembered once one of its files has been read.
fn album_image_key(album: &str) -> String {
    format!("album:{album}")
}

/// The cover an album should show: whichever of its tracks turned out to have
/// artwork. Decodes it once and keeps it under the album's own key, so the
/// other tracks of the album — and the grid card — cost nothing.
///
/// Most calls are answered by the cache: a cover that arrives on a worker is
/// stored under the album's key the moment it lands. The decode below is for the
/// cases that come first — a library loaded from the disk index, or an album
/// opened before any of its files has been looked at.
///
/// `None` is never cached: an album with no art yet is still being looked at,
/// and the answer has to be allowed to change.
fn album_cover_image(album: &str, art_loader: &ArtLoader) -> Option<slint::Image> {
    album_cover_image_with(album, art_loader, false)
}

fn album_cover_image_now(album: &str, art_loader: &ArtLoader) -> Option<slint::Image> {
    album_cover_image_with(album, art_loader, true)
}

fn album_cover_image_with(
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

/// Each album's first track, which is where its artwork comes from.
fn album_cover_sources(tracks: &[Track]) -> Vec<(String, Track)> {
    let mut first: BTreeMap<String, Track> = BTreeMap::new();
    for track in tracks {
        first
            .entry(track.album.clone())
            .or_insert_with(|| track.clone());
    }
    first.into_iter().collect()
}

/// Queues the artwork of every track in the library, and does it after every
/// scan and on every start.
///
/// This is what "index the library when it is added" means: every file, on
/// screen or not, is looked at once, and its cover is left on disk as a
/// thumbnail keyed by the file and by the images beside it. The launch after
/// that paints its artwork from those thumbnails instead of opening every audio
/// file again, which is the difference between covers arriving at once and
/// covers trickling in for a minute.
///
/// Album artwork goes first: the Albums tab is where a library usually gets its
/// first look, and one job per album lights up a whole card. `urgent` pushes onto
/// the front of the queue, hence the reverse walk; the rest of the library
/// follows behind it.
fn index_library_art(tracks: &[Track], loader: &ArtLoader) {
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

fn build_album_data(tracks: &[Track], art_loader: &ArtLoader) -> Vec<AlbumData> {
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
            // An album's cover is whichever of its tracks has one, so the card
            // lights up as soon as any file on the album has been looked at.
            // For the first 30 visible cards on launch, check cache or on-disk thumbnails immediately.
            let cover_opt = if let Some(known) = cached_cover_image(&album_image_key(&name)) {
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

fn build_artist_data(tracks: &[Track]) -> Vec<ArtistData> {
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

fn build_summary(tracks: &[Track]) -> String {
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

fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn current_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hour = (secs / 3600) % 24;
    let min = (secs / 60) % 60;
    let sec = secs % 60;
    format!("{:02}:{:02}:{:02}", hour, min, sec)
}

fn open_database() -> Result<LibraryDb, String> {
    let data_dir = app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    LibraryDb::open(data_dir.join("library.sqlite3")).map_err(|e| e.to_string())
}

fn providers_for_settings(
    database: &LibraryDb,
    track: &Track,
    settings: &AppSettings,
) -> Result<Vec<Box<dyn LyricsProvider>>, String> {
    let mut providers: Vec<Box<dyn LyricsProvider>> = Vec::new();
    let ttl_seconds = match settings.retry_days {
        0 => 0_u64,
        d => d as u64 * 86400,
    };

    macro_rules! add_provider {
        ($enabled:expr, $name:expr, $cached_name:expr, $ctor:expr) => {
            if $enabled {
                if let Some(lyrics) = database
                    .cached_lyrics(&track.audio_path, $cached_name)
                    .map_err(|e| e.to_string())?
                {
                    providers.push(Box::new(CachedLyricsProvider { lyrics }));
                } else {
                    providers.push(Box::new(
                        $ctor.map_err(|e: lyrics_core::ProviderError| e.to_string())?,
                    ));
                }
            }
        };
    }

    add_provider!(
        settings.lrclib_enabled,
        "LRCLib",
        ProviderName::Lrclib,
        LrclibProvider::new(&settings.lrclib_base_url)
    );
    add_provider!(
        settings.musixmatch_enabled,
        "Musixmatch",
        ProviderName::Musixmatch,
        MusixmatchProvider::new(settings.musixmatch_api_key.clone())
    );
    add_provider!(
        settings.netease_enabled,
        "NetEase",
        ProviderName::Netease,
        NeteaseProvider::new()
    );
    add_provider!(
        settings.megalobiz_enabled,
        "Megalobiz",
        ProviderName::Megalobiz,
        MegalobizProvider::new()
    );
    add_provider!(
        settings.genius_enabled,
        "Genius",
        ProviderName::Genius,
        GeniusProvider::new(settings.genius_api_token.clone())
    );

    let _ = ttl_seconds;
    Ok(providers)
}

// ---------------------------------------------------------------------------
// Track setup helper (updates info, artwork, waveform, lyrics, and plays if requested)
// ---------------------------------------------------------------------------

fn setup_track_for_playback(
    track: &Track,
    window: &MainWindow,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
    auto_play: bool,
) {
    window.set_track_title(track.title.clone().into());
    window.set_track_subtitle(format!("{} • {}", track.artist, track.album).into());
    window.set_has_track(true);

    // Cover art: the track's own, or failing that its album's.
    let cover_opt = track_cover_now(track, art_loader)
        .or_else(|| album_cover_image(&track.album, art_loader));
    if let Some(ref cover) = cover_opt {
        window.set_track_cover(cover.clone());
        window.set_has_cover(true);
        art_loader.set_player_album(None);
    } else {
        window.set_track_cover(slint::Image::default());
        window.set_has_cover(false);
        // Nothing to show yet. The album's artwork may still be being looked
        // for, so remember which album to fill the player in with.
        art_loader.set_player_album(Some(track.album.clone()));
    }

    // Waveform: the track's loudness envelope, decoded once and cached.
    let stamp = track.mtime.unwrap_or(0.0) as u64;
    let path = track.audio_path.clone();

    // 1. Check in-memory cache
    let cached_envelope = {
        let cache = waveform_cache.lock().unwrap();
        cache.get(&path).cloned()
    };

    // 2. Check on-disk WaveformStore if not in memory (instant <0.1ms load)
    let cached_envelope = cached_envelope.or_else(|| {
        let store = WaveformStore::default_location()?;
        let envelope = store.get(&path, stamp)?;
        let arc = Arc::new(envelope);
        if let Ok(mut cache) = waveform_cache.lock() {
            cache.insert(path.clone(), Arc::clone(&arc));
        }
        Some(arc)
    });

    if let Some(envelope) = cached_envelope {
        set_waveform_bars(window, &envelope_bars(&envelope, WAVEFORM_BARS));
    } else {
        // While decoding, set baseline shape — the live animation timer will
        // immediately modulate it with live audio level so it is responsive from frame 1!
        set_waveform_bars(window, &default_waveform());
        let wf_cache = Arc::clone(waveform_cache);
        let weak = window.as_weak();
        let bg_path = path.clone();
        thread::spawn(move || {
            let envelope = Arc::new(compute_audio_waveform(&bg_path));
            if let Some(store) = WaveformStore::default_location() {
                store.put(&bg_path, stamp, &envelope);
            }
            if let Ok(mut cache) = wf_cache.lock() {
                cache.insert(bg_path.clone(), Arc::clone(&envelope));
            }
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    set_waveform_bars(&w, &envelope_bars(&envelope, WAVEFORM_BARS));
                }
            });
        });
    }

    // Load lyrics
    let has_lyrics = if let Ok(contents) = fs::read_to_string(&track.lrc_path) {
        let lines = parse_lines(&contents);
        let is_synced = lines.iter().any(|l| l.timestamp_seconds > 0.0);
        if is_synced {
            let ui_lines: Vec<LyricLine> = lines
                .iter()
                .map(|l| LyricLine {
                    timestamp: l.timestamp_seconds as f32,
                    text: l.text.clone().into(),
                })
                .collect();
            window.set_lyrics_lines(ModelRc::new(VecModel::from(ui_lines)));
            window.set_lyrics_plain_text("".into());
            window.set_lyrics_status("synced".into());
            if let Ok(mut ls) = lyric_state.lock() {
                *ls = lines;
            }
            true
        } else if !contents.trim().is_empty() {
            window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
            window.set_lyrics_plain_text(contents.into());
            window.set_lyrics_status("plain".into());
            if let Ok(mut ls) = lyric_state.lock() {
                ls.clear();
            }
            true
        } else {
            window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
            window.set_lyrics_plain_text("".into());
            window.set_lyrics_status("missing".into());
            if let Ok(mut ls) = lyric_state.lock() {
                ls.clear();
            }
            false
        }
    } else {
        window.set_lyrics_lines(ModelRc::new(VecModel::from(Vec::<LyricLine>::new())));
        window.set_lyrics_plain_text("".into());
        window.set_lyrics_status("missing".into());
        if let Ok(mut ls) = lyric_state.lock() {
            ls.clear();
        }
        false
    };

    // Update has_lyrics property on window for PlayerBar lyrics button visibility
    window.set_has_lyrics(has_lyrics);

    // Smoothly open side lyrics panel if track has lyrics
    if has_lyrics {
        window.set_lyrics_visible(true);
    }

    // Audio Playback
    if let Ok(mut eng) = engine.lock() {
        eng.current_path = Some(track.audio_path.clone());
        if auto_play {
            if let Err(e) = eng.play(&track.audio_path) {
                eprintln!("Audio playback error: {e}");
            } else {
                window.set_is_playing(true);
                window.set_playback_icon("⏸".into());
            }
        }
    }
}

fn prefetch_track_waveform(track: &Track, waveform_cache: &WaveformCacheMap) {
    let path = track.audio_path.clone();
    let stamp = track.mtime.unwrap_or(0.0) as u64;
    {
        if let Ok(cache) = waveform_cache.lock() {
            if cache.contains_key(&path) {
                return;
            }
        }
    }
    if let Some(store) = WaveformStore::default_location() {
        if let Some(bars) = store.get(&path, stamp) {
            if let Ok(mut cache) = waveform_cache.lock() {
                cache.insert(path, Arc::new(bars));
            }
            return;
        }
    }
    let wf_cache = Arc::clone(waveform_cache);
    thread::spawn(move || {
        let envelope = Arc::new(compute_audio_waveform(&path));
        if let Some(store) = WaveformStore::default_location() {
            store.put(&path, stamp, &envelope);
        }
        if let Ok(mut cache) = wf_cache.lock() {
            cache.insert(path, envelope);
        }
    });
}

fn prefetch_next_track(app_state: &Arc<Mutex<AppState>>, waveform_cache: &WaveformCacheMap) {
    let next_track = {
        let Ok(s) = app_state.lock() else { return };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let next_row = match s.current_filtered_row {
            Some(cur) => (cur + 1) % s.filtered_tracks.len(),
            None => 0,
        };
        let track_idx = s.filtered_tracks[next_row];
        s.all_tracks.get(track_idx).cloned()
    };
    if let Some(track) = next_track {
        prefetch_track_waveform(&track, waveform_cache);
    }
}

fn play_next_track(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
) {
    let next_track_opt = {
        let mut s = match app_state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let total = s.filtered_tracks.len();
        let next_row = match s.current_filtered_row {
            Some(cur) => (cur + 1) % total,
            None => 0,
        };
        s.current_filtered_row = Some(next_row);
        let track_idx = s.filtered_tracks[next_row];
        s.all_tracks.get(track_idx).cloned()
    };

    if let Some(track) = next_track_opt {
        setup_track_for_playback(
            &track,
            window,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(app_state, waveform_cache);
    }
}

fn play_prev_track(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
) {
    let prev_track_opt = {
        let mut s = match app_state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if s.filtered_tracks.is_empty() {
            return;
        }
        let total = s.filtered_tracks.len();
        let prev_row = match s.current_filtered_row {
            Some(cur) => {
                if cur == 0 {
                    total - 1
                } else {
                    cur - 1
                }
            }
            None => 0,
        };
        s.current_filtered_row = Some(prev_row);
        let track_idx = s.filtered_tracks[prev_row];
        s.all_tracks.get(track_idx).cloned()
    };

    if let Some(track) = prev_track_opt {
        setup_track_for_playback(
            &track,
            window,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(app_state, waveform_cache);
    }
}

/// Plays row `row_idx` of the album or artist detail list. Both lists are built
/// from `all_tracks`, so the row index maps through the indices recorded when
/// the list was built.
fn play_detail_row(
    window: &slint::Weak<MainWindow>,
    state: &Arc<Mutex<AppState>>,
    engine: &Arc<Mutex<RodioAudioEngine>>,
    lyric_state: &Arc<Mutex<Vec<LrcLine>>>,
    art_loader: &ArtLoader,
    waveform_cache: &WaveformCacheMap,
    row_idx: i32,
    from_artist_view: bool,
) {
    let Some(w) = window.upgrade() else { return };
    let track = {
        let Ok(s) = state.lock() else { return };
        let indices = if from_artist_view {
            &s.artist_detail_tracks
        } else {
            &s.album_detail_tracks
        };
        indices
            .get(row_idx as usize)
            .and_then(|&i| s.all_tracks.get(i))
            .cloned()
    };
    if let Some(track) = track {
        setup_track_for_playback(
            &track,
            &w,
            engine,
            lyric_state,
            art_loader,
            waveform_cache,
            true,
        );
        prefetch_next_track(state, waveform_cache);
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    window.show()?;
    let audio = Arc::new(Mutex::new(RodioAudioEngine::new()));
    let lyric_state = Arc::new(Mutex::new(Vec::<LrcLine>::new()));
    let app_state = Arc::new(Mutex::new(AppState::new()));
    let art_loader = Arc::new(ArtLoader::new());
    art_loader.spawn_workers(window.as_weak());
    let waveform_cache: WaveformCacheMap = Arc::new(Mutex::new(HashMap::new()));
    let batch_job_control: Arc<Mutex<Option<ActiveBatchJob>>> = Arc::new(Mutex::new(None));

    // Set initial default waveform in UI
    window.set_waveform_data(ModelRc::new(VecModel::from(default_waveform())));

    // Load settings into UI
    let settings = load_settings().unwrap_or_default();
    apply_settings_to_ui(&window, &settings);

    // Load saved library immediately
    load_library_into_ui(&window, &app_state, &art_loader);

    // Nothing can be done in an empty library, so on a first run (or after every
    // folder has been removed) the directory picker opens itself. It closes as
    // soon as a folder is scanned.
    if window.get_settings_music_dirs().row_count() == 0 {
        window.set_directories_open(true);
    }

    // Automatically scan library in background on open if enabled in settings
    if settings.scan_on_startup {
        let event_loop = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        thread::spawn(move || {
            let _ = (|| -> Result<(), String> {
                let db = open_database()?;
                let dirs = db.directories().map_err(|e| e.to_string())?;
                if dirs.is_empty() {
                    return Ok(());
                }
                let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;
                let mut any_changed = false;
                for dir in &dirs {
                    if let Ok(res) = scan_directory_incremental(dir, Some(&existing_map)) {
                        if !res.changed_tracks.is_empty() {
                            let _ = db.upsert_tracks(&res.changed_tracks);
                            any_changed = true;
                        }
                    }
                }
                if any_changed {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &cache);
                        }
                    });
                }
                Ok(())
            })();
        });
    }

    // -----------------------------------------------------------------------
    // 100ms playback position, reactive waveform, and auto-advance timer
    // -----------------------------------------------------------------------
    let timer_audio = Arc::clone(&audio);
    let timer_lyrics = Arc::clone(&lyric_state);
    let timer_app_state = Arc::clone(&app_state);
    let timer_art_loader = Arc::clone(&art_loader);
    let timer_wf_cache = Arc::clone(&waveform_cache);
    let timer_window = window.as_weak();
    // Smoothed loudness at the playhead, and the track it belongs to. Rising
    // quickly and falling slowly is what makes the bars breathe with the music
    // instead of flickering.
    let mut level = 0.0f32;
    let mut level_path: Option<String> = None;

    // 30 fps rather than 10: the waveform and the playhead are both drawn from
    // the position, and at 10 fps every step was visible as a jump. Everything
    // that only changes once a second is compared before it is written, so the
    // faster tick costs a handful of property reads.
    let position_timer = Timer::default();
    position_timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
        let Some(window) = timer_window.upgrade() else {
            return;
        };
        let Ok(engine) = timer_audio.lock() else {
            return;
        };
        let position_opt = engine.position_seconds();
        let duration = engine.duration_seconds().unwrap_or(0.0);
        let playing = engine.is_playing();
        let current_path = engine.current_path.clone();
        let raw_live_level = engine.live_level.get();
        drop(engine);

        let Some(position) = position_opt else {
            return;
        };

        let progress = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let position_text = format_time(position);
        if window.get_position_text().as_str() != position_text.as_str() {
            window.set_position_text(position_text.into());
        }
        let duration_text = format_time(duration);
        if window.get_duration_text().as_str() != duration_text.as_str() {
            window.set_duration_text(duration_text.into());
        }
        if window.get_is_playing() != playing {
            window.set_is_playing(playing);
            window.set_playback_icon(if playing { "⏸" } else { "▶" }.into());
        }
        if (window.get_seek_position() - progress).abs() > 0.0005 {
            window.set_seek_position(progress);
        }

        // Sound-reactive waveform. The static shape comes from the track's
        // envelope (decoded once per track and cached on disk), but the *level*
        // that drives the animation is the real live RMS amplitude from the audio
        // thread — captured by the LevelCapture source wrapper.
        // If the track's exact envelope is still computing, use default_waveform()
        // as the baseline so the waveform starts reacting and pulsing to the music
        // immediately from frame 1 rather than freezing flat!
        let envelope = current_path.as_ref().and_then(|path| {
            let cache = timer_wf_cache.lock().ok()?;
            cache.get(path).cloned()
        });
        let default_env = default_waveform();
        let env_slice = envelope.as_deref().map(|v| v.as_slice()).unwrap_or(&default_env);
        if level_path.as_deref() != current_path.as_deref() {
            level_path = current_path.clone();
            level = 0.0;
        }
        let target = if playing { raw_live_level } else { 0.0 };
        // Smooth: fast attack so beats land immediately, moderate release
        // so bars don't flicker between frames.
        let rate = if target > level { 0.7 } else { 0.2 };
        level += (target - level) * rate;
        set_waveform_bars(
            &window,
            &reactive_bars(env_slice, progress, level, playing),
        );

        // Update active lyric line
        if let Ok(lines) = timer_lyrics.lock() {
            let active_idx = lines
                .iter()
                .enumerate()
                .rev()
                .find(|(_, line)| line.timestamp_seconds <= position)
                .map(|(i, _)| i as i32)
                .unwrap_or(-1);
            if window.get_active_line_index() != active_idx {
                window.set_active_line_index(active_idx);
            }
        }

        // Detect end of track and auto advance to next song in playlist
        let should_advance = {
            if let Ok(mut s) = timer_app_state.lock() {
                let finished = s.was_playing
                    && !playing
                    && (duration > 0.0 && position >= duration - 1.2 || position < 0.05);
                s.was_playing = playing;
                finished
            } else {
                false
            }
        };

        if should_advance {
            play_next_track(
                &window,
                &timer_app_state,
                &timer_audio,
                &timer_lyrics,
                &timer_art_loader,
                &timer_wf_cache,
            );
        }
    });

    // -----------------------------------------------------------------------
    // Add a music folder
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_add_directory(move || {
            let Some(folder) = rfd::FileDialog::new()
                .set_title("Choose music folder")
                .pick_folder()
            else {
                return;
            };
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let result = (|| -> Result<(), String> {
                    let db = open_database()?;
                    db.add_directory(folder.to_string_lossy().as_ref())
                        .map_err(|e| e.to_string())?;
                    let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;
                    let res = scan_directory_incremental(&folder, Some(&existing_map))
                        .map_err(|e| format!("{e:?}"))?;
                    if !res.changed_tracks.is_empty() {
                        db.upsert_tracks(&res.changed_tracks).map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("Scan error: {e}");
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = event_loop.upgrade() {
                        // A folder was just scanned: covers may have appeared
                        // next to files, and files may have been re-tagged, so
                        // nothing the artwork index remembers is trustworthy.
                        art_loader.reset_after_scan();
                        load_library_into_ui(&w, &state, &art_loader);
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Refresh
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_refresh(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_is_refreshing() {
                return;
            }
            w.set_is_refreshing(true);
            w.set_refresh_progress(0.0);
            w.set_refresh_text(SharedString::from("Refreshing..."));

            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let result = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let dirs = db.directories().map_err(|e| e.to_string())?;
                    let existing_map = db.get_tracks_map().map_err(|e| e.to_string())?;

                    // Efficiently discover paths without repeating directory walks
                    let mut all_paths = Vec::new();
                    for dir in &dirs {
                        let mut dir_paths = Vec::new();
                        if lyrics_core::collect_audio_paths(std::path::Path::new(dir), &mut dir_paths).is_ok() {
                            dir_paths.sort();
                            all_paths.extend(dir_paths);
                        }
                    }

                    let total_files = all_paths.len();
                    if total_files == 0 {
                        return Ok(());
                    }

                    let last_update = Arc::new(Mutex::new(std::time::Instant::now()));
                    let event_loop_cb = event_loop.clone();
                    let last_update_cb = Arc::clone(&last_update);

                    let res = lyrics_core::scan_audio_paths_incremental(
                        all_paths,
                        Some(&existing_map),
                        move |idx, total| {
                            let should_update = {
                                let mut last = last_update_cb.lock().unwrap();
                                if idx == 0 || idx == total || last.elapsed() >= Duration::from_millis(50) {
                                    *last = std::time::Instant::now();
                                    true
                                } else {
                                    false
                                }
                            };

                            if should_update {
                                let prog = if total > 0 { idx as f32 / total as f32 } else { 1.0 };
                                let pct = (prog * 100.0).round() as u32;
                                let el = event_loop_cb.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(w) = el.upgrade() {
                                        w.set_refresh_progress(prog);
                                        w.set_refresh_text(SharedString::from(format!("Refreshing... {pct}%")));
                                    }
                                });
                            }
                        },
                    );

                    if !res.changed_tracks.is_empty() {
                        let _ = db.upsert_tracks(&res.changed_tracks);
                    }
                    Ok(())
                })();

                if let Err(e) = result {
                    eprintln!("Refresh scan error: {e}");
                }

                let el = event_loop.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        art_loader.reset_after_scan();
                        load_library_into_ui(&w, &st, &ac);
                        w.set_refresh_progress(1.0);
                        w.set_refresh_text(SharedString::from("Done!"));
                    }
                });

                // Keep completion state briefly visible for feedback
                thread::sleep(Duration::from_millis(700));
                let el_reset = event_loop.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el_reset.upgrade() {
                        w.set_is_refreshing(false);
                        w.set_refresh_progress(0.0);
                        w.set_refresh_text(SharedString::from("Refresh Directory"));
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Batch Download: Options dialog, incremental rescan, live progress & platform logs
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        window.on_download_all(move || {
            let Some(w) = weak.upgrade() else { return };
            // If already running or finished in stage 1, restore dialog directly:
            if w.get_batch_is_running() || (w.get_batch_is_finished() && w.get_batch_download_stage() == 1) {
                w.set_batch_download_open(true);
                return;
            }
            let Ok(s) = state.lock() else { return };
            let total = s.all_tracks.len() as i32;
            let missing = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Missing)
                .count() as i32;
            let plain = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Plain)
                .count() as i32;
            let suspicious = s
                .all_tracks
                .iter()
                .filter(|t| t.status == LyricsStatus::Suspicious)
                .count() as i32;
            drop(s);

            w.set_batch_total_count(total);
            w.set_batch_missing_count(missing);
            w.set_batch_plain_count(plain);
            w.set_batch_suspicious_count(suspicious);
            w.set_batch_download_stage(0);
            w.set_batch_download_open(true);
        });
    }
    {
        let weak = window.as_weak();
        let _job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_close(move || {
            if let Some(w) = weak.upgrade() {
                w.set_batch_download_open(false);
                // When closing the modal with 'X' while running, do not cancel the job.
                // The download continues running in the background and can be restored
                // from the floating pill or the toolbar Download button.
                // Cancellation is explicitly handled by on_batch_stop ("Stop" button).
                if w.get_batch_is_finished() {
                    w.set_batch_is_finished(false);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_pause_resume(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    let next = !job.paused.load(std::sync::atomic::Ordering::Relaxed);
                    job.paused.store(next, std::sync::atomic::Ordering::Relaxed);
                    w.set_batch_is_paused(next);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_stop(move || {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(mut lock) = job_ctrl.lock() {
                if let Some(ref job) = *lock {
                    job.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                    job.paused.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                *lock = None;
            }
            w.set_batch_is_running(false);
            w.set_batch_phase_text(SharedString::from("Stopped by user."));
            w.set_batch_is_finished(true);
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        let job_ctrl = Arc::clone(&batch_job_control);
        window.on_batch_start_mode(move |mode_idx| {
            let Some(w) = weak.upgrade() else { return };
            let (mode, mode_title) = match mode_idx {
                0 => (DownloadMode::ReplaceAll, "Batch Download: Whole Directory"),
                1 => (DownloadMode::MissingOnly, "Batch Download: Missing Only"),
                2 => (DownloadMode::UpgradePlain, "Batch Download: Upgrade Plain to Synced"),
                3 => (DownloadMode::FixSuspicious, "Batch Download: Fix Suspicious"),
                _ => (DownloadMode::MissingOnly, "Batch Download"),
            };

            let cancelled = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            if let Ok(mut lock) = job_ctrl.lock() {
                *lock = Some(ActiveBatchJob {
                    cancelled: Arc::clone(&cancelled),
                    paused: Arc::clone(&paused),
                });
            }

            w.set_batch_mode_title(SharedString::from(mode_title));
            w.set_batch_phase_text(SharedString::from("Phase 1: Rescanning music directories..."));
            w.set_batch_current_track(SharedString::from("Rescanning music directories..."));
            w.set_batch_progress(0.0);
            w.set_batch_progress_text(SharedString::from("Rescanning..."));
            w.set_batch_stats(BatchStats {
                total: 0,
                processed: 0,
                remaining: 0,
                synced: 0,
                plain: 0,
                not_found: 0,
                errors: 0,
            });

            let initial_logs = vec![BatchLogEntry {
                timestamp: current_timestamp().into(),
                track_title: "Rescan Directories".into(),
                track_artist: "Library".into(),
                details: "Rescanning music folders to update tracks before starting download...".into(),
                action_text: "".into(),
                badge_text: "SCAN".into(),
                badge_type: "scan".into(),
            }];
            w.set_batch_logs(ModelRc::new(VecModel::from(initial_logs.clone())));

            w.set_batch_download_stage(1);
            w.set_batch_is_running(true);
            w.set_batch_is_paused(false);
            w.set_batch_is_finished(false);
            w.set_batch_phase_text("Phase 1: Scanning music library...".into());

            let event_loop = w.as_weak();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            let job_ctrl = Arc::clone(&job_ctrl);
            let cancelled = Arc::clone(&cancelled);
            let paused = Arc::clone(&paused);

            thread::spawn(move || {
                let mut logs = initial_logs;

                let db = match open_database() {
                    Ok(db) => db,
                    Err(e) => {
                        logs.push(BatchLogEntry {
                            timestamp: current_timestamp().into(),
                            track_title: "Database Error".into(),
                            track_artist: "System".into(),
                            details: format!("Could not open database: {e}").into(),
                            action_text: "".into(),
                            badge_text: "ERROR".into(),
                            badge_type: "error".into(),
                        });
                        let el = event_loop.clone();
                        let logs_clone = logs.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = el.upgrade() {
                                w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                                w.set_batch_is_running(false);
                                w.set_batch_is_finished(true);
                                w.set_batch_phase_text("Error opening database".into());
                            }
                        });
                        return;
                    }
                };

                // Phase 1: Rescan music directories
                let dirs = db.directories().unwrap_or_default();
                let existing_map = db.get_tracks_map().unwrap_or_default();
                let mut all_paths = Vec::new();
                for dir in &dirs {
                    let mut dir_paths = Vec::new();
                    if lyrics_core::collect_audio_paths(std::path::Path::new(dir), &mut dir_paths).is_ok() {
                        dir_paths.sort();
                        all_paths.extend(dir_paths);
                    }
                }

                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                let scan_res = lyrics_core::scan_audio_paths_incremental(all_paths, Some(&existing_map), |_, _| {});
                if !scan_res.changed_tracks.is_empty() {
                    let _ = db.upsert_tracks(&scan_res.changed_tracks);
                }

                // Reload tracks into app_state and update main UI
                let el = event_loop.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        ac.reset_after_scan();
                        load_library_into_ui(&w, &st, &ac);
                    }
                });

                let all_tracks = db.tracks().unwrap_or_default();

                logs.push(BatchLogEntry {
                    timestamp: current_timestamp().into(),
                    track_title: "Rescan Complete".into(),
                    track_artist: "Library".into(),
                    details: format!("Discovered {} total tracks in library.", all_tracks.len()).into(),
                    action_text: "".into(),
                    badge_text: "SCAN".into(),
                    badge_type: "scan".into(),
                });

                let el = event_loop.clone();
                let logs_clone = logs.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                    }
                });

                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                // Phase 2: Select candidate tracks based on chosen mode
                let candidates: Vec<Track> = all_tracks
                    .into_iter()
                    .filter(|t| t.should_download(mode))
                    .collect();

                let total_candidates = candidates.len();
                if total_candidates == 0 {
                    logs.push(BatchLogEntry {
                        timestamp: current_timestamp().into(),
                        track_title: "No Matching Songs".into(),
                        track_artist: "Batch".into(),
                        details: "No tracks matching this download criteria were found in your library.".into(),
                        action_text: "".into(),
                        badge_text: "INFO".into(),
                        badge_type: "info".into(),
                    });
                    let el = event_loop.clone();
                    let logs_clone = logs.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el.upgrade() {
                            w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                            w.set_batch_progress(1.0);
                            w.set_batch_progress_text("0 / 0".into());
                            w.set_batch_phase_text("Completed: No matching tracks found".into());
                            w.set_batch_current_track("No tracks to download".into());
                            w.set_batch_is_running(false);
                            w.set_batch_is_finished(true);
                        }
                    });
                    return;
                }

                let settings = load_settings().unwrap_or_default();
                let mut stats = BatchStats {
                    total: total_candidates as i32,
                    processed: 0,
                    remaining: total_candidates as i32,
                    synced: 0,
                    plain: 0,
                    not_found: 0,
                    errors: 0,
                };

                let el = event_loop.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        w.set_batch_phase_text(format!("Phase 2: Downloading lyrics ({} tracks)", total_candidates).into());
                    }
                });

                // Phase 3: Download loop
                let mut was_cancelled = false;
                for (i, track) in candidates.iter().enumerate() {
                    // Check cancellation
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        was_cancelled = true;
                        break;
                    }

                    // Handle pause
                    while paused.load(std::sync::atomic::Ordering::Relaxed) && !cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(150));
                    }
                    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        was_cancelled = true;
                        break;
                    }

                    let cur_display = format!("{} — {}", track.artist, track.title);
                    let prog = i as f32 / total_candidates as f32;
                    let prog_text = format!("Song {} of {} ({} remaining)", i + 1, total_candidates, total_candidates - i);

                    let el = event_loop.clone();
                    let cd_clone = cur_display.clone();
                    let pt_clone = prog_text.clone();
                    let stats_clone = stats.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el.upgrade() {
                            w.set_batch_current_track(cd_clone.into());
                            w.set_batch_progress(prog);
                            w.set_batch_progress_text(pt_clone.into());
                            w.set_batch_stats(stats_clone);
                        }
                    });

                    // Search providers and track results per platform
                    let provider_results = Arc::new(Mutex::new(Vec::<(ProviderName, Option<bool>, Option<String>)>::new()));
                    let outcome_res = match providers_for_settings(&db, track, &settings) {
                        Ok(providers) => {
                            let pr_cb = Arc::clone(&provider_results);
                            download_track(track, mode, &providers, &cancelled, move |event| {
                                if let Ok(mut pr) = pr_cb.lock() {
                                    match event {
                                        DownloadEvent::ProviderFound { provider, synced } => {
                                            pr.push((provider, Some(synced), None));
                                        }
                                        DownloadEvent::ProviderNotFound { provider } => {
                                            pr.push((provider, None, None));
                                        }
                                        DownloadEvent::ProviderFailed { provider, message } => {
                                            pr.push((provider, None, Some(message)));
                                        }
                                        _ => {}
                                    }
                                }
                            })
                            .map_err(|e| e.to_string())
                        }
                        Err(e) => Err(e),
                    };

                    let pr_list = provider_results.lock().map(|l| l.clone()).unwrap_or_default();
                    let mut parts = Vec::new();
                    for (p, status, err) in pr_list {
                        if let Some(synced) = status {
                            parts.push(format!("✓ {} ({})", p.label(), if synced { "synced" } else { "plain" }));
                        } else if let Some(_) = err {
                            parts.push(format!("⚠ {} (error)", p.label()));
                        } else {
                            parts.push(format!("✗ {}", p.label()));
                        }
                    }
                    let providers_summary = if parts.is_empty() {
                        "No provider responses".to_string()
                    } else {
                        parts.join("  |  ")
                    };

                    let (badge_text, badge_type, result_action) = match outcome_res {
                        Ok(DownloadOutcome::Downloaded { provider, synced }) => {
                            if synced {
                                stats.synced += 1;
                                let _ = db.update_lyrics_status(&track.audio_path, LyricsStatus::Synced, Some(provider.label()));
                                ("SYNCED", "synced", format!("→ Saved synced lyrics from {}", provider.label()))
                            } else {
                                stats.plain += 1;
                                let _ = db.update_lyrics_status(&track.audio_path, LyricsStatus::Plain, Some(provider.label()));
                                ("PLAIN", "plain", format!("→ Saved plain lyrics from {}", provider.label()))
                            }
                        }
                        Ok(DownloadOutcome::NotFound) => {
                            stats.not_found += 1;
                            ("NOT FOUND", "missing", "→ No matching lyrics found".to_string())
                        }
                        Ok(DownloadOutcome::Skipped) => {
                            ("SKIPPED", "info", "→ Skipped (kept existing lyrics)".to_string())
                        }
                        Ok(DownloadOutcome::Cancelled) => {
                            was_cancelled = true;
                            ("CANCELLED", "missing", "→ Cancelled by user".to_string())
                        }
                        Err(err) => {
                            stats.errors += 1;
                            ("ERROR", "error", format!("→ Error: {err}"))
                        }
                    };

                    stats.processed = (i + 1) as i32;
                    stats.remaining = (total_candidates - (i + 1)) as i32;

                    logs.push(BatchLogEntry {
                        timestamp: current_timestamp().into(),
                        track_title: track.title.clone().into(),
                        track_artist: track.artist.clone().into(),
                        details: providers_summary.into(),
                        action_text: result_action.into(),
                        badge_text: badge_text.into(),
                        badge_type: badge_type.into(),
                    });

                    // Update UI live
                    let el = event_loop.clone();
                    let logs_clone = logs.clone();
                    let stats_clone = stats.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = el.upgrade() {
                            w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                            w.set_batch_stats(stats_clone);
                        }
                    });

                    // Periodically refresh library view in background so track list shows live badges
                    if (i + 1) % 5 == 0 || i + 1 == total_candidates {
                        let el = event_loop.clone();
                        let st = Arc::clone(&state);
                        let ac = Arc::clone(&art_loader);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = el.upgrade() {
                                load_library_into_ui(&w, &st, &ac);
                            }
                        });
                    }

                    if was_cancelled {
                        break;
                    }
                }

                // Phase 4: Finish
                logs.push(BatchLogEntry {
                    timestamp: current_timestamp().into(),
                    track_title: if was_cancelled { "Process Stopped" } else { "Process Finished" }.into(),
                    track_artist: "Batch".into(),
                    details: format!("Processed {}/{} tracks. Synced: {}, Plain: {}, Not Found: {}, Errors: {}", stats.processed, total_candidates, stats.synced, stats.plain, stats.not_found, stats.errors).into(),
                    action_text: "".into(),
                    badge_text: if was_cancelled { "STOPPED" } else { "DONE" }.into(),
                    badge_type: if was_cancelled { "missing" } else { "synced" }.into(),
                });

                if let Ok(mut lock) = job_ctrl.lock() {
                    *lock = None;
                }

                let el = event_loop.clone();
                let logs_clone = logs.clone();
                let stats_clone = stats.clone();
                let st = Arc::clone(&state);
                let ac = Arc::clone(&art_loader);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = el.upgrade() {
                        w.set_batch_logs(ModelRc::new(VecModel::from(logs_clone)));
                        w.set_batch_stats(stats_clone);
                        w.set_batch_progress(1.0);
                        w.set_batch_progress_text(format!("{}/{}", stats.processed, total_candidates).into());
                        w.set_batch_current_track(if was_cancelled { "Batch download stopped." } else { "Batch download completed!" }.into());
                        w.set_batch_phase_text(format!("Finished: {} synced, {} plain, {} not found, {} errors", stats.synced, stats.plain, stats.not_found, stats.errors).into());
                        w.set_batch_is_running(false);
                        w.set_batch_is_finished(true);
                        load_library_into_ui(&w, &st, &ac);
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Search and filter
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let loader = Arc::clone(&art_loader);
        window.on_search_changed(move |text| {
            if let (Some(w), Ok(mut s)) = (weak.upgrade(), state.lock()) {
                s.rebuild_filter(text.as_str(), w.get_status_filter());
                let filtered: Vec<TrackData> = UI_LIBRARY.with(|lib| {
                    lib.borrow().get_filtered(&s.filtered_tracks)
                });
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
        let state = Arc::clone(&app_state);
        let loader = Arc::clone(&art_loader);
        window.on_filter_changed(move |filter_idx| {
            if let (Some(w), Ok(mut s)) = (weak.upgrade(), state.lock()) {
                s.rebuild_filter(w.get_search_text().as_str(), filter_idx);
                let filtered: Vec<TrackData> = UI_LIBRARY.with(|lib| {
                    lib.borrow().get_filtered(&s.filtered_tracks)
                });
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

    // -----------------------------------------------------------------------
    // Track row click: select *and* play, so one click starts the song.
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let lyrics = Arc::clone(&lyric_state);
        let engine = Arc::clone(&audio);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_select_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, true);
            prefetch_next_track(&state, &wf_cache);
        });
    }

    // -----------------------------------------------------------------------
    // Play track (play button on row)
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_track(move |row_idx| {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            s.current_filtered_row = Some(row);
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            setup_track_for_playback(&track, &w, &engine, &lyrics, &cache, &wf_cache, true);
            prefetch_next_track(&state, &wf_cache);
        });
    }

    // -----------------------------------------------------------------------
    // Player bar controls
    // -----------------------------------------------------------------------
    {
        let engine = Arc::clone(&audio);
        let weak = window.as_weak();
        window.on_play_pause(move || {
            if let Ok(mut eng) = engine.lock() {
                match eng.toggle_pause() {
                    Ok(paused) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_is_playing(!paused);
                            w.set_playback_icon(if paused { "▶" } else { "⏸" }.into());
                            if !paused && w.get_lyrics_lines().row_count() > 0 {
                                w.set_lyrics_visible(true);
                            }
                        }
                    }
                    Err(e) => eprintln!("Play/Pause toggle error: {e}"),
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_seek(move |fraction| {
            if let Ok(mut eng) = engine.lock() {
                if let Some(dur) = eng.duration_seconds() {
                    let _ = eng.seek(fraction as f64 * dur);
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_rewind(move || {
            if let Ok(mut eng) = engine.lock() {
                if let Some(pos) = eng.position_seconds() {
                    let _ = eng.seek((pos - 10.0).max(0.0));
                }
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_forward(move || {
            if let Ok(mut eng) = engine.lock() {
                if let Some(pos) = eng.position_seconds() {
                    let target = pos + 10.0;
                    let _ = eng.seek(target);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyr = Arc::clone(&lyric_state);
        let art = Arc::clone(&art_loader);
        let wf = Arc::clone(&waveform_cache);
        window.on_next_track(move || {
            if let Some(w) = weak.upgrade() {
                play_next_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let audio_eng = Arc::clone(&audio);
        let lyr = Arc::clone(&lyric_state);
        let art = Arc::clone(&art_loader);
        let wf = Arc::clone(&waveform_cache);
        window.on_prev_track(move || {
            if let Some(w) = weak.upgrade() {
                play_prev_track(&w, &state, &audio_eng, &lyr, &art, &wf);
            }
        });
    }
    {
        let engine = Arc::clone(&audio);
        window.on_volume_changed(move |vol| {
            if let Ok(mut eng) = engine.lock() {
                eng.set_volume(vol);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Lyrics: seek to line & copy
    // -----------------------------------------------------------------------
    {
        let lyrics = Arc::clone(&lyric_state);
        let engine = Arc::clone(&audio);
        window.on_seek_to_line(move |line_idx| {
            if let (Ok(lines), Ok(mut eng)) = (lyrics.lock(), engine.lock()) {
                if let Some(line) = lines.get(line_idx as usize) {
                    let _ = eng.seek(line.timestamp_seconds);
                }
            }
        });
    }
    {
        let lyrics = Arc::clone(&lyric_state);
        window.on_copy_lyrics(move || {
            if let Ok(lines) = lyrics.lock() {
                let text: String = lines
                    .iter()
                    .map(|l| l.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = text;
            }
        });
    }

    // -----------------------------------------------------------------------
    // Fetch single track lyrics
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_fetch_track(move |row_idx| {
            let Ok(s) = state.lock() else { return };
            let row = row_idx as usize;
            let Some(&track_idx) = s.filtered_tracks.get(row) else {
                return;
            };
            let Some(track) = s.all_tracks.get(track_idx).cloned() else {
                return;
            };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;
                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                        }
                    });
                    Ok(())
                })();
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let cache = Arc::clone(&art_loader);
        window.on_fetch_current_lyrics(move || {
            let Ok(eng) = engine.lock() else { return };
            let Some(current_path) = eng.current_path.clone() else {
                return;
            };
            drop(eng);
            let Ok(s) = state.lock() else { return };
            let Some(track) = s
                .all_tracks
                .iter()
                .find(|t| t.audio_path == current_path)
                .cloned()
            else {
                return;
            };
            drop(s);
            let event_loop = weak.clone();
            let state = Arc::clone(&state);
            let art_loader = Arc::clone(&cache);
            thread::spawn(move || {
                let _ = (|| -> Result<(), String> {
                    let db = open_database()?;
                    let settings = load_settings().map_err(|e| e.to_string())?;
                    let providers = providers_for_settings(&db, &track, &settings)?;
                    let cancelled = AtomicBool::new(false);
                    let outcome = download_track(
                        &track,
                        DownloadMode::ReplaceAll,
                        &providers,
                        &cancelled,
                        |_| {},
                    )
                    .map_err(|e| e.to_string())?;
                    if let DownloadOutcome::Downloaded { provider, synced } = outcome {
                        db.update_lyrics_status(
                            &track.audio_path,
                            if synced {
                                LyricsStatus::Synced
                            } else {
                                LyricsStatus::Plain
                            },
                            Some(provider.label()),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = event_loop.upgrade() {
                            load_library_into_ui(&w, &state, &art_loader);
                        }
                    });
                    Ok(())
                })();
            });
        });
    }

    // -----------------------------------------------------------------------
    // Albums view
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
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
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_album_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, false,
            );
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_download_album(move |_album_idx| {
            if let Some(w) = weak.upgrade() {
                load_library_into_ui(&w, &state, &cache);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Artists view
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
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
        let state = Arc::clone(&app_state);
        let engine = Arc::clone(&audio);
        let lyrics = Arc::clone(&lyric_state);
        let cache = Arc::clone(&art_loader);
        let wf_cache = Arc::clone(&waveform_cache);
        window.on_play_artist_track(move |row_idx| {
            play_detail_row(
                &weak, &state, &engine, &lyrics, &cache, &wf_cache, row_idx, true,
            );
        });
    }
    {
        let weak = window.as_weak();
        let state = Arc::clone(&app_state);
        let cache = Arc::clone(&art_loader);
        window.on_download_artist(move |_artist_idx| {
            if let Some(w) = weak.upgrade() {
                load_library_into_ui(&w, &state, &cache);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Settings
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        window.on_save_settings(move || {
            let Some(w) = weak.upgrade() else { return };
            let settings = AppSettings {
                lrclib_enabled: w.get_settings_enable_lrclib(),
                musixmatch_enabled: w.get_settings_enable_musixmatch(),
                musixmatch_api_key: w.get_settings_musixmatch_key().to_string(),
                netease_enabled: w.get_settings_enable_netease(),
                megalobiz_enabled: w.get_settings_enable_megalobiz(),
                genius_enabled: w.get_settings_enable_genius(),
                genius_api_token: w.get_settings_genius_token().to_string(),
                lrclib_base_url: w.get_settings_lrclib_url().to_string(),
                request_interval_secs: w.get_settings_request_interval() as f64,
                retry_days: match w.get_settings_cache_period_idx() {
                    0 => 7,
                    1 => 14,
                    2 => 30,
                    _ => 0,
                },
                volume: w.get_volume(),
                window_width: 1280,
                window_height: 820,
                scan_on_startup: w.get_settings_scan_on_startup(),
            };
            let status = match save_settings(&settings) {
                Ok(()) => "Saved".to_string(),
                Err(e) => format!("Could not save: {e}"),
            };
            w.set_settings_status(status.into());
            // Clear the confirmation on its own, so it reads as feedback rather
            // than as a label that is always there.
            let weak = w.as_weak();
            Timer::single_shot(Duration::from_secs(2), move || {
                if let Some(w) = weak.upgrade() {
                    w.set_settings_status("".into());
                }
            });
        });
    }

    // -----------------------------------------------------------------------
    // About: open the repository, and look for a newer release
    // -----------------------------------------------------------------------
    {
        window.on_open_github(move || {
            if let Err(e) = open_in_browser(REPOSITORY_URL) {
                eprintln!("Could not open {REPOSITORY_URL}: {e}");
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_check_updates(move || {
            let Some(w) = weak.upgrade() else { return };
            w.set_updates_checking(true);
            w.set_updates_status("Checking for updates…".into());
            let weak = w.as_weak();
            thread::spawn(move || {
                let status = latest_release_status();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_updates_checking(false);
                        w.set_updates_status(status.into());
                    }
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Music directories
    //
    // These live in their own dialog rather than on the settings screen: they
    // describe the library, not a preference, and on a first run they are the
    // only thing that can be done.
    // -----------------------------------------------------------------------
    {
        let weak = window.as_weak();
        window.on_manage_directories(move || {
            if let Some(w) = weak.upgrade() {
                w.set_directories_open(true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_close_directories(move || {
            if let Some(w) = weak.upgrade() {
                w.set_directories_open(false);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_remove_directory(move |dir_idx| {
            let _ = (|| -> Result<(), String> {
                let db = open_database()?;
                let dirs = db.directories().map_err(|e| e.to_string())?;
                if let Some(path) = dirs.get(dir_idx as usize) {
                    db.remove_directory(path).map_err(|e| e.to_string())?;
                }
                let remaining = db.directories().map_err(|e| e.to_string())?;
                if let Some(w) = weak.upgrade() {
                    w.set_settings_music_dirs(ModelRc::new(VecModel::from(
                        remaining
                            .into_iter()
                            .map(SharedString::from)
                            .collect::<Vec<_>>(),
                    )));
                }
                Ok(())
            })();
        });
    }

    window.run()
}

// ---------------------------------------------------------------------------
// Updates and links
// ---------------------------------------------------------------------------

const REPOSITORY_URL: &str = "https://github.com/Sandeep2062/Synced-Lyrics-GUI";
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Sandeep2062/Synced-Lyrics-GUI/releases/latest";

/// One line describing whether a newer release exists. Never fails: an offline
/// machine or a repository with no releases yet gets a sentence, not an error.
fn latest_release_status() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let client = match reqwest::blocking::Client::builder()
        .user_agent(concat!("SyncedLyrics/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(e) => return format!("Could not check for updates: {e}"),
    };

    let response = match client.get(LATEST_RELEASE_API).send() {
        Ok(response) => response,
        Err(e) => return format!("Could not reach GitHub: {e}"),
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return "No releases published yet".to_string();
    }
    if !response.status().is_success() {
        return format!("GitHub returned {} — try again later", response.status());
    }

    let body: serde_json::Value = match response.json() {
        Ok(body) => body,
        Err(e) => return format!("Unexpected reply from GitHub: {e}"),
    };
    let Some(tag) = body.get("tag_name").and_then(|tag| tag.as_str()) else {
        return "No releases published yet".to_string();
    };

    if version_is_newer(tag.trim_start_matches(['v', 'V']), current) {
        format!("Version {tag} is available")
    } else {
        format!("Up to date (v{current})")
    }
}

/// Dotted-numeric comparison. Anything it cannot parse (a pre-release suffix,
/// say) counts as "not newer", so it can never nag about a tag it does not
/// understand.
fn version_is_newer(candidate: &str, current: &str) -> bool {
    fn numbers(version: &str) -> Option<Vec<u32>> {
        version
            .split('.')
            .map(|part| part.trim().parse().ok())
            .collect()
    }
    let (Some(candidate), Some(current)) = (numbers(candidate), numbers(current)) else {
        return false;
    };
    for index in 0..candidate.len().max(current.len()) {
        let new = candidate.get(index).copied().unwrap_or(0);
        let old = current.get(index).copied().unwrap_or(0);
        if new != old {
            return new > old;
        }
    }
    false
}

/// Hands `url` to the desktop's default browser. `std` has no "open a link",
/// but every platform has a one-liner for it.
fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        // `start` is a cmd builtin; the empty argument is the window title it
        // would otherwise take the URL for.
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };

    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn apply_settings_to_ui(window: &MainWindow, settings: &AppSettings) {
    window.set_app_version(env!("CARGO_PKG_VERSION").into());
    window.set_settings_enable_lrclib(settings.lrclib_enabled);
    window.set_settings_enable_musixmatch(settings.musixmatch_enabled);
    window.set_settings_musixmatch_key(settings.musixmatch_api_key.clone().into());
    window.set_settings_enable_netease(settings.netease_enabled);
    window.set_settings_enable_megalobiz(settings.megalobiz_enabled);
    window.set_settings_enable_genius(settings.genius_enabled);
    window.set_settings_genius_token(settings.genius_api_token.clone().into());
    window.set_settings_lrclib_url(settings.lrclib_base_url.clone().into());
    window.set_settings_request_interval(settings.request_interval_secs as f32);
    window.set_volume(settings.volume);
    let cache_idx = match settings.retry_days {
        0..=7 => 0,
        8..=14 => 1,
        15..=30 => 2,
        _ => 3,
    };
    window.set_settings_cache_period_idx(cache_idx);
    window.set_settings_scan_on_startup(settings.scan_on_startup);
}

fn load_library_into_ui(
    window: &MainWindow,
    app_state: &Arc<Mutex<AppState>>,
    art_loader: &ArtLoader,
) {
    let Ok(db) = open_database() else { return };
    let Ok(all_tracks) = db.tracks() else { return };
    let Ok(dirs) = db.directories() else { return };

    // The artwork code needs to know which album every track is on, so that a
    // cover found on one of an album's files can be shown on all of them.
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

    // Where every cover goes, so a finished thumbnail is written straight into
    // the rows waiting for it instead of rescanning every list.
    art_loader.remember_library_rows(&visible_tracks);
    art_loader.remember_album_cards(&albums);
    window.set_tracks(ModelRc::new(VecModel::from(visible_tracks)));
    window.set_album_rows(ModelRc::new(VecModel::from(album_rows(albums.len()))));
    window.set_albums(ModelRc::new(VecModel::from(albums)));
    window.set_artists(ModelRc::new(VecModel::from(artists)));
    window.set_tracks_summary(display_summary.into());

    // Index the whole library's artwork, not just what happens to be on screen.
    // Rows and album cards are now registered so background extraction results
    // reliably patch directly into their matching UI cards.
    index_library_art(&all_tracks, art_loader);

    let has_directories = !dirs.is_empty();
    window.set_settings_music_dirs(ModelRc::new(VecModel::from(
        dirs.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
    // A folder exists now, so the first-run picker has done its job.
    if has_directories {
        window.set_directories_open(false);
    }
}

#[cfg(test)]
mod ui_audit;

#[cfg(test)]
mod ui_snapshot;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_formats_correctly() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(65.0), "1:05");
        assert_eq!(format_time(3661.0), "61:01");
    }

    #[test]
    fn the_waveform_model_holds_exactly_the_bars_that_are_drawn() {
        // The seek bar mutates this model in place, so a length that disagreed
        // with the number of bars the UI repeats would strand stale bars on
        // screen for the rest of the session.
        let wf = default_waveform();
        assert_eq!(wf.len(), WAVEFORM_BARS);
        for &bar in &wf {
            assert!((0.12..=1.0).contains(&bar), "bar out of range: {bar}");
        }
    }

    #[test]
    fn envelope_bars_keeps_the_peak_of_each_window() {
        // Down-sampling takes the loudest sample of each window, not the mean:
        // averaging would flatten a single drum hit into nothing.
        let envelope = [0.1, 0.2, 0.3, 0.9, 0.4, 0.5, 0.6, 0.7];
        assert_eq!(envelope_bars(&envelope, 2), vec![0.9, 0.7]);
        assert_eq!(envelope_bars(&envelope, 8), envelope.to_vec());
        // An empty envelope still has to produce a shape to draw.
        assert_eq!(envelope_bars(&[], WAVEFORM_BARS).len(), WAVEFORM_BARS);
    }




    #[test]
    fn only_a_playing_waveform_rides_the_live_level() {
        let envelope = vec![0.2f32; 100];

        let paused = reactive_bars(&envelope, 0.5, 1.0, false);
        assert!(paused.iter().all(|bar| (*bar - 0.2).abs() < 1e-6));

        let playing = reactive_bars(&envelope, 0.5, 1.0, true);
        let tallest = playing.iter().copied().fold(0.0f32, f32::max);
        assert!(
            tallest > 0.4,
            "the bars at the playhead should be boosted by the live level, got {tallest}"
        );
        // The first bar is far from the playhead so it should not get the
        // local pulse — only the global breath, which at level=1.0 equals
        // the base envelope height.
        assert!((playing[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn version_compare_handles_the_shapes_a_release_tag_takes() {
        assert!(version_is_newer("1.0.1", "1.0.0"));
        assert!(version_is_newer("0.2", "0.1.9"));
        assert!(version_is_newer("2.0.0", "1.9.9"));
        assert!(!version_is_newer("1.0.0", "1.0.0"));
        assert!(!version_is_newer("0.9.9", "1.0.0"));
        // A tag we cannot parse must never report a phantom update.
        assert!(!version_is_newer("1.1.0-beta.1", "1.0.0"));
        assert!(!version_is_newer("", "1.0.0"));
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
            },
        ];
        let loader = ArtLoader::new();
        let albums = build_album_data(&tracks, &loader);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].track_count, 2);
        assert_eq!(albums[0].synced_count, 1);
        // No artwork has been extracted, so every album starts as a placeholder
        // (and a job is queued) rather than blocking on the file.
        assert!(!albums[0].has_cover);
    }

    /// A minimal but valid 16-bit PCM WAV, so probing it behaves like a real
    /// track instead of a corrupt file.
    fn write_silent_wav(path: &std::path::Path) {
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

    fn jpeg_bytes() -> Vec<u8> {
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

    fn write_jpeg(path: &std::path::Path) {
        std::fs::write(path, jpeg_bytes()).expect("write jpeg fixture");
    }

    /// The artwork pipeline, end to end and without a window: a track with no
    /// embedded art, sitting in a folder that has a `cover.jpg`, has to come back
    /// with a thumbnail — and the second time it must come from the index rather
    /// than from the file. This is the "some tracks have no cover" fix.
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

        // First pass: the tags have no picture, so the folder's cover is used.
        let (thumbnail, pixels) = load_thumbnail(Some(&store), &job);
        let thumbnail = thumbnail.expect("the folder's cover.jpg should produce a thumbnail");
        assert!(pixels.is_some(), "the thumbnail should decode to pixels");

        // Second pass, folder untouched: the index answers, so neither the audio
        // file nor the cover is read again — which is what makes the next
        // launch's covers instant.
        let (indexed, indexed_pixels) = load_thumbnail(Some(&store), &job);
        assert_eq!(
            indexed.expect("the index should answer").as_slice(),
            thumbnail.as_slice()
        );
        assert!(indexed_pixels.is_some());

        // Deleting the cover changes the *folder*, not the audio file, and the
        // index has to notice: artwork that is gone must not keep being shown.
        std::fs::remove_file(&cover).expect("remove the cover behind the index's back");
        lyrics_core::clear_folder_art_cache();
        let (without_cover, _) = load_thumbnail(Some(&store), &job);
        assert!(
            without_cover.is_none(),
            "an index entry for a cover that vanished must not be reused"
        );

        // A re-tagged file (a different stamp) is a different question entirely.
        let restamped = ArtJob { stamp: 43, ..job };
        let (fresh, _) = load_thumbnail(Some(&store), &restamped);
        assert!(fresh.is_none(), "a new mtime invalidates the indexed cover");

        std::fs::remove_dir_all(&dir).expect("clean up fixture");
    }

    /// Point `LYRICS_TEST_AUDIO` at a tagged file from your own library to run
    /// this; it is skipped otherwise so CI stays green without fixtures.
    ///
    /// This is the path the UI depends on: the worker reads the artwork, decodes
    /// it and hands the UI a thumbnail, which is why covers can appear without
    /// the window ever touching the disk.
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

    /// The artwork pipeline end to end for the layout taggers actually produce:
    /// one track per file, one image per track, nothing embedded and nothing
    /// called `cover`. Every row has to come back with its own picture.
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

        // Each entry is its own: the index is keyed by the file, so a track
        // never borrows its neighbour's picture.
        for index in 0..jobs.len() {
            let (restored, _) = load_thumbnail(Some(&store), &jobs[index]);
            assert_eq!(
                restored.expect("the index should answer").as_slice(),
                thumbnails[index].as_slice()
            );
        }

        // The index, not the audio files, is what a later launch reads: with the
        // songs themselves gone the covers still come back, which is what makes
        // the second start instant.
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

    /// A cover that arrives on a worker lights up its album too, without the UI
    /// thread decoding anything: the pixels that just arrived *are* the album's.
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
        // Nothing has been *extracted* here, so the album's cover cannot have
        // come from a decode: the arriving pixels were shared with it.
        assert!(loader.album_art("Album1").is_none());
        assert!(
            cached_cover_image(&album_image_key("Album1"))
                .flatten()
                .is_some(),
            "the album should show the cover that just arrived"
        );
        assert!(cached_cover_image("b.mp3").flatten().is_some());

        // A track that has no artwork of its own reports no picture, so it keeps
        // the placeholder it already has instead of being handed a wrong one.
        let (none, no_albums) = absorb_ready_art(vec![("a.mp3".to_string(), None)], &loader);
        assert!(none.is_empty());
        assert!(no_albums.is_empty());
    }

    /// A scan throws away what the loader believed about the artwork, so covers
    /// that appeared, changed or were removed since then are picked up.
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

        // ...and the file is asked about again, which is what picks up a cover
        // that appeared after the last look.
        loader.request("a.mp3", "a.mp3", 10, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
    }

    /// The same file at the same stamp is never read twice; the same file after
    /// a change is read again.
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

        // A re-tagged file is a different question, and gets asked.
        loader.request("a.mp3", "a.mp3", 11, false);
        assert_eq!(
            loader.next_job().map(|job| job.key),
            Some("a.mp3".to_string())
        );
    }

    /// The UI's image cache is bounded: a library of a few thousand albums must
    /// not become a few thousand pixel buffers the app never lets go of.
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

        // A cover that keeps being asked for survives a backfill of new ones —
        // the rows on screen are the ones worth keeping.
        store_cover_image("in-view", Some(image.clone()));
        for index in 0..UI_IMAGE_CACHE_LIMIT - 1 {
            assert!(cached_cover_image("in-view").is_some());
            store_cover_image(&format!("backfill-{index}"), Some(image.clone()));
        }
        assert!(cached_cover_image("in-view").is_some());
    }

    #[test]
    fn album_grid_rows_cover_every_album() {
        // The grid is `ALBUM_GRID_COLUMNS` wide, so the row list must be long
        // enough that no album is stranded off the end.
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
        }];
        let artists = build_artist_data(&tracks);
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "The Beatles");
        assert_eq!(artists[0].track_count, 1);
        assert_eq!(artists[0].album_count, 1);
    }

    /// A real library is only half tagged, so a track whose own tags carry no
    /// picture has to show its album's. Anything else leaves a hole in the list
    /// wherever the tagging was thin.
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

        // Nothing has been extracted yet.
        assert!(loader.album_art("Album1").is_none());

        // Only the album's *second* file carries artwork — which is exactly the
        // case that used to leave the first row, and the album card, blank.
        let thumbnail = lyrics_core::thumbnail_jpeg(&jpeg_bytes()).expect("jpeg to thumbnail");
        loader
            .inner
            .thumbs
            .lock()
            .unwrap()
            .insert("b.mp3".to_string(), Some(Arc::new(thumbnail)));

        assert!(loader.album_art("Album1").is_some());
        // Decoded once, and remembered under the album's own key so the rest of
        // the album's rows cost nothing.
        assert!(album_cover_image("Album1", &loader).is_some());
        assert!(cached_cover_image(&album_image_key("Album1")).is_some());
        // An album with no artwork must stay a placeholder rather than borrow
        // whichever cover happened to be decoded last.
        assert!(album_cover_image("Album2", &loader).is_none());
        assert!(cached_cover_image(&album_image_key("Album2")).is_none());
    }

    #[test]
    fn rows_are_indexed_by_file_and_by_album() {
        let rows = vec![
            track_to_ui(&mk_track("a.mp3", "A", "T1", "Album1"), None),
            track_to_ui(&mk_track("b.mp3", "A", "T2", "Album1"), None),
            track_to_ui(&mk_track("c.mp3", "B", "T3", "Album2"), None),
        ];
        let (by_path, by_album) = row_index(&rows);
        // Arriving artwork is written through these, so a row that is not listed
        // here would never be filled in.
        assert_eq!(by_path["b.mp3"], vec![1]);
        assert_eq!(by_album["Album1"], vec![0, 1]);
        assert_eq!(by_album["Album2"], vec![2]);
    }

    /// "Index the whole library after it's in the directory": every file gets a
    /// job, whether or not it is on screen, and album artwork goes first so the
    /// grid fills in before the backfill does.
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

        // And a second pass queues nothing: the index already has those files.
        index_library_art(&tracks, &loader);
        assert!(loader.next_job().is_none());
    }

    #[test]
    fn urgent_cover_lookup_checks_thumbnail_store_and_caches_result() {
        let temp_dir = std::env::temp_dir().join(format!("synced_lyrics_test_{}", std::process::id()));
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
        };

        // When cover_for is called with urgent = true:
        // Even if loader.inner.thumbs was empty, it reads and decodes the image directly.
        let full_stamp = lyrics_core::artwork_stamp(std::path::Path::new(&track.audio_path), art_stamp(&track));
        assert!(store.get(&track.audio_path, full_stamp).flatten().is_some());

        // Clean up temp dir
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn mk_track(path: &str, artist: &str, title: &str, album: &str) -> Track {
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
        }
    }
}
