use lofty::prelude::TaggedFileExt;
use lofty::probe::Probe;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub fn extract_cover_art(path: &Path) -> Option<Vec<u8>> {
    let probe = Probe::open(path).ok()?;
    let probe = match probe.guess_file_type() {
        Ok(p) => p,
        Err(_) => Probe::open(path).ok()?,
    };
    let tagged_file = probe.read().ok()?;

    // Gather all tags across primary, first, and secondary
    let mut all_tags: Vec<&lofty::tag::Tag> = Vec::new();
    if let Some(t) = tagged_file.primary_tag() {
        all_tags.push(t);
    }
    if let Some(t) = tagged_file.first_tag() {
        if !all_tags.iter().any(|existing| std::ptr::eq(*existing, t)) {
            all_tags.push(t);
        }
    }
    for t in tagged_file.tags() {
        if !all_tags.iter().any(|existing| std::ptr::eq(*existing, t)) {
            all_tags.push(t);
        }
    }

    // 1. Look for explicit CoverFront with non-empty data
    for tag in &all_tags {
        if let Some(picture) = tag.pictures().iter().find(|p| {
            p.pic_type() == lofty::picture::PictureType::CoverFront && !p.data().is_empty()
        }) {
            return Some(picture.data().to_vec());
        }
    }

    // 2. Fall back to any picture with non-empty data
    for tag in &all_tags {
        if let Some(picture) = tag.pictures().iter().find(|p| !p.data().is_empty()) {
            return Some(picture.data().to_vec());
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Artwork sources
// ---------------------------------------------------------------------------
//
// A cover can live in two places, and a real library uses both: embedded in the
// tags, or as an image file sitting next to the audio. Only looking at the tags
// is why part of a library shows artwork and the rest shows placeholders —
// untagged rips and downloads habitually carry a `cover.jpg` and nothing else.

/// File names that mean "this is the album cover", most specific first. Matched
/// case-insensitively and against the stem, so `Cover.JPG` and `folder.png` both
/// count.
const COVER_FILE_STEMS: &[&str] = &[
    "cover",
    "folder",
    "front",
    "album",
    "albumart",
    "album art",
    "artwork",
    "folder_art",
    "cover_art",
    "thumb",
];

/// Extensions worth reading. The image crate is built with all of these.
const COVER_FILE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"];

/// Names that clearly are *not* the front cover, so a folder that also holds
/// scans of the back cover or the disc does not show those instead.
const NON_COVER_STEMS: &[&str] = &[
    "back", "cd", "disc", "disk", "inlay", "spine", "tray", "inside", "rear", "booklet",
];

/// What one look at a folder told us: the images in it that could be a cover,
/// and a fingerprint of those files.
struct FolderArt {
    /// Candidate cover files, sorted so the choice is the same on every run.
    images: Vec<PathBuf>,
    /// Identifies the folder's artwork: any file appearing, disappearing or
    /// being rewritten changes it.
    fingerprint: u64,
}

/// Directory -> its listing. Libraries keep hundreds of tracks per folder and
/// every one of them asks, so a directory is read once instead of once per
/// track.
fn folder_art_cache() -> &'static Mutex<HashMap<PathBuf, Arc<FolderArt>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<FolderArt>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The thumbnail decoded from one cover file, or `None` when that file is not a
/// usable image.
type FolderThumbnail = Option<Arc<Vec<u8>>>;

/// Cover file -> the thumbnail decoded from it. An entry that is `None` means
/// "that file is not a usable image". Every track of an album shares one cover
/// file, so without this each of them would read *and decode* the same artwork
/// again — which is most of what a first index of an untagged library spends its
/// time on.
fn folder_thumbnail_cache() -> &'static Mutex<HashMap<PathBuf, FolderThumbnail>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, FolderThumbnail>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Forgets everything worked out about folders' artwork: which images they
/// hold, which of them is the cover, and the thumbnails decoded from them. Call
/// this after a scan: that is when new files — and new covers — appear, and a
/// folder remembered as having none would otherwise stay bare for the rest of
/// the session.
pub fn clear_folder_art_cache() {
    if let Ok(mut cache) = folder_art_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = folder_thumbnail_cache().lock() {
        cache.clear();
    }
}

/// The images in `directory` that could be a cover, plus their fingerprint.
/// Read from disk at most once per folder; the lock is only held for the
/// lookup, so parallel art workers never queue behind each other's reads.
fn folder_art(directory: &Path) -> Arc<FolderArt> {
    if let Ok(cache) = folder_art_cache().lock() {
        if let Some(known) = cache.get(directory) {
            return Arc::clone(known);
        }
    }
    let listing = Arc::new(read_folder_art(directory));
    if let Ok(mut cache) = folder_art_cache().lock() {
        cache.insert(directory.to_path_buf(), Arc::clone(&listing));
    }
    listing
}

fn read_folder_art(directory: &Path) -> FolderArt {
    let mut candidates: Vec<(PathBuf, u64, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase());
            if !COVER_FILE_EXTENSIONS.contains(&extension.as_deref().unwrap_or("")) {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_ascii_lowercase())
                .unwrap_or_default();
            let is_non_cover = NON_COVER_STEMS.iter().any(|&non| {
                stem == non
                    || stem.starts_with(&format!("{non}-"))
                    || stem.starts_with(&format!("{non}_"))
                    || stem.starts_with(&format!("{non} "))
                    || stem.ends_with(&format!("-{non}"))
                    || stem.ends_with(&format!("_{non}"))
                    || stem.ends_with(&format!(" {non}"))
            });
            if is_non_cover {
                continue;
            }
            // Size and timestamp, not just the name: replacing `cover.jpg`
            // with a different picture has to be noticed too.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|age| age.as_secs())
                .unwrap_or(0);
            candidates.push((path, metadata.len(), modified));
        }
    }
    // Sorted before the fingerprint is folded, because the order `read_dir`
    // hands entries back in is not defined by anything we can rely on.
    candidates.sort();
    let mut fingerprint = FNV_OFFSET;
    for (path, len, modified) in &candidates {
        fold(&mut fingerprint, &fingerprint_path(path).to_le_bytes());
        fold(&mut fingerprint, &len.to_le_bytes());
        fold(&mut fingerprint, &modified.to_le_bytes());
    }
    FolderArt {
        images: candidates.into_iter().map(|(path, _, _)| path).collect(),
        fingerprint,
    }
}

fn is_cover_keyword(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    let norm = lower.replace(['-', '_', ' '], "");
    norm.contains("cover")
        || norm.contains("front")
        || norm.contains("folder")
        || norm.contains("albumart")
        || norm.contains("artwork")
        || COVER_FILE_STEMS.iter().any(|&preferred| norm == preferred.replace(['-', '_', ' '], ""))
}

/// The cover file stored beside `audio_path`, if there is one.
fn cover_file_for(directory: &Path, audio_path: &Path) -> Option<PathBuf> {
    let listing = folder_art(directory);
    let images = listing.images.as_slice();
    if images.is_empty() {
        return None;
    }

    // 1. An image named after the track itself. `Song.mp3` + `Song.jpg`
    if let Some(stem) = audio_path.file_stem().and_then(|stem| stem.to_str()) {
        if let Some(path) = images.iter().find(|path| stem_matches(path, stem)) {
            return Some(path.clone());
        }
    }

    // 2. Exact match with preferred cover names (cover, folder, front, album, etc.)
    for preferred in COVER_FILE_STEMS {
        if let Some(path) = images.iter().find(|path| stem_matches(path, preferred)) {
            return Some(path.clone());
        }
    }

    // 3. Substring / keyword match (e.g. AlbumArtLarge, AlbumArt_{GUID}_Large, cover-front, front_cover, folder-art)
    let mut matching_candidates: Vec<&PathBuf> = images
        .iter()
        .filter(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(is_cover_keyword)
        })
        .collect();

    if !matching_candidates.is_empty() {
        matching_candidates.sort_by(|a, b| {
            let a_stem = a.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
            let b_stem = b.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
            let score = |s: &str| -> i32 {
                let mut sc = 0;
                if s.contains("large") { sc += 10; }
                if s.contains("front") { sc += 8; }
                if s.contains("cover") { sc += 6; }
                if s.contains("folder") { sc += 4; }
                if s.contains("albumart") { sc += 3; }
                if s.contains("small") { sc -= 10; }
                sc
            };
            score(&b_stem).cmp(&score(&a_stem))
        });
        return Some(matching_candidates[0].clone());
    }

    // 4. An image named after the folder
    if let Some(name) = directory.file_name().and_then(|name| name.to_str()) {
        if let Some(path) = images.iter().find(|path| stem_matches(path, name)) {
            return Some(path.clone());
        }
    }

    // 5. The folder's only image
    if images.len() == 1 {
        images.first().cloned()
    } else {
        None
    }
}

