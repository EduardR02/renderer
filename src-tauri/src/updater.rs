//! Release metadata establishes whether a signed updater manifest is needed.
//! Never turn an unavailable or malformed newer release into “up to date”.

use std::sync::LazyLock;
use std::time::Duration;

use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{Manager, ResourceId, Webview};
use tauri_plugin_updater::UpdaterExt;
use url::Url;

const RELEASE_URL: &str = "https://api.github.com/repos/EduardR02/renderer/releases/latest";
const DOWNLOAD_ROOT: &str = "https://github.com/EduardR02/renderer/releases/download/";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    published_at: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

struct NewRelease {
    version: Version,
    manifest: Url,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadata {
    rid: ResourceId,
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
    raw_json: serde_json::Value,
}

fn release_client() -> Result<&'static Client, String> {
    static CLIENT: LazyLock<Result<Client, String>> = LazyLock::new(|| {
        Client::builder()
            .user_agent(concat!("renderer/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| "Could not create the update HTTP client".to_owned())
    });
    CLIENT.as_ref().map_err(Clone::clone)
}

fn newer_release(installed: &Version, release: Release) -> Result<Option<NewRelease>, String> {
    let invalid = || "The latest release contains invalid version metadata".to_owned();
    let version = Version::parse(release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name))
        .map_err(|_| invalid())?;
    if release.draft || release.prerelease || !version.pre.is_empty() || release.published_at.is_empty() {
        return Err(invalid());
    }
    // Build metadata is not release precedence (0.1.19+local == 0.1.19).
    if !version.cmp_precedence(installed).is_gt() {
        return Ok(None);
    }
    if !release.assets.iter().any(|asset| asset.name == "latest.json") {
        return Err(format!("Release {version} is newer, but its signed updater manifest has not been published"));
    }
    let mut manifest = Url::parse(DOWNLOAD_ROOT).expect("static release download URL");
    manifest.path_segments_mut().expect("release URL has a path")
        .pop_if_empty().push(&release.tag_name).push("latest.json");
    Ok(Some(NewRelease { version, manifest }))
}

async fn fetch_release(http: &Client, endpoint: &str, installed: &Version) -> Result<Option<NewRelease>, String> {
    let response = http.get(endpoint)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send().await.map_err(|_| "Could not reach the release service; check your connection and try again".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("Could not check releases (HTTP {}); try again later", response.status().as_u16()));
    }
    let release = response.json().await
        .map_err(|_| "The release service returned invalid release metadata".to_owned())?;
    newer_release(installed, release)
}

