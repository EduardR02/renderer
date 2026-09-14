//! Cover art: fetched from the URL the engine reports and served to the
//! frontend as `cover://<sha1hex-of-url>` via a custom URI scheme.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, UNIX_EPOCH};

use parking_lot::Mutex;
use sha1::{Digest, Sha1};

use crate::app::data_dir;

/// How long a cover download may take before it fails.
const COVER_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

fn http_client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(COVER_FETCH_TIMEOUT)
            .build()
            .expect("reqwest client builder cannot fail with default settings")
    });
    &CLIENT
}

fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The `cover://<sha1hex>` URL for a cover URL.
pub fn cover_url_for(url: &str) -> String {
    format!("cover://{}", sha1_hex(url.as_bytes()))
}

fn cover_path(data_dir: &Path, url: &str) -> PathBuf {
    data_dir.join("covers").join(sha1_hex(url.as_bytes()))
}

/// True when the magic bytes identify a format the cache serves as an image.
/// Everything else — an empty body, a truncated file, an HTML error page — is
/// treated as a miss.
fn is_image(bytes: &[u8]) -> bool {
    sniff_content_type(bytes) != "application/octet-stream"
}

/// How much of a cached file the image test reads. Twelve bytes decide every
/// format above; a round 32 costs nothing and leaves room for a longer
/// signature later.
const MAGIC_BYTES: usize = 32;

/// Whatever [`is_image`] can say about the file at `path` from its first
/// bytes.
///
/// A cache hit is a yes/no question about a file that may be hundreds of
/// kilobytes, so it must not read the whole picture to answer it. `None` is
/// "could not be opened at all", deliberately distinct from `Some(false)`:
/// only a file we could read and found wanting is a corrupt entry worth
/// evicting.
fn cached_image_header(path: &Path) -> Option<bool> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; MAGIC_BYTES];
    let mut filled = 0;
    // A short read is legal for a file, so fill the buffer rather than trusting
    // one call and mistaking a six-byte GIF for a non-image.
    while filled < MAGIC_BYTES {
        match file.read(&mut magic[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return None,
        }
    }
    Some(is_image(&magic[..filled]))
}

/// Writes `bytes` to `path` through a unique temp sibling plus the same
/// platform-correct atomic replacement the JSON caches use (plain `rename`
/// cannot replace an existing file on Windows), so a crash mid-write can
/// never leave a truncated image behind as a cache hit.
fn write_cover_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let temp =
        path.with_extension(format!("tmp{}", NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&temp, bytes)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    crate::app::replace_file_atomically(&temp, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

/// Returns the `cover://` URL for `url`, downloading and caching the image
/// bytes on first use. A cached entry only counts as a hit when it is a real
/// image; corrupt leftovers are evicted and refetched.
pub async fn get_cover(url: &str) -> Result<String, String> {
    let dir = data_dir();
    let path = cover_path(&dir, url);
    match cached_image_header(&path) {
        Some(true) => return Ok(cover_url_for(url)),
        Some(false) => {
            let _ = std::fs::remove_file(&path);
            forget_served(&path);
        }
        None => {}
    }
    let response = http_client()
        .get(url)
        .send()
        .await
        .map_err(|error| format!("could not fetch cover {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "cover fetch failed: {} for {url}",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("could not read cover {url}: {error}"))?;
    if !is_image(&bytes) {
        return Err(format!("cover fetch returned a non-image body for {url}"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create covers dir: {error}"))?;
    }
    write_cover_atomically(&path, &bytes)?;
    Ok(cover_url_for(url))
}

/* --- The bytes last served --------------------------------------------- */

/// What a client is told about a cover's freshness. A `cover://<sha1hex>` name
/// IS the sha1 of the url the bytes were fetched from, and Spotify addresses
/// artwork by content, so the bytes behind one name never change. Without this
/// header the webview had no freshness for these responses at all and
/// re-entered this handler for every recycled row — a whole-file read on its
/// UI thread each time.
const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// How many recently served covers stay in memory, and the byte budget that
/// trims the set first: a screenful or two of artwork, not the cache.
const SERVED_ENTRIES: usize = 64;
const SERVED_BYTES: usize = 8 * 1024 * 1024;

/// A file's identity: how long it is and when it was written. One fetch writes
/// one file, so these two numbers change whenever its bytes do — which is what
/// makes them usable both as the `ETag` and as the test that an in-memory copy
/// still belongs to the file it came from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: u128,
}

impl FileIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata
                .modified()
                .ok()
                .and_then(|written| written.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_nanos())
                .unwrap_or_default(),
        }
    }

    fn etag(&self) -> String {
        format!("\"{:x}-{:x}\"", self.len, self.modified)
    }
}

struct Served {
    identity: FileIdentity,
    bytes: Arc<[u8]>,
}

/// The covers this process has served, oldest evicted first.
///
/// The webview's own cache answers the steady state (see [`CACHE_CONTROL`]);
/// this is what covers the first ask of a session, the moment after a cache
/// clear, and any window where the webview's cache is not doing its job. An
/// entry is only handed out while the file it came from still has the same
/// identity, so bytes whose file was replaced — refetched after a corrupt
/// eviction, or wiped by Settings — can never be served from here.
#[derive(Default)]
struct ServedCovers {
    entries: HashMap<String, Served>,
    /// Insertion order, which is the order covers were drawn in.
    order: VecDeque<String>,
    bytes: usize,
}

impl ServedCovers {
    fn get(&mut self, hex: &str, identity: FileIdentity) -> Option<Arc<[u8]>> {
        let entry = self.entries.get(hex)?;
        (entry.identity == identity).then(|| entry.bytes.clone())
    }

    fn insert(&mut self, hex: String, identity: FileIdentity, bytes: Arc<[u8]>) {
        if bytes.len() > SERVED_BYTES {
            return;
        }
        match self.entries.insert(
            hex.clone(),
            Served {
                identity,
                bytes: bytes.clone(),
            },
        ) {
            Some(previous) => self.bytes -= previous.bytes.len(),
            None => self.order.push_back(hex),
        }
        self.bytes += bytes.len();
        while self.bytes > SERVED_BYTES || self.order.len() > SERVED_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes.len();
            }
        }
    }

    fn remove(&mut self, hex: &str) {
        if let Some(entry) = self.entries.remove(hex) {
            self.bytes -= entry.bytes.len();
            self.order.retain(|key| key != hex);
        }
    }
}

