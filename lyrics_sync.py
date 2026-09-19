#!/usr/bin/env python3
"""
lyrics_sync.py - fast, resumable .lrc manager for large music libraries.
Always searches for SYNCED lyrics first, then falls back to PLAIN lyrics.
Every platform is asked separately and carefully (lrclib, Musixmatch, NetEase,
Megalobiz, Genius), so a miss on one never hides a hit on another. It is slow
on purpose; use --fast for a quicker, less polite run.

  python lyrics_sync.py                 FULL run
      1. songs with no .lrc                      -> download (synced, else plain)
      2. .lrc that is plain / broken             -> replace with synced lyrics
      3. synced .lrc that looks WRONG (audit)    -> re-download, replace only if
                                                    the new lyrics pass the checks
  (each song is first looked up directly on lrclib by artist + title + length)
  python lyrics_sync.py --missing-only  QUICK run: steps 1 and 2 only (no audit)

Audit checks (step 3):
  - last lyric timestamp vs. real audio length (runs past the end / ends way too early)
  - [ti:] title tag inside the .lrc vs. the song's title

Setup:  pip install -U syncedlyrics mutagen tqdm
"""
import argparse
import json
import logging
import os
import re
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from difflib import SequenceMatcher
from pathlib import Path

import requests
from syncedlyrics.providers import Genius, Megalobiz, Musixmatch, NetEase
from mutagen import File as MutagenFile
from tqdm import tqdm

AUDIO_EXT = ('.flac', '.mp3', '.m4a', '.opus', '.ogg', '.wav')
# Platforms searched for every song, one at a time (lrclib is handled by a direct lookup):
SYNCED_PLATFORMS = ["Musixmatch", "NetEase", "Megalobiz"]   # can return timestamps
PLAIN_PLATFORM = "Genius"                                   # plain text only, asked last
PLATFORM_FACTORY = {
    "Musixmatch": lambda: Musixmatch(lang=None, enhanced=False),
    "NetEase": NetEase,
    "Megalobiz": Megalobiz,
    "Genius": Genius,
}

STAMP_RE = re.compile(r"\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]")
TITLE_TAG_RE = re.compile(r"^\s*\[ti:([^\]]*)\]", re.I | re.M)
NOISE_RE = re.compile(
    r"[\(\[][^\)\]]*(feat|ft\.|remaster|version|live|deluxe|edit|mono|stereo|bonus)[^\)\]]*[\)\]]",
    re.I,
)

# Direct lrclib lookup (matches on artist + title + song length)
LRCLIB_SEARCH = "https://lrclib.net/api/search"
LRCLIB_HEADERS = {"User-Agent": "lyrics_sync.py (personal library tool)"}
DURATION_TOLERANCE = 3       # seconds the lrclib entry may differ from the audio length

# Audit thresholds
PAST_END_TOLERANCE = 10      # seconds lyrics may run past the audio length
EARLY_END_RATIO = 0.5        # lyrics ending before 50% of the song ...
EARLY_END_GAP = 45           # ... AND more than 45 s before the end => suspicious

SCRIPT_DIR = Path(__file__).resolve().parent
CACHE_FILE = SCRIPT_DIR / ".lyrics_cache.json"    # remembers tracks with nothing better to fetch
NOT_FOUND_LOG = SCRIPT_DIR / "not_found.txt"
SUSPICIOUS_LOG = SCRIPT_DIR / "suspicious.txt"
REJECTED_LOG = SCRIPT_DIR / "rejected.txt"        # lyrics found online but discarded as wrong


# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
def parse_stamps(text):
    out = []
    for m, s, f in STAMP_RE.findall(text):
        t = int(m) * 60 + int(s)
        if f:
            t += int(f) / (10 ** len(f))
        out.append(t)
    return out


def count_timestamps(text):
    return len(STAMP_RE.findall(text))


def read_text(path, limit=None):
    try:
        with open(path, "r", encoding="utf-8", errors="ignore") as f:
            return f.read(limit) if limit else f.read()
    except OSError:
        return ""


def is_valid_synced_lrc(path):
    """True if file has >= 3 real timestamps. Reads only the first 16 KB."""
    return count_timestamps(read_text(path, 16384)) >= 3