fn stem_matches(path: &Path, stem: &str) -> bool {
    path.file_stem()
        .and_then(|candidate| candidate.to_str())
        .is_some_and(|candidate| {
            candidate.eq_ignore_ascii_case(stem)
                || candidate.replace(['-', '_', ' '], "").eq_ignore_ascii_case(&stem.replace(['-', '_', ' '], ""))
        })
}

/// The cover image stored beside `audio_path`, if there is one.
pub fn folder_cover_art(audio_path: &Path) -> Option<Vec<u8>> {
    let directory = audio_path.parent()?;
    let found = cover_file_for(directory, audio_path)?;
    fs::read(found).ok()
}

/// The cover beside `audio_path`, already shrunk to a thumbnail. Decoded at
/// most once per cover file, however many tracks share the folder.
pub fn folder_cover_thumbnail(audio_path: &Path) -> FolderThumbnail {
    let directory = audio_path.parent()?;
    let found = cover_file_for(directory, audio_path)?;

    // Resolve under the lock, then decode outside it: decoding takes
    // milliseconds and the art workers run in parallel.
    if let Ok(cache) = folder_thumbnail_cache().lock() {
        if let Some(known) = cache.get(&found).cloned() {
            return known;
        }
    }
    let thumbnail = fs::read(&found)
        .ok()
        .and_then(|bytes| thumbnail_jpeg(&bytes))
        .map(Arc::new);
    if let Ok(mut cache) = folder_thumbnail_cache().lock() {
        cache.insert(found, thumbnail.clone());
    }
    thumbnail
}

