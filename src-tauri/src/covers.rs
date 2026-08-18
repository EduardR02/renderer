//! Cover art: fetched from the URL the engine reports and served to the
//! frontend as `cover://<sha1hex-of-url>` via a custom URI scheme.

use std::path::{Path, PathBuf};
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

/// Returns the `cover://` URL for `url`, downloading and caching the image
/// bytes on first use.
pub async fn get_cover(url: &str) -> Result<String, String> {
    let dir = data_dir();
    let path = cover_path(&dir, url);
    if path.is_file() {
        return Ok(cover_url_for(url));
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
    if bytes.is_empty() {
        return Err(format!("cover fetch returned an empty body for {url}"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create covers dir: {error}"))?;
    }
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("could not cache cover {url}: {error}"))?;
    Ok(cover_url_for(url))
}

/// Reads a cached cover and answers the URI scheme request. `hex` is the
/// path/authority the frontend requested; the bytes are keyed by that sha1.
pub fn serve_cover(hex: &str) -> tauri::http::Response<std::borrow::Cow<'static, [u8]>> {
    let path = data_dir().join("covers").join(hex);
    match std::fs::read(&path) {
        Ok(bytes) => tauri::http::Response::builder()
            .status(200)
            .header("Content-Type", sniff_content_type(&bytes))
            .body(std::borrow::Cow::Owned(bytes))
            .expect("cover response is valid"),
        Err(_) => tauri::http::Response::builder()
            .status(404)
            .body(std::borrow::Cow::Borrowed(&b"cover not found"[..]))
            .expect("404 response is valid"),
    }
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

    #[test]
    fn content_type_sniffing_recognizes_common_formats() {
        assert_eq!(sniff_content_type(&[0x89, b'P', b'N', b'G', 0x0D]), "image/png");
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
}