def existing_size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0


def read_audio_info(path):
    """Returns (artist, title, duration_seconds_or_None)."""
    artist = title = duration = None
    try:
        meta = MutagenFile(path, easy=True)
        if meta:
            artist = (meta.get("artist") or [None])[0]
            title = (meta.get("title") or [None])[0]
            info = getattr(meta, "info", None)
            duration = getattr(info, "length", None)
    except Exception:
        pass
    if not title:
        base = os.path.splitext(os.path.basename(path))[0]
        if " - " in base:
            artist, title = [p.strip() for p in base.split(" - ", 1)]
        else:
            title = base
    return artist, title, duration


def build_queries(artist, title):
    """Primary query first, then a cleaned-up fallback (removes '(feat. X)', '[Remastered]', ...)."""
    def q(a, t):
        return f"{a} {t}".strip() if a else t

    queries = [q(artist, title)]
    cleaned = re.sub(r"\s+", " ", NOISE_RE.sub("", title)).strip(" -")
    if cleaned and cleaned != title:
        queries.append(q(artist, cleaned))
    return queries


def write_atomic(path, text):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    os.replace(tmp, path)


def fmt_mmss(sec):
    sec = int(sec)
    return f"{sec // 60}:{sec % 60:02d}"


def fmt_time(sec):
    sec = int(sec)
    h, r = divmod(sec, 3600)
    m, s = divmod(r, 60)
    return f"{h}h {m:02d}m {s:02d}s" if h else f"{m}m {s:02d}s"


# --------------------------------------------------------------------------- #
# Lyrics sanity checks (used by the audit and to validate replacements)
# --------------------------------------------------------------------------- #
def _norm(s):
    return re.sub(r"[\W_]+", "", NOISE_RE.sub("", s).lower())


def titles_match(a, b):
    a, b = _norm(a), _norm(b)
    if not a or not b:
        return True
    return a in b or b in a or SequenceMatcher(None, a, b).ratio() >= 0.6


def check_lrc(text, duration, title):
    """Returns None if the synced lyrics look right, else a short reason string."""
    stamps = parse_stamps(text)
    if len(stamps) < 3:
        return "no timestamps"
    last = max(stamps)
    if duration:
        if last > duration + PAST_END_TOLERANCE:
            return f"lyrics run past song end (lyrics {fmt_mmss(last)}, song {fmt_mmss(duration)})"
        if last < duration * EARLY_END_RATIO and duration - last > EARLY_END_GAP:
            return f"lyrics end too early (lyrics {fmt_mmss(last)}, song {fmt_mmss(duration)})"
    m = TITLE_TAG_RE.search(text)
    if m and title and m.group(1).strip() and not titles_match(m.group(1), title):
        return f"title tag mismatch ('{m.group(1).strip()}' vs '{title}')"
    return None


def audit_one(item):
    audio, lrc = item
    _, title, duration = read_audio_info(audio)
    return audio, lrc, check_lrc(read_text(lrc), duration, title)


def run_audit(items, workers):
    flagged = []
    with ThreadPoolExecutor(max_workers=workers) as ex, \
            tqdm(total=len(items), unit="song", desc="Auditing", dynamic_ncols=True,
                 bar_format="{l_bar}{bar}| {n_fmt}/{total_fmt} [{elapsed}<{remaining}, {rate_fmt}] {postfix}"
                 ) as bar:
        futs = [ex.submit(audit_one, it) for it in items]
        try:
            for fut in as_completed(futs):
                try:
                    audio, lrc, reason = fut.result()
                except Exception:
                    bar.update(1)
                    continue
                if reason:
                    flagged.append((audio, lrc, reason))
                bar.set_postfix_str(f"suspicious:{len(flagged)}", refresh=False)
                bar.update(1)
        except KeyboardInterrupt:
            ex.shutdown(wait=False, cancel_futures=True)
            raise
    return flagged