/// A fingerprint of the images sitting next to `audio_path`, or `0` when there
/// are none. Shares the folder listing with the cover lookup, so asking about
/// every track of an album costs one directory read in total.
pub fn folder_art_fingerprint(audio_path: &Path) -> u64 {
    audio_path
        .parent()
        .map(|directory| folder_art(directory).fingerprint)
        .unwrap_or(0)
}

/// The stamp an artwork index entry is keyed by: the audio file's own timestamp
/// together with the fingerprint of the images beside it.
///
/// The audio file's timestamp alone is not enough. Half a library keeps its
/// cover in a file next to the music rather than in the tags, and dropping a
/// `cover.jpg` into a folder touches no audio file at all — so without the
/// second half of this, a track remembered as "no artwork" would stay blank
/// forever.
pub fn artwork_stamp(audio_path: &Path, audio_stamp: u64) -> u64 {
    mix(audio_stamp, folder_art_fingerprint(audio_path))
}

/// Embedded artwork first, then the folder's own cover file.
pub fn extract_cover_art_with_fallback(path: &Path) -> Option<Vec<u8>> {
    extract_cover_art(path).or_else(|| folder_cover_art(path))
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Folds `bytes` into a running FNV-1a hash.
fn fold(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// 64-bit FNV-1a over two values, used to fold a file's timestamp and its
/// folder's fingerprint into the one key an index entry has.
fn mix(a: u64, b: u64) -> u64 {
    let mut hash = FNV_OFFSET;
    fold(&mut hash, &a.to_le_bytes());
    fold(&mut hash, &b.to_le_bytes());
    hash
}

/// The same hash, over a file name. Folded unit-wise rather than through
/// `Path::to_string_lossy`, because the names come back as `OsStr`.
fn fingerprint_path(path: &Path) -> u64 {
    let mut hash = FNV_OFFSET;
    for unit in path.as_os_str().to_string_lossy().encode_utf16() {
        fold(&mut hash, &unit.to_le_bytes());
    }
    hash
}

// ---------------------------------------------------------------------------
// Thumbnails
// ---------------------------------------------------------------------------

/// Longest edge of a stored thumbnail.
///
/// The largest cover the UI draws is a 124px album card, so anything past
/// ~190px is invisible on a 150% display — and it is not free: every cover the
/// app holds costs three bytes a pixel, and the album grid keeps one per album.
/// At 320px that is 300KB each and a thousand albums' worth of artwork is enough
/// memory to make scrolling an album grid stutter; at 192 it is a third of that
/// and still sharp.
pub const THUMBNAIL_MAX_PX: u32 = 192;

/// JPEG quality for the on-disk thumbnails: high enough that the artwork still
/// looks right, low enough that a 2 000-track index is tens of megabytes rather
/// than gigabytes.
const THUMBNAIL_QUALITY: u8 = 82;

fn shrink(image: image::DynamicImage) -> image::DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width > THUMBNAIL_MAX_PX || height > THUMBNAIL_MAX_PX {
        image.thumbnail(THUMBNAIL_MAX_PX, THUMBNAIL_MAX_PX)
    } else {
        image
    }
}

/// Decodes `bytes` (any format the app was built with) and re-encodes it as a
/// small JPEG thumbnail. `None` means "not a decodable image", which is how a
/// track ends up with a placeholder instead of a wrong cover.
pub fn thumbnail_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    let thumb = shrink(image::load_from_memory(bytes).ok()?);
    let rgb = thumb.to_rgb8();
    let mut encoded = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, THUMBNAIL_QUALITY);
    encoder.encode_image(&rgb).ok()?;
    Some(encoded)
}