fn served_covers() -> &'static Mutex<ServedCovers> {
    static SERVED: LazyLock<Mutex<ServedCovers>> =
        LazyLock::new(|| Mutex::new(ServedCovers::default()));
    &SERVED
}

/// Drops the in-memory copy of a cover file that has just been deleted, so
/// nothing here can outlive the file it describes.
fn forget_served(path: &Path) {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        served_covers().lock().remove(name);
    }
}

/// Reads a cached cover and answers the URI scheme request. `hex` is the
/// path/authority the frontend requested; the bytes are keyed by that sha1,
/// and `if_none_match` is the client's `If-None-Match`, if it sent one.
pub fn serve_cover(
    hex: &str,
    if_none_match: Option<&str>,
) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    cover_response(hex, &data_dir().join("covers").join(hex), if_none_match)
}

/// The response for one cached cover file. `hex` is the name it is cached
/// under, which is also the key the served-bytes cache holds it by; `path` is
/// split out of [`serve_cover`] so the response a client sees — headers,
/// revalidation and the corrupt-entry eviction — can be exercised on a file of
/// the test's own.
fn cover_response(
    hex: &str,
    path: &Path,
    if_none_match: Option<&str>,
) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return cover_not_found();
    };
    let identity = FileIdentity::of(&metadata);
    /* Scoped to its own statement on purpose: a guard taken in the `match`
       scrutinee would live until the end of the match, and the miss arm below
       takes the same mutex — on one thread, that is a deadlock. */
    let served = served_covers().lock().get(hex, identity);
    let bytes = match served {
        Some(bytes) => bytes,
        None => match std::fs::read(path) {
            Ok(bytes) if is_image(&bytes) => {
                let bytes: Arc<[u8]> = bytes.into();
                served_covers()
                    .lock()
                    .insert(hex.to_owned(), identity, bytes.clone());
                bytes
            }
            // A zero-length or non-image file is a corrupt entry (a crashed
            // write, a stored error page): evict it so the next `get_cover`
            // round refetches instead of answering with garbage forever.
            Ok(_) => {
                let _ = std::fs::remove_file(path);
                forget_served(path);
                return cover_not_found();
            }
            Err(_) => return cover_not_found(),
        },
    };

    let etag = identity.etag();
    /* The client holds exactly these bytes already: answer with the tag alone,
       which is all a revalidation needs. Weak tags compare equal to strong
       ones under the If-None-Match rules, hence the prefix strip. */
    let revalidated = if_none_match.is_some_and(|value| {
        value.split(',').any(|tag| {
            let tag = tag.trim();
            tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag
        })
    });
    if revalidated {
        return tauri::http::Response::builder()
            .status(304)
            .header("Cache-Control", CACHE_CONTROL)
            .header("ETag", etag)
            .header("Access-Control-Allow-Origin", "*")
            .body(std::borrow::Cow::Borrowed(&[][..]))
            .expect("revalidation response is valid");
    }

    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", sniff_content_type(&bytes))
        // Lets the frontend read cover pixels back off a canvas, which is
        // how a detail header derives its wash from the actual artwork
        // rather than from a hash of the id. Without it the image taints
        // the canvas and `getImageData` throws. This scheme only ever
        // serves files out of our own cover cache to our own webview.
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", CACHE_CONTROL)
        .header("ETag", etag)
        .body(std::borrow::Cow::Owned(bytes.to_vec()))
        .expect("cover response is valid")
}