# --------------------------------------------------------------------------- #
# Adaptive rate limiter (shared by all threads)
# --------------------------------------------------------------------------- #
class Limiter:
    """Spaces out requests globally. Slows down on 429s, gradually speeds back up."""

    def __init__(self, base_interval):
        self.base = base_interval
        self.interval = base_interval
        self.next_slot = 0.0
        self.lock = threading.Lock()

    def wait(self):
        with self.lock:
            now = time.monotonic()
            slot = max(now, self.next_slot)
            self.next_slot = slot + self.interval
        delay = slot - now
        if delay > 0:
            time.sleep(delay)

    def penalize(self):
        with self.lock:
            self.interval = min(self.interval * 2, 5.0)
            self.next_slot = max(self.next_slot, time.monotonic() + self.interval * 4)

    def relax(self):
        with self.lock:
            self.interval = max(self.base, self.interval * 0.95)


# --------------------------------------------------------------------------- #
# Scanning
# --------------------------------------------------------------------------- #
def scan_library(root):
    """
    Returns (todo, ok_items)
      todo:     [(audio, lrc, 'missing' | 'fix')]   no .lrc / .lrc without real timestamps
      ok_items: [(audio, lrc)]                       .lrc with valid timestamps
    """
    todo, ok_items = [], []
    dirs_seen = 0
    print(f"Scanning '{root}' ...")
    for dirpath, _, names in os.walk(root):
        dirs_seen += 1
        lrc_by_stem = {os.path.splitext(n)[0].lower(): n
                       for n in names if n.lower().endswith(".lrc")}
        for n in names:
            if not n.lower().endswith(AUDIO_EXT):
                continue
            stem = os.path.splitext(n)[0]
            audio = os.path.join(dirpath, n)
            existing = lrc_by_stem.get(stem.lower())
            if existing is None:
                todo.append((audio, os.path.join(dirpath, stem + ".lrc"), "missing"))
                continue
            lrc = os.path.join(dirpath, existing)
            if is_valid_synced_lrc(lrc):
                ok_items.append((audio, lrc))
            else:
                todo.append((audio, lrc, "fix"))
    print(f"Scanned {dirs_seen} folders.")
    return todo, ok_items


# --------------------------------------------------------------------------- #
# Worker
# --------------------------------------------------------------------------- #
def title_variants(title):
    cleaned = re.sub(r"\s+", " ", NOISE_RE.sub("", title)).strip(" -")
    return [title] + ([cleaned] if cleaned and cleaned != title else [])


def pick_lrclib(results, artist, title, duration):
    """Best lrclib record -> (synced_text, plain_text), or None.
    Requires matching title (+ artist when known) and a similar song length."""
    best, best_score = None, None
    for r in results or []:
        if not isinstance(r, dict) or r.get("instrumental"):
            continue
        synced = r.get("syncedLyrics") or ""
        plain = r.get("plainLyrics") or ""
        if count_timestamps(synced) < 3:
            synced = ""
        if not synced and not plain.strip():
            continue
        if not titles_match(r.get("trackName") or "", title):
            continue
        if artist and not titles_match(r.get("artistName") or "", artist):
            continue
        diff = 0.0
        if duration and r.get("duration"):
            diff = abs(float(r["duration"]) - duration)
            if diff > DURATION_TOLERANCE:
                continue
        score = (bool(synced), -diff)
        if best_score is None or score > best_score:
            best, best_score = (synced, plain), score
    return best