// ---------------------------------------------------------------------------
// On-disk thumbnail index
// ---------------------------------------------------------------------------

/// Covers that have already been extracted and decoded once, kept on disk.
///
/// This is what makes "index the library" mean something: the first scan has to
/// read and decode every audio file, but every launch after that paints its
/// covers from a few dozen kilobytes of thumbnails instead of opening a
/// thousand songs. Files that turned out to have no artwork are remembered too
/// (as an empty entry), because finding that out is the expensive part.
pub struct ThumbnailStore {
    directory: PathBuf,
}

impl ThumbnailStore {
    pub fn open(directory: PathBuf) -> Self {
        fs::create_dir_all(&directory).ok();
        Self { directory }
    }

    /// The app's own cache, beside the settings and the library database.
    pub fn default_location() -> Option<Self> {
        crate::app_data_dir()
            .ok()
            .map(|directory| Self::open(directory.join("covers")))
    }

    /// `stamp` is the audio file's modification time, folded with the state of
    /// the images beside it (see [`artwork_stamp`]). It is part of the key, so
    /// re-tagging a file — or adding a cover next to it — invalidates exactly
    /// that one entry.
    fn entry_path(&self, audio_path: &str, stamp: u64) -> PathBuf {
        self.directory
            .join(format!("{:016x}.img", fingerprint(audio_path, stamp)))
    }

    /// `Some(None)` means "this file had no artwork when we last looked"; `None`
    /// means we have never looked.
    pub fn get(&self, audio_path: &str, stamp: u64) -> Option<Option<Vec<u8>>> {
        match fs::read(self.entry_path(audio_path, stamp)) {
            Ok(bytes) if bytes.is_empty() => Some(None),
            Ok(bytes) => Some(Some(bytes)),
            Err(_) => None,
        }
    }

    pub fn put(&self, audio_path: &str, stamp: u64, thumbnail: Option<&[u8]>) {
        // An empty file is the "no artwork" marker: it costs nothing and tells
        // the next launch not to read that file again.
        let _ = fs::write(
            self.entry_path(audio_path, stamp),
            thumbnail.unwrap_or_default(),
        );
    }