fn validate_manifest(expected: &Version, version: &str, signature: &str, download: &Url) -> Result<(), String> {
    let actual = Version::parse(version).map_err(|_| "The updater manifest contains an invalid version".to_owned())?;
    if actual.cmp_precedence(expected).is_ne() {
        return Err(format!("The updater manifest version does not match release {expected}"));
    }
    if signature.trim().is_empty() || download.scheme() != "https" {
        return Err("The release updater manifest is missing a signature or secure download URL".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub async fn check_update(webview: Webview) -> Result<Option<UpdateMetadata>, String> {
    let installed = &webview.package_info().version;
    let Some(release) = fetch_release(release_client()?, RELEASE_URL, installed).await? else {
        return Ok(None);
    };
    let updater = webview.updater_builder()
        .endpoints(vec![release.manifest]).map_err(|_| "The release updater URL is invalid".to_owned())?
        .timeout(TIMEOUT)
        // Metadata already proved a newer release. Even a stale/older manifest
        // must be returned for validation, not interpreted as “no update”.
        .version_comparator(|_, _| true)
        .build().map_err(|error| format!("Could not initialize the signed updater: {error}"))?;
    let update = updater.check().await
        .map_err(|error| format!("Could not load the signed updater manifest for release {}: {error}", release.version))?
        .ok_or_else(|| format!("Release {} is newer, but its signed updater manifest is empty", release.version))?;
    validate_manifest(&release.version, &update.version, &update.signature, &update.download_url)?;
    let metadata = UpdateMetadata {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date: update.raw_json.get("pub_date").and_then(serde_json::Value::as_str).map(str::to_owned),
        body: update.body.clone(),
        raw_json: update.raw_json.clone(),
        // Keep the actual plugin resource: download/install still verify its
        // minisign signature against the configured public key.
        rid: webview.resources_table().add(update),
    };
    Ok(Some(metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn release(tag: &str, manifest: bool) -> Release {
        Release {
            tag_name: tag.into(), draft: false, prerelease: false,
            published_at: "2026-09-30T00:00:00Z".into(),
            assets: if manifest { vec![Asset { name: "latest.json".into() }] } else { vec![] },
        }
    }

    #[test]
    fn same_and_older_releases_need_no_manifest() {
        let installed = Version::parse("0.1.19+local").unwrap();
        for tag in ["v0.1.18", "v0.1.19", "0.1.19+release"] {
            assert!(newer_release(&installed, release(tag, false)).unwrap().is_none());
        }
    }

    #[test]
    fn newer_release_requires_a_published_manifest_and_pins_its_tag() {
        let installed = Version::parse("0.1.19").unwrap();
        assert!(newer_release(&installed, release("v0.1.20", false)).is_err());
        let newer = newer_release(&installed, release("v0.1.20", true)).unwrap().unwrap();
        assert_eq!(newer.manifest.as_str(), "https://github.com/EduardR02/renderer/releases/download/v0.1.20/latest.json");
    }

    #[test]
    fn invalid_or_unpublished_release_cannot_prove_up_to_date() {
        let installed = Version::parse("0.1.19").unwrap();
        for tag in ["not-a-version", "v0.1.18-beta.1", "0.1"] {
            assert!(newer_release(&installed, release(tag, false)).is_err());
        }
        let mut draft = release("v0.1.18", false);
        draft.draft = true;
        assert!(newer_release(&installed, draft).is_err());
        let mut prerelease = release("v0.1.18", false);
        prerelease.prerelease = true;
        assert!(newer_release(&installed, prerelease).is_err());
        let mut unpublished = release("v0.1.18", false);
        unpublished.published_at.clear();
        assert!(newer_release(&installed, unpublished).is_err());
    }

    #[test]
    fn newer_manifest_must_match_release_and_keep_signature_and_https() {
        let expected = Version::parse("0.1.20").unwrap();
        let https = Url::parse("https://github.com/release/installer.exe").unwrap();
        assert!(validate_manifest(&expected, "0.1.20", "signed", &https).is_ok());
        assert!(validate_manifest(&expected, "0.1.19", "signed", &https).is_err());
        assert!(validate_manifest(&expected, "0.1.21", "signed", &https).is_err());
        assert!(validate_manifest(&expected, "broken", "signed", &https).is_err());
        assert!(validate_manifest(&expected, "0.1.20", " ", &https).is_err());
        assert!(validate_manifest(&expected, "0.1.20", "signed", &Url::parse("http://example.org/installer.exe").unwrap()).is_err());
    }

    async fn http_release(status: &str, body: &str) -> Result<Option<NewRelease>, String> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            stream.read(&mut request).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        let result = fetch_release(&client, &endpoint, &Version::parse("0.1.19").unwrap()).await;
        server.await.unwrap();
        result
    }

    #[tokio::test]
    async fn real_http_failures_and_corrupt_metadata_are_not_no_update() {
        assert!(http_release("404 Not Found", r#"{"message":"Not Found"}"#).await.is_err());
        assert!(http_release("503 Service Unavailable", "unavailable").await.is_err());
        assert!(http_release("200 OK", "<html>not JSON</html>").await.is_err());
        assert!(http_release("200 OK", r#"{"tag_name":"v0.1.18"}"#).await.is_err());
        assert!(http_release("204 No Content", "").await.is_err());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let http = Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        assert!(fetch_release(&http, &endpoint, &Version::parse("0.1.19").unwrap()).await.is_err());
    }

    #[tokio::test]
    async fn real_http_older_release_without_manifest_proves_up_to_date() {
        let body = r#"{"tag_name":"v0.1.18","draft":false,"prerelease":false,"published_at":"2026-09-25T22:31:40Z","assets":[]}"#;
        assert!(http_release("200 OK", body).await.unwrap().is_none());
    }
}