class Fetcher:
    def __init__(self, limiter):
        self.limiter = limiter
        self.memo = {}                    # cache so identical songs are only fetched once
        self.memo_lock = threading.Lock()
        self.local = threading.local()    # one provider instance per thread
        self.stats = {}                   # platform -> [found, nothing, errors]
        self.last_errors = {}             # platform -> last error text
        self.rejected = []                # (audio, platform, reason)
        self.stats_lock = threading.Lock()

    # ---- bookkeeping ----
    def _stat(self, name, idx):
        with self.stats_lock:
            self.stats.setdefault(name, [0, 0, 0])[idx] += 1

    def _error(self, name, msg):
        with self.stats_lock:
            self.stats.setdefault(name, [0, 0, 0])[2] += 1
            self.last_errors[name] = msg

    # ---- lrclib: direct, duration-matched lookup ----
    def _lrclib_request(self, params):
        for attempt in range(3):
            self.limiter.wait()
            try:
                r = requests.get(LRCLIB_SEARCH, params=params, headers=LRCLIB_HEADERS, timeout=15)
            except requests.RequestException as e:
                raise RuntimeError(f"lrclib: {type(e).__name__}: {e}") from e
            if r.status_code == 429:
                self.limiter.penalize()
                time.sleep(2 ** attempt)
                continue
            if r.status_code != 200:
                raise RuntimeError(f"lrclib: HTTP {r.status_code}")
            self.limiter.relax()
            try:
                return r.json()
            except ValueError:
                return []
        raise RuntimeError("lrclib: rate limited (HTTP 429) after 3 retries")

    def lrclib(self, artist, title, duration):
        """Returns (synced_text, plain_text); either may be ''. Raises RuntimeError on failure."""
        key = ("lrclib", artist, title, round(duration) if duration else None)
        with self.memo_lock:
            if key in self.memo:
                return self.memo[key]
        found = None
        try:
            for t in title_variants(title):
                params = {"track_name": t}
                if artist:
                    params["artist_name"] = artist
                found = pick_lrclib(self._lrclib_request(params), artist, t, duration)
                if found:
                    break
        except RuntimeError as e:
            self._error("lrclib", str(e))
            raise
        self._stat("lrclib", 0 if found else 1)
        found = found or ("", "")
        with self.memo_lock:
            self.memo[key] = found
        return found

    # ---- the other platforms: one request to one platform ----
    def _provider(self, name):
        cache = getattr(self.local, "providers", None)
        if cache is None:
            cache = self.local.providers = {}
        if name not in cache:
            cache[name] = PLATFORM_FACTORY[name]()
        return cache[name]

    def query_platform(self, name, query):
        """Returns (synced_text, plain_text); raises if the platform failed."""
        key = (name, query)
        with self.memo_lock:
            if key in self.memo:
                return self.memo[key]
        lyr = None
        for attempt in range(3):
            self.limiter.wait()
            try:
                lyr = self._provider(name).get_lrc(query)
                self.limiter.relax()
                break
            except Exception as e:
                msg = str(e)
                if "429" in msg or "Too Many" in msg:
                    self.limiter.penalize()
                    time.sleep(2 ** attempt)
                    continue
                self._error(name, f"{type(e).__name__}: {msg}")
                raise
        else:
            self._error(name, "rate limited (HTTP 429)")
            raise RuntimeError(f"{name}: rate limited (HTTP 429)")

        synced = (getattr(lyr, "synced", None) or "") if lyr else ""
        plain = (getattr(lyr, "unsynced", None) or "") if lyr else ""
        if count_timestamps(synced) < 3:
            synced = ""
        result = (synced, plain if plain.strip() else "")
        self._stat(name, 0 if (result[0] or result[1]) else 1)
        with self.memo_lock:
            self.memo[key] = result
        return result

    # ---- one song ----
    def process(self, item):
        audio, lrc, status = item
        artist, title, duration = read_audio_info(audio)
        queries = build_queries(artist, title)
        old_text = read_text(lrc).strip() if status == "suspicious" else None
        failed, tried = [], 0

        def acceptable(text, source):
            reason = check_lrc(text, duration, title)
            if reason:
                with self.stats_lock:
                    self.rejected.append((audio, source, reason))
                return False
            return status != "suspicious" or text.strip() != old_text

        plain = ""

        # 1) lrclib - matched on artist + title + song length
        tried += 1
        try:
            lr_synced, lr_plain = self.lrclib(artist, title, duration)
        except RuntimeError as e:
            failed.append(str(e))
            lr_synced = lr_plain = ""
        if lr_synced and acceptable(lr_synced, "lrclib"):
            write_atomic(lrc, lr_synced)
            return audio, "synced", queries[0]
        plain = lr_plain

        # 2) every other platform that can have timestamps, one after another
        for name in SYNCED_PLATFORMS:
            tried += 1
            worked = False
            for query in queries:
                try:
                    synced, unsynced = self.query_platform(name, query)
                except Exception:
                    continue
                worked = True
                if synced and acceptable(synced, name):
                    write_atomic(lrc, synced)
                    return audio, "synced", query
                if unsynced and not plain:
                    plain = unsynced
            if not worked:
                failed.append(name)

        # no synced lyrics anywhere: never touch a file that already has content
        if status == "suspicious":
            return audio, "kept", queries[0]
        if status == "fix" and existing_size(lrc) >= 32:
            return audio, "kept", queries[0]   # keeps the plain lyrics it already has

        # 3) plain fallback (Genius is only asked when nobody else had plain text)
        if not plain.strip():
            tried += 1
            worked = False
            for query in queries:
                try:
                    _, unsynced = self.query_platform(PLAIN_PLATFORM, query)
                except Exception:
                    continue
                worked = True
                if unsynced:
                    plain = unsynced
                    break
            if not worked:
                failed.append(PLAIN_PLATFORM)
        if plain.strip():
            write_atomic(lrc, plain)
            return audio, "plain", queries[0]

        if failed and len(failed) >= tried:
            raise RuntimeError("every platform failed: " + " | ".join(
                f"{n}: {self.last_errors.get(n, 'error')}" if n in self.last_errors else n
                for n in failed))
        return audio, "none", queries[0]