    // Entries are a couple of dozen kilobytes each and are never evicted. A
    // re-tagged file leaves its old entry behind, so the directory is allowed to
    // quietly accumulate the few stale files that implies; it is a cache in the
    // app's own data directory, and the alternative — tracking every entry to
    // sweep it — costs more than the space it would win back.
}

/// Stable 64-bit FNV-1a over the path and the file's timestamp.
///
/// `DefaultHasher` would be wrong here: it is explicitly not stable across Rust
/// releases, so a toolchain upgrade would silently turn every cached entry into
/// a miss — or, worse, an entry belonging to a different track.
pub fn fingerprint(value: &str, stamp: u64) -> u64 {
    let mut hash = FNV_OFFSET;
    fold(&mut hash, value.as_bytes());
    fold(&mut hash, &stamp.to_le_bytes());
    hash
}

pub struct ArtCache {
    inner: Mutex<ArtCacheInner>,
    cache_dir: PathBuf,
    max_memory: usize,
}

struct ArtCacheInner {
    memory: HashMap<String, Vec<u8>>, // key -> raw image bytes
    access_order: Vec<String>,        // LRU tracking
    negative: HashSet<String>,        // keys known to have no art
}

impl ArtCache {
    pub fn new(cache_dir: PathBuf, max_memory: usize) -> Self {
        fs::create_dir_all(&cache_dir).ok();
        Self {
            inner: Mutex::new(ArtCacheInner {
                memory: HashMap::new(),
                access_order: Vec::new(),
                negative: HashSet::new(),
            }),
            cache_dir,
            max_memory,
        }
    }