fn cover_not_found() -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    tauri::http::Response::builder()
        .status(404)
        .body(std::borrow::Cow::Borrowed(&b"cover not found"[..]))
        .expect("404 response is valid")
}

/// Sniffs the image format from its magic bytes (png/jpg/webp/gif), falling
/// back to octet-stream so unknown payloads still render where possible.
fn sniff_content_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.starts_with(b"GIF8") {
        return "image/gif";
    }
    "application/octet-stream"
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Unix seconds, unique enough per test run to avoid temp-dir collisions.
    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default()
    }

    #[test]
    fn content_type_sniffing_recognizes_common_formats() {
        assert_eq!(
            sniff_content_type(&[0x89, b'P', b'N', b'G', 0x0D]),
            "image/png"
        );
        assert_eq!(sniff_content_type(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(
            sniff_content_type(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            "image/webp"
        );
        assert_eq!(sniff_content_type(b"GIF89a"), "image/gif");
        assert_eq!(sniff_content_type(b"nope"), "application/octet-stream");
    }

    #[test]
    fn cover_urls_are_deterministic_sha1_hex() {
        let first = cover_url_for("https://i.scdn.co/image/abc");
        let second = cover_url_for("https://i.scdn.co/image/abc");
        assert_eq!(first, second);
        assert!(first.starts_with("cover://"));
        assert_eq!(first.len(), "cover://".len() + 40);
        assert_ne!(first, cover_url_for("https://i.scdn.co/image/abd"));
    }

    #[test]
    fn zero_length_and_non_image_payloads_are_not_image_hits() {
        assert!(!is_image(&[]));
        assert!(!is_image(b"<html><body>503</body></html>"));
        assert!(is_image(b"GIF89a"));
        assert!(is_image(&[0x89, b'P', b'N', b'G', 0x0D]));
    }

    #[test]
    fn cover_writes_replace_atomically_without_leaving_temp_files() {
        let dir = std::env::temp_dir().join(format!(
            "renderer-cover-write-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("deadbeef");

        write_cover_atomically(&path, b"GIF89a").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"GIF89a");

        // Replacing an existing entry must work (a plain Windows rename
        // would silently fail) and leave exactly the one image behind.
        write_cover_atomically(&path, &[0x89, b'P', b'N', b'G']).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), &[0x89, b'P', b'N', b'G']);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp sibling survives");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory of this test's own, so parallel tests cannot meet.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "renderer-cover-{name}-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A response for a file that is not in the cache at all.
    #[test]
    fn a_missing_or_corrupt_cached_file_answers_not_found() {
        let dir = scratch_dir("serve-missing");
        let missing = dir.join("no-such-name");
        assert_eq!(cover_response("no-such-name", &missing, None).status(), 404);

        // A stored error page is a corrupt entry: evicted, so the next
        // `get_cover` round replaces it instead of serving it forever.
        let corrupt = dir.join("corrupt-name");
        std::fs::write(&corrupt, b"<html>503</html>").unwrap();
        assert_eq!(cover_response("corrupt-name", &corrupt, None).status(), 404);
        assert!(!corrupt.exists(), "the corrupt entry is evicted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a client is told about a cover it has already fetched: the bytes,
    /// with a tag and a freshness long enough that it never has to ask again.
    #[test]
    fn a_served_cover_is_cacheable_and_revalidates_by_its_tag() {
        let dir = scratch_dir("serve-headers");
        let path = dir.join("header-test-name");
        let bytes = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(&path, bytes).unwrap();

        let first = cover_response("header-test-name", &path, None);
        assert_eq!(first.status(), 200);
        let headers = first.headers();
        assert_eq!(headers["Content-Type"].to_str().unwrap(), "image/png");
        assert_eq!(headers["Access-Control-Allow-Origin"].to_str().unwrap(), "*");
        assert_eq!(headers["Cache-Control"].to_str().unwrap(), CACHE_CONTROL);
        assert_eq!(first.body().as_ref(), &bytes);

        let etag = headers["ETag"].to_str().unwrap().to_owned();
        assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");

        // Asking about the tag it holds costs a status line, not the picture.
        let revalidated = cover_response("header-test-name", &path, Some(&etag));
        assert_eq!(revalidated.status(), 304);
        assert!(revalidated.body().is_empty());
        assert_eq!(revalidated.headers()["ETag"].to_str().unwrap(), etag);

        // A tag that is not this file's gets the bytes — a client holding some
        // other picture must be given this one — while `*` matches whatever
        // representation exists, which is what the client asked about.
        assert_eq!(
            cover_response("header-test-name", &path, Some("\"0-0\"")).status(),
            200
        );
        assert_eq!(
            cover_response("header-test-name", &path, Some("*")).status(),
            304
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cache_hit_is_judged_from_the_file_header_alone() {
        let dir = scratch_dir("header");

        // Six bytes on purpose: a real image, and a reader that demanded a
        // full buffer would call it corrupt and refetch it forever.
        let short = dir.join("gif");
        std::fs::write(&short, b"GIF89a").unwrap();
        assert_eq!(cached_image_header(&short), Some(true));

        let page = dir.join("html");
        std::fs::write(&page, "<html>".repeat(64)).unwrap();
        assert_eq!(cached_image_header(&page), Some(false));

        let empty = dir.join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(cached_image_header(&empty), Some(false));

        // A file that is not there is a miss, never an eviction.
        assert_eq!(cached_image_header(&dir.join("missing")), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn served_bytes_are_handed_out_only_for_the_file_they_came_from() {
        let mut served = ServedCovers::default();
        let identity = FileIdentity {
            len: 3,
            modified: 7,
        };
        served.insert("abc".to_owned(), identity, Arc::from(&b"GIF"[..]));
        assert_eq!(served.get("abc", identity).as_deref(), Some(&b"GIF"[..]));

        // Rewritten longer, and rewritten at another time: neither is the file
        // the bytes were read from, so neither may be answered from memory.
        assert!(served
            .get(
                "abc",
                FileIdentity {
                    len: 4,
                    modified: 7
                }
            )
            .is_none());
        assert!(served
            .get(
                "abc",
                FileIdentity {
                    len: 3,
                    modified: 8
                }
            )
            .is_none());
        assert!(served.get("def", identity).is_none());

        // Only forget takes it out.
        served.remove("abc");
        assert!(served.get("abc", identity).is_none());
        assert_eq!(served.bytes, 0);
    }

    #[test]
    fn the_served_cache_stays_inside_its_bounds() {
        let identity = FileIdentity {
            len: 1,
            modified: 0,
        };

        let mut served = ServedCovers::default();
        for index in 0..SERVED_ENTRIES + 8 {
            served.insert(
                format!("{index:040}"),
                identity,
                Arc::from(&b"x"[..]),
            );
        }
        assert_eq!(served.entries.len(), SERVED_ENTRIES);
        assert_eq!(served.order.len(), SERVED_ENTRIES, "the order queue leaks keys");
        assert_eq!(served.bytes, SERVED_ENTRIES);
        assert!(served.get(&format!("{:040}", 0), identity).is_none());
        assert!(served
            .get(&format!("{:040}", SERVED_ENTRIES + 7), identity)
            .is_some());

        // Three quarters of the budget each: two fit, the third pushes the
        // first out.
        let four_mb = SERVED_BYTES / 2;
        let mut served = ServedCovers::default();
        for index in 0..3 {
            served.insert(
                format!("{index:040}"),
                identity,
                vec![0u8; four_mb].into(),
            );
        }
        assert_eq!(served.entries.len(), 2);
        assert_eq!(served.bytes, SERVED_BYTES);
        assert!(served.get(&format!("{:040}", 0), identity).is_none());

        // An image bigger than the whole budget is served, never held.
        let mut served = ServedCovers::default();
        served.insert(
            "big".to_owned(),
            identity,
            vec![0u8; SERVED_BYTES + 1].into(),
        );
        assert_eq!(served.bytes, 0);
        assert!(served.get("big", identity).is_none());
    }

    #[test]
    fn deleting_a_cover_file_forgets_its_served_bytes() {
        let name = "a-name-only-this-test-uses";
        let identity = FileIdentity {
            len: 3,
            modified: 7,
        };
        served_covers()
            .lock()
            .insert(name.to_owned(), identity, Arc::from(&b"GIF"[..]));

        forget_served(&Path::new("covers").join(name));

        assert!(served_covers().lock().get(name, identity).is_none());
    }

    #[test]
    fn a_changed_file_gets_a_changed_tag() {
        let file = FileIdentity {
            len: 4096,
            modified: 1_700_000_000_000_000_000,
        };
        // Either number moving is a different file, and a tag that ignored one
        // of them would let a client keep bytes that are no longer there.
        assert_ne!(
            file.etag(),
            FileIdentity {
                len: 4097,
                ..file
            }
            .etag()
        );
        assert_ne!(
            file.etag(),
            FileIdentity {
                modified: file.modified + 1,
                ..file
            }
            .etag()
        );
        assert_eq!(file.etag(), file.etag());
    }
}
