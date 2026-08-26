//! Cover art: fetched from the URL the engine reports and served to the
//! frontend as `cover://<sha1hex-of-url>` via a custom URI scheme.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use sha1::{Digest, Sha1};

use crate::app::data_dir;

/// How long a cover download may take before it fails.
const COVER_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(COVER_FETCH_TIMEOUT)
            .build()
            .expect("reqwest client builder cannot fail with default settings")
    })
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
    if let Ok(cached) = std::fs::read(&path) {
        if is_image(&cached) {
            return Ok(cover_url_for(url));
        }
        let _ = std::fs::remove_file(&path);
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

/// Reads a cached cover and answers the URI scheme request. `hex` is the
/// path/authority the frontend requested; the bytes are keyed by that sha1.
pub fn serve_cover(hex: &str) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    let path = data_dir().join("covers").join(hex);
    match std::fs::read(&path) {
        Ok(bytes) if is_image(&bytes) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", sniff_content_type(&bytes))
            // Lets the frontend read cover pixels back off a canvas, which is
            // how a detail header derives its wash from the actual artwork
            // rather than from a hash of the id. Without it the image taints
            // the canvas and `getImageData` throws. This scheme only ever
            // serves files out of our own cover cache to our own webview.
            .header("Access-Control-Allow-Origin", "*")
            .body(std::borrow::Cow::Owned(bytes))
            .expect("cover response is valid"),
        // A zero-length or non-image file is a corrupt entry (a crashed
        // write, a stored error page): evict it so the next `get_cover`
        // round refetches instead of answering with garbage forever.
        Ok(_) => {
            let _ = std::fs::remove_file(&path);
            cover_not_found()
        }
        Err(_) => cover_not_found(),
    }
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
            "spotify-renderer-cover-write-{}-{}",
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
}