    fn generate_key(&self, audio_path: &Path) -> String {
        let path_str = audio_path.to_string_lossy().to_string();
        let mut hasher = DefaultHasher::new();
        path_str.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    pub fn get(&self, audio_path: &Path) -> Option<Vec<u8>> {
        let key = self.generate_key(audio_path);

        let mut inner = self.inner.lock().unwrap();

        if inner.negative.contains(&key) {
            return None;
        }

        if let Some(art) = inner.memory.get(&key) {
            let art = art.clone();
            inner.access_order.retain(|k| k != &key);
            inner.access_order.push(key.clone());
            return Some(art);
        }

        let disk_path = self.cache_dir.join(format!("{}_master.png", key));
        if let Ok(art) = fs::read(&disk_path) {
            if inner.memory.len() >= self.max_memory {
                if let Some(evict_key) = inner.access_order.first().cloned() {
                    inner.memory.remove(&evict_key);
                    inner.access_order.remove(0);
                }
            }
            inner.memory.insert(key.clone(), art.clone());
            inner.access_order.push(key.clone());
            return Some(art);
        }

        None
    }

    pub fn get_or_extract(&self, audio_path: &Path) -> Option<Vec<u8>> {
        if let Some(art) = self.get(audio_path) {
            return Some(art);
        }

        let key = self.generate_key(audio_path);

        if let Some(art) = extract_cover_art(audio_path) {
            let mut inner = self.inner.lock().unwrap();

            let disk_path = self.cache_dir.join(format!("{}_master.png", key));
            fs::write(disk_path, &art).ok();

            if inner.memory.len() >= self.max_memory {
                if let Some(evict_key) = inner.access_order.first().cloned() {
                    inner.memory.remove(&evict_key);
                    inner.access_order.remove(0);
                }
            }
            inner.memory.insert(key.clone(), art.clone());
            inner.access_order.push(key.clone());

            Some(art)
        } else {
            let mut inner = self.inner.lock().unwrap();
            inner.negative.insert(key);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG, i.e. the smallest thing that is really an image.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "synced-lyrics-art-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_cover_next_to_the_audio_is_used_when_the_tags_have_none() {
        let dir = scratch_dir("folder");
        fs::write(dir.join("Song.mp3"), []).unwrap();
        // Neither of these is the front cover and neither may be picked up.
        fs::write(dir.join("back.jpg"), TINY_PNG).unwrap();
        fs::write(dir.join("notes.txt"), b"not an image").unwrap();
        assert!(folder_cover_art(&dir.join("Song.mp3")).is_none());

        // Dropping a cover into the folder must be picked up after a rescan,
        // which is the only thing that clears the "this folder has none" memo.
        fs::write(dir.join("Cover.JPG"), TINY_PNG).unwrap();
        clear_folder_art_cache();
        // `Cover.JPG` must match case-insensitively.
        assert_eq!(
            folder_cover_art(&dir.join("Song.mp3")).as_deref(),
            Some(TINY_PNG)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_named_image_counts_as_the_cover() {
        let dir = scratch_dir("folder-named");
        fs::write(dir.join("track.flac"), []).unwrap();
        // `<folder>/<folder>.jpg` is the other common convention.
        let named = dir.join(format!(
            "{}.jpg",
            dir.file_name().unwrap().to_str().unwrap()
        ));
        fs::write(&named, TINY_PNG).unwrap();

        assert_eq!(
            folder_cover_art(&dir.join("track.flac")).as_deref(),
            Some(TINY_PNG)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unrelated_image_in_the_folder_is_not_treated_as_a_cover() {
        // A flat library full of artwork-less files must not have every row
        // painted with whichever image happens to sit in the folder.
        let dir = scratch_dir("unrelated");
        fs::write(dir.join("song.mp3"), []).unwrap();
        fs::write(dir.join("band-photo.jpg"), TINY_PNG).unwrap();
        fs::write(dir.join("01.png"), TINY_PNG).unwrap();

        assert!(folder_cover_art(&dir.join("song.mp3")).is_none());

        fs::remove_dir_all(dir).unwrap();
    }

    /// Every tagger and downloader writes `<track>.jpg` beside `<track>.mp3`.
    /// Those files used to be ignored because they are not called `cover`,
    /// which is a large part of why one library showed artwork in some rows and
    /// placeholders in the rest.
    #[test]
    fn an_image_named_after_the_track_is_that_tracks_cover() {
        let dir = scratch_dir("track-named");
        fs::write(dir.join("First.mp3"), []).unwrap();
        fs::write(dir.join("Second.mp3"), []).unwrap();
        fs::write(dir.join("First.jpg"), TINY_PNG).unwrap();
        fs::write(dir.join("Second.jpg"), TINY_PNG).unwrap();

        // Each track gets the image it is named after, not the folder's first.
        assert_eq!(
            folder_cover_art(&dir.join("First.mp3")).as_deref(),
            Some(TINY_PNG)
        );
        assert_eq!(
            folder_cover_art(&dir.join("Second.mp3")).as_deref(),
            Some(TINY_PNG)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// A folder holding exactly one image next to its music means that image to
    /// be the artwork, even when neither track nor folder is named like it —
    /// `Artist - Album.jpg` beside the tracks of that album is a common rip.
    #[test]
    fn the_only_image_in_a_folder_is_the_cover() {
        let dir = scratch_dir("only-image");
        fs::write(dir.join("01 Song.mp3"), []).unwrap();
        fs::write(dir.join("02 Other.mp3"), []).unwrap();
        fs::write(dir.join("Artist - Album.jpg"), TINY_PNG).unwrap();

        assert_eq!(
            folder_cover_art(&dir.join("01 Song.mp3")).as_deref(),
            Some(TINY_PNG)
        );
        assert_eq!(
            folder_cover_art(&dir.join("02 Other.mp3")).as_deref(),
            Some(TINY_PNG)
        );

        fs::remove_dir_all(dir).unwrap();
    }

    /// One cover file shared by a whole album is decoded once, and the answer
    /// is remembered until a scan clears the folder cache.
    #[test]
    fn a_folder_cover_is_decoded_once_and_remembered() {
        let dir = scratch_dir("shared-cover");
        fs::write(dir.join("01.mp3"), []).unwrap();
        fs::write(dir.join("02.mp3"), []).unwrap();
        fs::write(dir.join("cover.jpg"), TINY_PNG).unwrap();

        let first = folder_cover_thumbnail(&dir.join("01.mp3")).expect("cover should decode");
        let second = folder_cover_thumbnail(&dir.join("02.mp3")).expect("cover should decode");
        // Same thumbnail, and not two copies of it.
        assert!(Arc::ptr_eq(&first, &second));

        // Replacing the cover behind the cache's back is only noticed once a
        // scan clears it, which is exactly when the app does.
        fs::write(dir.join("cover.jpg"), b"not an image").unwrap();
        assert!(folder_cover_thumbnail(&dir.join("01.mp3")).is_some());
        clear_folder_art_cache();
        assert!(folder_cover_thumbnail(&dir.join("01.mp3")).is_none());

        fs::remove_dir_all(dir).unwrap();
    }

    /// The index key has to notice a cover appearing next to the audio, or a
    /// track remembered as "no artwork" stays blank for good.
    #[test]
    fn a_cover_appearing_beside_the_audio_changes_the_index_stamp() {
        let dir = scratch_dir("stamp");
        let track = dir.join("Song.mp3");
        fs::write(&track, []).unwrap();

        let without_cover = artwork_stamp(&track, 7);
        assert_eq!(
            artwork_stamp(&track, 7),
            without_cover,
            "the stamp has to be the same on every run, or the index never hits"
        );

        fs::write(dir.join("Song.jpg"), TINY_PNG).unwrap();
        clear_folder_art_cache();
        let with_cover = artwork_stamp(&track, 7);
        assert_ne!(without_cover, with_cover);

        // Replacing it with a different picture has to be noticed too.
        fs::write(dir.join("Song.jpg"), b"not an image").unwrap();
        clear_folder_art_cache();
        assert_ne!(with_cover, artwork_stamp(&track, 7));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn thumbnails_round_trip_and_are_shrunk() {
        // Big enough that the resize has something to do.
        let mut source = image::RgbImage::new(900, 700);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 120]);
        }
        let mut png = Vec::new();
        source
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let thumbnail = thumbnail_jpeg(&png).expect("png should convert to a thumbnail");
        // What we store has to be readable again by the decoder the UI uses.
        let decoded = image::load_from_memory(&thumbnail).expect("thumbnail should decode");
        assert!(
            decoded.width() <= THUMBNAIL_MAX_PX && decoded.height() <= THUMBNAIL_MAX_PX,
            "thumbnail should be shrunk to {THUMBNAIL_MAX_PX}px, got {}x{}",
            decoded.width(),
            decoded.height()
        );

        // Anything that is not an image must be reported as such, not guessed at.
        assert!(thumbnail_jpeg(b"definitely not an image").is_none());
    }

    #[test]
    fn the_thumbnail_index_remembers_art_and_the_lack_of_it() {
        let dir = scratch_dir("store");
        let store = ThumbnailStore::open(dir.clone());

        // Never looked at this file yet.
        assert_eq!(store.get("C:\\music\\a.mp3", 111), None);

        store.put("C:\\music\\a.mp3", 111, Some(TINY_PNG));
        store.put("C:\\music\\b.mp3", 111, None);
        assert_eq!(
            store.get("C:\\music\\a.mp3", 111),
            Some(Some(TINY_PNG.to_vec()))
        );
        // "No artwork" is worth remembering: finding that out means opening and
        // parsing the file, which is the expensive part.
        assert_eq!(store.get("C:\\music\\b.mp3", 111), Some(None));

        // A re-tagged file (new mtime) must not reuse the old entry.
        assert_eq!(store.get("C:\\music\\a.mp3", 222), None);
        // A different path must never collide with an existing entry.
        assert_eq!(store.get("C:\\music\\c.mp3", 111), None);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_art_cache_memory() {
        let cache = ArtCache::new(PathBuf::from("dummy_dir"), 2);
        let path = Path::new("dummy/path/audio.mp3");
        let key = cache.generate_key(path);

        {
            let mut inner = cache.inner.lock().unwrap();
            inner.memory.insert(key.clone(), vec![1, 2, 3]);
            inner.access_order.push(key.clone());
        }

        let art = cache.get(path);
        assert_eq!(art, Some(vec![1, 2, 3]));
    }
}