# --------------------------------------------------------------------------- #
# Cache of tracks where nothing better could be fetched
# --------------------------------------------------------------------------- #
def load_cache():
    try:
        return json.loads(CACHE_FILE.read_text(encoding="utf-8"))
    except Exception:
        return {}


def save_cache(cache):
    try:
        CACHE_FILE.write_text(json.dumps(cache), encoding="utf-8")
    except OSError:
        pass


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #
def main():
    ap = argparse.ArgumentParser(description="Fast lyrics manager for big libraries")
    ap.add_argument("--dir", default=r"D:\Music", help="music folder")
    ap.add_argument("--missing-only", action="store_true",
                    help="quick run: only songs with no lyrics or without synced lyrics (skips the audit)")
    ap.add_argument("--workers", type=int, default=2, help="parallel threads (default 2, slow and careful)")
    ap.add_argument("--interval", type=float, default=0.8,
                    help="min seconds between requests across all threads (default 0.8)")
    ap.add_argument("--fast", action="store_true", help="quicker and less careful: 4 workers, 0.25 s interval")
    ap.add_argument("--retry-days", type=float, default=14,
                    help="re-try 'nothing found' tracks after N days (default 14)")
    ap.add_argument("--ignore-cache", action="store_true", help="ignore the not-found cache")
    ap.add_argument("--limit", type=int, default=0, help="only process first N tracks (testing)")
    ap.add_argument("--debug", action="store_true", help="show provider logs (use with --limit 5)")
    args = ap.parse_args()

    if args.fast:
        args.workers, args.interval = 4, 0.25
    if args.debug:
        logging.basicConfig(level=logging.DEBUG, format="%(name)s: %(message)s")

    todo, ok_items = scan_library(args.dir)
    n_missing = sum(1 for t in todo if t[2] == "missing")
    n_fix = sum(1 for t in todo if t[2] == "fix")

    suspicious = {}                                   # audio -> reason
    if not args.missing_only and ok_items:
        print(f"\nAuditing {len(ok_items)} synced lyrics files ...")
        try:
            flagged = run_audit(ok_items, workers=8)
        except KeyboardInterrupt:
            print("\nInterrupted during audit.")
            return
        for audio, lrc, reason in flagged:
            suspicious[audio] = reason
            todo.append((audio, lrc, "suspicious"))

    print(f"\nValid synced lyrics       : {len(ok_items) - len(suspicious)}")
    print(f"No lyrics file            : {n_missing}")
    print(f"Plain / broken lyrics     : {n_fix}")
    if not args.missing_only:
        print(f"Suspicious (wrong song?)  : {len(suspicious)}")

    cache = {} if args.ignore_cache else load_cache()
    cutoff = time.time() - args.retry_days * 86400
    skipped_cache = 0
    if cache:
        filtered = []
        for item in todo:
            ts = cache.get(item[0])
            if ts and ts > cutoff:
                skipped_cache += 1
            else:
                filtered.append(item)
        todo = filtered
    if args.limit:
        todo = todo[:args.limit]

    total = len(todo)
    print(f"Skipped (checked recently): {skipped_cache}")
    print(f"To process                : {total}\n")

    counts = {"synced": 0, "plain": 0, "kept": 0, "none": 0, "error": 0}
    results = {}
    not_found = []
    first_error = None
    fetcher = None
    start = time.time()

    if total:
        limiter = Limiter(args.interval)
        fetcher = Fetcher(limiter)
        bar = tqdm(
            total=total, unit="song", smoothing=0.05, dynamic_ncols=True,
            bar_format="{l_bar}{bar}| {n_fmt}/{total_fmt} [{elapsed}<{remaining}, {rate_fmt}] {postfix}",
        )
        executor = ThreadPoolExecutor(max_workers=max(1, args.workers))
        futures = {executor.submit(fetcher.process, it): it for it in todo}

        try:
            for n, fut in enumerate(as_completed(futures), start=1):
                audio = futures[fut][0]
                try:
                    _, kind, query = fut.result()
                except Exception as e:
                    kind, query = "error", os.path.basename(audio)
                    if first_error is None:
                        first_error = str(e)
                        bar.write(f"[!] First error: {first_error}\n"
                                  f"    (test with: python lyrics_sync.py --limit 5 --debug)")

                counts[kind] += 1
                results[audio] = kind
                if kind in ("none", "kept"):
                    cache[audio] = time.time()
                    if kind == "none":
                        not_found.append(query)
                else:
                    cache.pop(audio, None)

                bar.set_postfix_str(
                    f"synced:{counts['synced']} plain:{counts['plain']} kept:{counts['kept']} "
                    f"miss:{counts['none']} err:{counts['error']} | {os.path.basename(audio)[:30]}",
                    refresh=False,
                )
                bar.update(1)
                if n % 200 == 0:
                    save_cache(cache)
        except KeyboardInterrupt:
            bar.write("\nInterrupted - saving progress...")
        finally:
            bar.close()
            executor.shutdown(wait=False, cancel_futures=True)
            save_cache(cache)

    elapsed = time.time() - start
    done = sum(counts.values())
    if not_found:
        NOT_FOUND_LOG.write_text("\n".join(sorted(set(not_found))), encoding="utf-8")
    if fetcher and fetcher.rejected:
        REJECTED_LOG.write_text("\n".join(
            f"{os.path.basename(a)} | {src} | {why}" for a, src, why in sorted(fetcher.rejected)),
            encoding="utf-8")
    if suspicious:
        lines = []
        for audio, reason in sorted(suspicious.items()):
            state = "FIXED" if results.get(audio) == "synced" else "STILL SUSPICIOUS"
            lines.append(f"[{state}] {audio}\n    {reason}")
        SUSPICIOUS_LOG.write_text("\n".join(lines), encoding="utf-8")

    print("\n" + "=" * 54)
    if total:
        rate = f" ({done / elapsed:.2f} songs/s)" if elapsed > 0 else ""
        print(f"Processed : {done}/{total} in {fmt_time(elapsed)}{rate}")
    print(f"Synced    : {counts['synced']}")
    print(f"Plain     : {counts['plain']}")
    print(f"Kept      : {counts['kept']}  (existing lyrics kept - no synced/better version found)")
    print(f"Not found : {counts['none']}" + (f"  (list: {NOT_FOUND_LOG})" if not_found else ""))
    print(f"Errors    : {counts['error']}" + (f"  (first: {first_error})" if first_error else ""))
    if fetcher and fetcher.stats:
        print("\nPlatform results     found  nothing  errors")
        for name, (hit, miss, err) in sorted(fetcher.stats.items()):
            print(f"  {name:<16} {hit:>7} {miss:>8} {err:>7}"
                  + (f"   last error: {fetcher.last_errors.get(name)}" if err else ""))
        if fetcher.rejected:
            print(f"Discarded as wrong-looking: {len(fetcher.rejected)}  (list: {REJECTED_LOG})")
        print()
    if suspicious:
        fixed = sum(1 for a in suspicious if results.get(a) == "synced")
        print(f"Suspicious: {len(suspicious)} found, {fixed} fixed  (report: {SUSPICIOUS_LOG})")
    if done < total:
        print("Run again to resume - finished tracks are skipped automatically.")


if __name__ == "__main__":
    sys.exit(main())
