use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{Client, Response, StatusCode};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use url::Url;

const OFFICIAL_REPOSITORY_URL: &str = "https://github.com/adhamblabel31-boop/Adham-AI";
const API_BASE_URL: &str = "https://api.github.com";
const MAX_RELEASES: usize = 100;
const MAX_RELEASES_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_CHECKSUMS_BYTES: u64 = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UpdateError {
    NotConfigured,
    InvalidRepository,
    InvalidPublicKey,
    InvalidVersion(String),
    Request(String),
    RateLimited(Option<u64>),
    HttpStatus(u16),
    NoStableRelease,
    MissingAsset(String),
    UnexpectedAsset(String),
    UnsupportedArchitecture,
    InvalidAssetUrl,
    DownloadTooLarge,
    Download(String),
    InvalidSignature,
    InvalidChecksumManifest,
    ChecksumMismatch,
    InvalidDeb,
    InstallerUnavailable(String),
    InstallationCancelled,
    AuthorizationFailed,
    PackageManagerLocked,
    MissingDependencies(String),
    InstallationFailed(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => formatter.write_str(
                "Secure update verification is unavailable until a trusted Ed25519 public key is configured for this build.",
            ),
            Self::InvalidRepository => formatter.write_str("The update repository URL is invalid."),
            Self::InvalidPublicKey => formatter.write_str("The embedded update verification key is invalid."),
            Self::InvalidVersion(version) => {
                write!(formatter, "The release version {version:?} is invalid.")
            }
            Self::Request(detail) => write!(formatter, "Could not check for updates: {detail}"),
            Self::RateLimited(reset) => {
                if let Some(reset) = reset {
                    write!(formatter, "GitHub API rate limit reached. Try again after Unix time {reset}.")
                } else {
                    formatter.write_str("GitHub API rate limit reached. Try again later.")
                }
            }
            Self::HttpStatus(status) => {
                write!(formatter, "GitHub returned HTTP status {status}.")
            }
            Self::NoStableRelease => formatter.write_str("No stable release is available."),
            Self::MissingAsset(name) => write!(formatter, "The release is missing required asset {name}."),
            Self::UnexpectedAsset(name) => {
                write!(formatter, "The release contains an unexpected asset {name}.")
            }
            Self::UnsupportedArchitecture => {
                formatter.write_str("Software updates are currently available only for Linux AMD64.")
            }
            Self::InvalidAssetUrl => formatter.write_str("The release asset URL is not a trusted HTTPS GitHub URL."),
            Self::DownloadTooLarge => formatter.write_str("The release asset exceeds the maximum permitted download size."),
            Self::Download(detail) => write!(formatter, "Could not download the update: {detail}"),
            Self::InvalidSignature => formatter.write_str("The signed checksum manifest could not be verified."),
            Self::InvalidChecksumManifest => formatter.write_str("The signed checksum manifest is malformed or does not list the expected package."),
            Self::ChecksumMismatch => formatter.write_str("The downloaded Debian package does not match its signed SHA256 checksum."),
            Self::InvalidDeb => formatter.write_str("The downloaded file is not a valid Debian package archive."),
            Self::InstallerUnavailable(detail) => {
                write!(formatter, "The package installer is unavailable: {detail}")
            }
            Self::InstallationCancelled => formatter.write_str("Package installation was cancelled."),
            Self::AuthorizationFailed => {
                formatter.write_str("PolicyKit authorization failed or was denied.")
            }
            Self::PackageManagerLocked => formatter.write_str(
                "Another package operation is in progress. Close it and retry the installation.",
            ),
            Self::MissingDependencies(detail) => {
                write!(formatter, "APT could not resolve the package dependencies: {detail}")
            }
            Self::InstallationFailed(detail) => {
                write!(formatter, "Package installation failed: {detail}")
            }
        }
    }
}

impl std::error::Error for UpdateError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Repository {
    owner: String,
    name: String,
}

impl Repository {
    pub(super) fn parse(value: &str) -> Result<Self, UpdateError> {
        let url =
            Url::parse(value.trim_end_matches('/')).map_err(|_| UpdateError::InvalidRepository)?;
        let segments = url
            .path_segments()
            .map(|segments| segments.map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        if url.scheme() != "https"
            || url.host_str() != Some("github.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || segments.len() != 2
            || !valid_repository_segment(&segments[0])
            || !valid_repository_segment(&segments[1])
        {
            return Err(UpdateError::InvalidRepository);
        }

        Ok(Self {
            owner: segments[0].clone(),
            name: segments[1].clone(),
        })
    }

    pub(super) fn releases_url(&self) -> String {
        format!(
            "{API_BASE_URL}/repos/{}/{}/releases?per_page={MAX_RELEASES}",
            self.owner, self.name
        )
    }

    pub(super) fn web_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }

    pub(super) fn releases_page_url(&self) -> String {
        format!("{}/releases", self.web_url())
    }
}

pub(super) fn configured_repository() -> Result<Repository, UpdateError> {
    Repository::parse(OFFICIAL_REPOSITORY_URL)
}

pub(super) fn embedded_public_key() -> Result<VerifyingKey, UpdateError> {
    parse_public_key_config(option_env!("ADHAM_AI_UPDATE_PUBLIC_KEY"))
}

fn parse_public_key_config(encoded: Option<&str>) -> Result<VerifyingKey, UpdateError> {
    let encoded = encoded.ok_or(UpdateError::NotConfigured)?;
    let bytes = decode_hex_key(encoded).ok_or(UpdateError::InvalidPublicKey)?;
    let public_key = VerifyingKey::from_bytes(&bytes).map_err(|_| UpdateError::InvalidPublicKey)?;
    if public_key.is_weak() {
        return Err(UpdateError::InvalidPublicKey);
    }
    Ok(public_key)
}

fn decode_hex_key(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 {
        return None;
    }

    let mut bytes = [0; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn valid_repository_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Release {
    pub(super) version: Version,
    pub(super) tag: String,
    pub(super) notes: String,
    pub(super) published_at: Option<String>,
    pub(super) package: ReleaseAsset,
    pub(super) checksums: ReleaseAsset,
    pub(super) signature: ReleaseAsset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DownloadStage {
    Checksums,
    Signature,
    VerifyingSignature,
    Package,
    VerifyingPackage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DownloadProgress {
    pub(super) stage: DownloadStage,
    pub(super) downloaded: u64,
    pub(super) total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VerifiedPackage {
    path: PathBuf,
    version: Version,
    digest: [u8; 32],
}

impl VerifiedPackage {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn version(&self) -> &Version {
        &self.version
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleaseAsset {
    pub(super) name: String,
    pub(super) url: String,
    pub(super) size: u64,
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Debug, Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

pub(super) fn parse_version(tag: &str) -> Result<Version, UpdateError> {
    let value = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(value).map_err(|_| UpdateError::InvalidVersion(tag.to_owned()))
}

pub(super) fn newer_version(current: &Version, candidate: &Version) -> bool {
    candidate > current
}

fn select_latest_stable(releases: Vec<ApiRelease>) -> Result<ApiRelease, UpdateError> {
    releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = parse_version(&release.tag_name).ok()?;
            version.pre.is_empty().then_some(())?;
            Some((version, release))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, release)| release)
        .ok_or(UpdateError::NoStableRelease)
}

fn expected_package_name(version: &Version) -> String {
    format!("adham-ai_{version}_amd64.deb")
}

fn select_assets(release: ApiRelease) -> Result<Release, UpdateError> {
    select_assets_for_arch(release, std::env::consts::OS, std::env::consts::ARCH)
}

fn select_assets_for_arch(
    release: ApiRelease,
    operating_system: &str,
    architecture: &str,
) -> Result<Release, UpdateError> {
    let version = parse_version(&release.tag_name)?;
    if !version.pre.is_empty() {
        return Err(UpdateError::InvalidVersion(release.tag_name));
    }
    if operating_system != "linux" || architecture != "x86_64" {
        return Err(UpdateError::UnsupportedArchitecture);
    }

    let expected_package = expected_package_name(&version);
    for asset in &release.assets {
        if asset.name != expected_package
            && asset.name != "SHA256SUMS"
            && asset.name != "SHA256SUMS.sig"
        {
            return Err(UpdateError::UnexpectedAsset(asset.name.clone()));
        }
    }
    let package = select_asset(&release.assets, &expected_package)?;
    let checksums = select_asset(&release.assets, "SHA256SUMS")?;
    let signature = select_asset(&release.assets, "SHA256SUMS.sig")?;

    if package.size < 8 || checksums.size > MAX_CHECKSUMS_BYTES || signature.size != 64 {
        return Err(UpdateError::MissingAsset(
            "release assets have invalid sizes".to_owned(),
        ));
    }

    for asset in [&package, &checksums, &signature] {
        validate_github_asset_url(&asset.url)?;
    }

    Ok(Release {
        version,
        tag: release.tag_name,
        notes: release.body.or(release.name).unwrap_or_default(),
        published_at: release.published_at,
        package,
        checksums,
        signature,
    })
}

fn select_asset(assets: &[ApiAsset], expected_name: &str) -> Result<ReleaseAsset, UpdateError> {
    let mut matching = assets.iter().filter(|asset| asset.name == expected_name);
    let asset = matching
        .next()
        .ok_or_else(|| UpdateError::MissingAsset(expected_name.to_owned()))?;
    if matching.next().is_some() {
        return Err(UpdateError::MissingAsset(expected_name.to_owned()));
    }
    if asset.size > MAX_DOWNLOAD_BYTES {
        return Err(UpdateError::DownloadTooLarge);
    }
    Ok(ReleaseAsset {
        name: asset.name.clone(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
    })
}

fn validate_github_asset_url(value: &str) -> Result<Url, UpdateError> {
    validate_asset_url(value, false)
}

fn validate_asset_url(value: &str, allow_test_localhost: bool) -> Result<Url, UpdateError> {
    let url = Url::parse(value).map_err(|_| UpdateError::InvalidAssetUrl)?;
    let host = url.host_str().unwrap_or_default();
    let local_test_host = allow_test_localhost && matches!(host, "localhost" | "127.0.0.1" | "::1");
    let trusted_host = host == "github.com"
        || host == "api.github.com"
        || host == "githubusercontent.com"
        || host.ends_with(".githubusercontent.com")
        || local_test_host;
    if (url.scheme() != "https" && !(local_test_host && url.scheme() == "http"))
        || !trusted_host
        || !url.username().is_empty()
        || url.password().is_some()
        || (url.port().is_some() && !local_test_host)
    {
        return Err(UpdateError::InvalidAssetUrl);
    }
    Ok(url)
}

fn trusted_redirect(previous: &Url, next: &Url, allow_test_localhost: bool) -> bool {
    previous.scheme() == "https"
        && validate_asset_url(next.as_str(), allow_test_localhost).is_ok()
        && next.username().is_empty()
        && next.password().is_none()
}

fn parse_rate_limit_reset(response: &Response) -> Option<u64> {
    response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

fn response_error(response: &Response) -> UpdateError {
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok());
    classify_response_error(
        response.status(),
        remaining,
        parse_rate_limit_reset(response),
    )
}

fn classify_response_error(
    status: StatusCode,
    rate_limit_remaining: Option<&str>,
    reset: Option<u64>,
) -> UpdateError {
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && rate_limit_remaining == Some("0"))
    {
        UpdateError::RateLimited(reset)
    } else {
        UpdateError::HttpStatus(status.as_u16())
    }
}

fn parse_release_response(body: &[u8]) -> Result<Release, UpdateError> {
    let releases = serde_json::from_slice::<Vec<ApiRelease>>(body)
        .map_err(|error| UpdateError::Request(error.to_string()))?;
    select_assets(select_latest_stable(releases)?)
}

fn update_http_client() -> Result<Client, UpdateError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15 * 60))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Adham-AI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| UpdateError::Request(error.to_string()))
}

pub(super) struct ReleaseDiscoveryClient {
    http: Client,
    releases_url: String,
}

impl ReleaseDiscoveryClient {
    pub(super) fn new(repository: Repository) -> Result<Self, UpdateError> {
        let http = update_http_client()?;
        let releases_url = repository.releases_url();
        Ok(Self { http, releases_url })
    }

    #[cfg(test)]
    fn with_test_endpoint(releases_url: String) -> Result<Self, UpdateError> {
        Ok(Self {
            http: update_http_client()?,
            releases_url,
        })
    }

    pub(super) async fn latest_release(&self) -> Result<Release, UpdateError> {
        let response = self
            .http
            .get(&self.releases_url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|error| UpdateError::Request(error.to_string()))?;
        if !response.status().is_success() {
            return Err(response_error(&response));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RELEASES_RESPONSE_BYTES as u64)
        {
            return Err(UpdateError::Request(
                "GitHub's release response exceeded the size limit.".into(),
            ));
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| UpdateError::Request(error.to_string()))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RELEASES_RESPONSE_BYTES {
                return Err(UpdateError::Request(
                    "GitHub's release response exceeded the size limit.".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        parse_release_response(&body)
    }
}

struct DownloadedFile {
    path: PathBuf,
    keep: bool,
}

impl DownloadedFile {
    fn keep(mut self) -> PathBuf {
        self.keep = true;
        self.path.clone()
    }

    fn read(self, maximum: u64) -> Result<Vec<u8>, UpdateError> {
        let metadata = std::fs::metadata(&self.path).map_err(download_io_error)?;
        if metadata.len() > maximum {
            return Err(UpdateError::DownloadTooLarge);
        }
        std::fs::read(&self.path).map_err(download_io_error)
    }
}

impl Drop for DownloadedFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) struct VerifiedUpdateClient {
    http: Client,
    public_key: VerifyingKey,
    allow_test_localhost: bool,
}

impl VerifiedUpdateClient {
    pub(super) fn new() -> Result<Self, UpdateError> {
        Ok(Self {
            http: update_http_client()?,
            public_key: embedded_public_key()?,
            allow_test_localhost: false,
        })
    }

    #[cfg(test)]
    fn for_test(http: Client, public_key: VerifyingKey) -> Self {
        Self {
            http,
            public_key,
            allow_test_localhost: true,
        }
    }

    async fn download_asset(
        &self,
        asset: &ReleaseAsset,
        cache_directory: &Path,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<DownloadedFile, UpdateError> {
        let mut url = validate_asset_url(&asset.url, self.allow_test_localhost)?;
        std::fs::create_dir_all(cache_directory).map_err(download_io_error)?;
        let directory_metadata =
            std::fs::symlink_metadata(cache_directory).map_err(download_io_error)?;
        if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
            return Err(UpdateError::Download(
                "The update cache path must be a regular directory.".into(),
            ));
        }

        let mut redirects = 0;
        let response = loop {
            let response = self
                .http
                .get(url.clone())
                .send()
                .await
                .map_err(|error| UpdateError::Download(error.to_string()))?;
            if !response.status().is_redirection() {
                break response;
            }
            if redirects >= MAX_REDIRECTS {
                return Err(UpdateError::InvalidAssetUrl);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(UpdateError::InvalidAssetUrl)?;
            let next = url
                .join(location)
                .map_err(|_| UpdateError::InvalidAssetUrl)?;
            if !trusted_redirect(&url, &next, self.allow_test_localhost) {
                return Err(UpdateError::InvalidAssetUrl);
            }
            url = next;
            redirects += 1;
        };
        if !response.status().is_success() {
            return Err(response_error(&response));
        }

        let total = Some(asset.size);
        if asset.size > MAX_DOWNLOAD_BYTES
            || response
                .content_length()
                .is_some_and(|length| length != asset.size)
        {
            return Err(UpdateError::DownloadTooLarge);
        }
        let temporary = tempfile::Builder::new()
            .prefix("adham-ai-update-")
            .tempfile_in(cache_directory)
            .map_err(download_io_error)?;
        let (file, temporary_path) = temporary.into_parts();
        let mut file = tokio::fs::File::from_std(file);
        let mut downloaded = 0_u64;
        progress(downloaded, total);

        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| UpdateError::Download(error.to_string()))?
        {
            downloaded = downloaded
                .checked_add(chunk.len() as u64)
                .ok_or(UpdateError::DownloadTooLarge)?;
            if downloaded > MAX_DOWNLOAD_BYTES || total.is_some_and(|length| downloaded > length) {
                return Err(UpdateError::DownloadTooLarge);
            }
            file.write_all(&chunk).await.map_err(download_io_error)?;
            progress(downloaded, total);
        }
        if total.is_some_and(|length| downloaded != length) {
            return Err(UpdateError::Download(
                "The connection ended before the complete asset was received.".into(),
            ));
        }
        file.flush().await.map_err(download_io_error)?;
        file.sync_all().await.map_err(download_io_error)?;
        drop(file);
        let path = temporary_path
            .keep()
            .map_err(|error| download_io_error(error.error))?;
        Ok(DownloadedFile { path, keep: false })
    }

    pub(super) async fn download_verified_package(
        &self,
        release: &Release,
        cache_directory: &Path,
        mut progress: impl FnMut(DownloadProgress),
    ) -> Result<VerifiedPackage, UpdateError> {
        let checksums = self
            .download_asset(&release.checksums, cache_directory, |downloaded, total| {
                progress(DownloadProgress {
                    stage: DownloadStage::Checksums,
                    downloaded,
                    total,
                })
            })
            .await?
            .read(MAX_CHECKSUMS_BYTES)?;
        let signature = self
            .download_asset(&release.signature, cache_directory, |downloaded, total| {
                progress(DownloadProgress {
                    stage: DownloadStage::Signature,
                    downloaded,
                    total,
                })
            })
            .await?
            .read(64)?;

        progress(DownloadProgress {
            stage: DownloadStage::VerifyingSignature,
            downloaded: 0,
            total: Some(release.checksums.size),
        });
        let expected_digest = signed_manifest_digest(
            &self.public_key,
            &checksums,
            &signature,
            &release.package.name,
        )?;

        let package = self
            .download_asset(&release.package, cache_directory, |downloaded, total| {
                progress(DownloadProgress {
                    stage: DownloadStage::Package,
                    downloaded,
                    total,
                })
            })
            .await?;
        progress(DownloadProgress {
            stage: DownloadStage::VerifyingPackage,
            downloaded: 0,
            total: Some(release.package.size),
        });
        verify_package_digest(&package.path, &expected_digest)?;
        progress(DownloadProgress {
            stage: DownloadStage::VerifyingPackage,
            downloaded: release.package.size,
            total: Some(release.package.size),
        });
        Ok(VerifiedPackage {
            path: package.keep(),
            version: release.version.clone(),
            digest: expected_digest,
        })
    }
}

trait ProcessRunner {
    fn run(&self, program: &Path, arguments: &[&std::ffi::OsStr]) -> std::io::Result<Output>;
}

struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run(&self, program: &Path, arguments: &[&std::ffi::OsStr]) -> std::io::Result<Output> {
        Command::new(program).args(arguments).output()
    }
}

const MAX_PROCESS_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_METADATA_BYTES: usize = 64 * 1024;
const APT_ARCHIVE_DIRECTORY: &str = "/var/cache/apt/archives";

pub(super) fn install_verified_package(package: &VerifiedPackage) -> Result<(), UpdateError> {
    let runner = SystemProcessRunner;
    install_verified_package_with_runner(
        package,
        &runner,
        current_process_uid()?,
        Path::new(APT_ARCHIVE_DIRECTORY),
        true,
    )
}

fn install_verified_package_with_runner(
    package: &VerifiedPackage,
    runner: &impl ProcessRunner,
    effective_uid: u32,
    staging_directory: &Path,
    require_root_owned_staging: bool,
) -> Result<(), UpdateError> {
    if effective_uid == 0 {
        return Err(UpdateError::InstallerUnavailable(
            "Adham AI must not run as root.".into(),
        ));
    }
    verify_package_digest(&package.path, &package.digest)?;
    validate_debian_metadata(&package.path, &package.version, runner)?;

    if require_root_owned_staging {
        validate_root_owned_directory(staging_directory)?;
    }

    let staged_path = staging_directory.join(format!(
        "adham-ai-update-{}-{}.deb",
        package.version,
        std::process::id()
    ));
    let staged_output = runner
        .run(
            Path::new("/usr/bin/pkexec"),
            &[
                std::ffi::OsStr::new("/usr/bin/install"),
                std::ffi::OsStr::new("--owner=0"),
                std::ffi::OsStr::new("--group=0"),
                std::ffi::OsStr::new("--mode=0644"),
                std::ffi::OsStr::new("--"),
                package.path.as_os_str(),
                staged_path.as_os_str(),
            ],
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                UpdateError::InstallerUnavailable(
                    "pkexec or the system install utility could not be found.".into(),
                )
            } else {
                UpdateError::InstallerUnavailable(error.to_string())
            }
        })?;
    check_install_process_result(&staged_output)?;
    validate_staged_package(&staged_path, &package.digest, require_root_owned_staging)?;
    validate_debian_metadata(&staged_path, &package.version, runner)?;

    let output = runner
        .run(
            Path::new("/usr/bin/pkexec"),
            &[
                std::ffi::OsStr::new("/usr/bin/apt"),
                std::ffi::OsStr::new("install"),
                std::ffi::OsStr::new("--assume-yes"),
                std::ffi::OsStr::new("--no-remove"),
                staged_path.as_os_str(),
            ],
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                UpdateError::InstallerUnavailable(
                    "pkexec or APT could not be found. Install PolicyKit and APT.".into(),
                )
            } else {
                UpdateError::InstallerUnavailable(error.to_string())
            }
        })?;
    check_install_process_result(&output)
}

fn check_install_process_result(output: &Output) -> Result<(), UpdateError> {
    check_process_output_size(output)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = process_output_text(output);
    if output.status.code() == Some(126)
        || detail.to_ascii_lowercase().contains("cancelled")
        || detail.to_ascii_lowercase().contains("canceled")
    {
        return Err(UpdateError::InstallationCancelled);
    }
    if output.status.code() == Some(127) {
        return Err(UpdateError::AuthorizationFailed);
    }
    classify_apt_failure(&detail)
}

fn validate_root_owned_directory(directory: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mut current = Some(directory);
    while let Some(path) = current {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            UpdateError::InstallerUnavailable(format!(
                "Could not validate the trusted APT staging directory: {error}"
            ))
        })?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            return Err(UpdateError::InstallerUnavailable(
                "The APT staging directory is not protected from unprivileged modification.".into(),
            ));
        }
        current = path.parent();
    }
    Ok(())
}

fn validate_staged_package(
    path: &Path,
    expected_digest: &[u8; 32],
    require_root_owner: bool,
) -> Result<(), UpdateError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = std::fs::symlink_metadata(path).map_err(download_io_error)?;
    validate_staged_metadata(
        metadata.is_file(),
        metadata.uid(),
        metadata.permissions().mode(),
        require_root_owner,
    )?;
    verify_package_digest(path, expected_digest)
}

fn validate_staged_metadata(
    is_regular_file: bool,
    owner_uid: u32,
    mode: u32,
    require_root_owner: bool,
) -> Result<(), UpdateError> {
    if !is_regular_file || (require_root_owner && (owner_uid != 0 || mode & 0o022 != 0)) {
        return Err(UpdateError::InstallerUnavailable(
            "The staged package is not protected from unprivileged modification.".into(),
        ));
    }
    Ok(())
}

fn validate_debian_metadata(
    package_path: &Path,
    expected_version: &Version,
    runner: &impl ProcessRunner,
) -> Result<(), UpdateError> {
    let output = runner
        .run(
            Path::new("/usr/bin/dpkg-deb"),
            &[std::ffi::OsStr::new("--field"), package_path.as_os_str()],
        )
        .map_err(|error| {
            UpdateError::InstallerUnavailable(format!(
                "dpkg-deb metadata validation failed: {error}"
            ))
        })?;
    check_process_output_size(&output)?;
    if !output.status.success() || output.stdout.len() > MAX_PACKAGE_METADATA_BYTES {
        return Err(UpdateError::InvalidDeb);
    }
    let metadata = std::str::from_utf8(&output.stdout).map_err(|_| UpdateError::InvalidDeb)?;
    let fields = parse_control_fields(metadata)?;
    if fields.get("Package").map(String::as_str) != Some("adham-ai")
        || fields.get("Version").map(String::as_str) != Some(expected_version.to_string().as_str())
        || fields.get("Architecture").map(String::as_str) != Some("amd64")
    {
        return Err(UpdateError::InvalidDeb);
    }
    Ok(())
}

fn parse_control_fields(
    metadata: &str,
) -> Result<std::collections::HashMap<String, String>, UpdateError> {
    let mut fields = std::collections::HashMap::new();
    for line in metadata.lines() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            if fields
                .insert(name.to_owned(), value.trim().to_owned())
                .is_some()
            {
                return Err(UpdateError::InvalidDeb);
            }
        }
    }
    Ok(fields)
}

fn check_process_output_size(output: &Output) -> Result<(), UpdateError> {
    if output.stdout.len() > MAX_PROCESS_OUTPUT_BYTES
        || output.stderr.len() > MAX_PROCESS_OUTPUT_BYTES
    {
        return Err(UpdateError::InstallationFailed(
            "Installer output exceeded the permitted size.".into(),
        ));
    }
    Ok(())
}

fn process_output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = format!("{stdout}\n{stderr}").trim().to_owned();
    if detail.is_empty() {
        format!("Installer exited with status {}.", output.status)
    } else {
        detail.chars().take(2000).collect()
    }
}

fn classify_apt_failure(detail: &str) -> Result<(), UpdateError> {
    let lowercase = detail.to_ascii_lowercase();
    if lowercase.contains("could not get lock")
        || lowercase.contains("unable to acquire")
        || lowercase.contains("dpkg frontend lock")
        || lowercase.contains("package cache lock")
    {
        Err(UpdateError::PackageManagerLocked)
    } else if lowercase.contains("unmet dependencies")
        || lowercase.contains("dependency problems")
        || lowercase.contains("depends:")
        || lowercase.contains("but it is not installable")
    {
        Err(UpdateError::MissingDependencies(detail.to_owned()))
    } else {
        Err(UpdateError::InstallationFailed(detail.to_owned()))
    }
}

fn current_process_uid() -> Result<u32, UpdateError> {
    let status = std::fs::read_to_string("/proc/self/status").map_err(|error| {
        UpdateError::InstallerUnavailable(format!("Could not inspect process privileges: {error}"))
    })?;
    effective_uid_from_status(&status)
}

fn effective_uid_from_status(status: &str) -> Result<u32, UpdateError> {
    let uid_line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or_else(|| {
            UpdateError::InstallerUnavailable("Could not inspect process privileges.".into())
        })?;
    uid_line
        .split_whitespace()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            UpdateError::InstallerUnavailable("Could not inspect process privileges.".into())
        })
}

fn download_io_error(error: impl std::fmt::Display) -> UpdateError {
    UpdateError::Download(error.to_string())
}

fn signed_manifest_digest(
    public_key: &VerifyingKey,
    signed_manifest: &[u8],
    signature_bytes: &[u8],
    expected_filename: &str,
) -> Result<[u8; 32], UpdateError> {
    verify_detached_signature(public_key, signed_manifest, signature_bytes)?;
    checksum_manifest_digest(signed_manifest, expected_filename)
}

fn verify_detached_signature(
    public_key: &VerifyingKey,
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), UpdateError> {
    let signature =
        Signature::from_slice(signature_bytes).map_err(|_| UpdateError::InvalidSignature)?;
    public_key
        .verify_strict(message, &signature)
        .map_err(|_| UpdateError::InvalidSignature)
}

fn checksum_manifest_digest(
    signed_manifest: &[u8],
    expected_filename: &str,
) -> Result<[u8; 32], UpdateError> {
    if signed_manifest.len() > 1024 * 1024 {
        return Err(UpdateError::InvalidChecksumManifest);
    }

    let manifest =
        std::str::from_utf8(signed_manifest).map_err(|_| UpdateError::InvalidChecksumManifest)?;
    let mut matching_digest = None;
    let mut filenames = std::collections::HashSet::new();
    for line in manifest.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (digest, filename) = line
            .split_once("  ")
            .ok_or(UpdateError::InvalidChecksumManifest)?;
        if digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || filename.is_empty()
            || !filename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            || !filenames.insert(filename)
        {
            return Err(UpdateError::InvalidChecksumManifest);
        }
        if filename == expected_filename {
            matching_digest = Some(digest);
        }
    }

    let expected_digest = matching_digest.ok_or(UpdateError::InvalidChecksumManifest)?;
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&expected_digest[start..start + 2], 16)
            .map_err(|_| UpdateError::InvalidChecksumManifest)?;
    }
    Ok(digest)
}

fn verify_package_digest(
    package_path: &Path,
    expected_digest: &[u8; 32],
) -> Result<(), UpdateError> {
    let mut package = std::fs::File::open(package_path).map_err(download_io_error)?;
    let mut header = [0; 8];
    package
        .read_exact(&mut header)
        .map_err(|_| UpdateError::InvalidDeb)?;
    if &header != b"!<arch>\n" {
        return Err(UpdateError::InvalidDeb);
    }
    package
        .seek(SeekFrom::Start(0))
        .map_err(download_io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = package.read(&mut buffer).map_err(download_io_error)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hasher.finalize().as_slice() != expected_digest {
        return Err(UpdateError::ChecksumMismatch);
    }
    Ok(())
}

pub(super) fn update_cache_directory() -> Result<PathBuf, UpdateError> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or_else(|| {
            UpdateError::Download("Could not locate the user's cache directory.".into())
        })?;
    let directory = base.join("adham-ai").join("updates");
    std::fs::create_dir_all(&directory).map_err(download_io_error)?;
    let metadata = std::fs::symlink_metadata(&directory).map_err(download_io_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(UpdateError::Download(
            "The update cache path must be a regular directory.".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(download_io_error)?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::ffi::OsString;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct MockHttpResponse {
        status: &'static str,
        declared_length: usize,
        body: Vec<u8>,
    }

    struct MockHttpServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        shutdown: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl MockHttpServer {
        fn start(
            responses: std::collections::HashMap<String, MockHttpResponse>,
            expected_requests: usize,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
            listener
                .set_nonblocking(true)
                .expect("set local test listener nonblocking");
            let address = listener.local_addr().expect("local test server address");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded_requests = Arc::clone(&requests);
            let shutdown = Arc::new(AtomicBool::new(false));
            let worker_shutdown = Arc::clone(&shutdown);
            let worker = thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                for _ in 0..expected_requests {
                    let (mut stream, _) = loop {
                        if worker_shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        match listener.accept() {
                            Ok(connection) => break connection,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && std::time::Instant::now() < deadline =>
                            {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => return,
                        }
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let Ok(cloned) = stream.try_clone() else {
                        return;
                    };
                    let mut reader = std::io::BufReader::new(cloned);
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).is_err() {
                        return;
                    }
                    let path = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let mut header = String::new();
                    loop {
                        header.clear();
                        match reader.read_line(&mut header) {
                            Ok(0) | Err(_) => return,
                            Ok(_) if header == "\r\n" || header == "\n" => break,
                            Ok(_) => {}
                        }
                    }
                    recorded_requests
                        .lock()
                        .expect("request record lock")
                        .push(path.clone());
                    let Some(response) = responses.get(&path) else {
                        return;
                    };
                    let response_headers = format!(
                        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.status, response.declared_length
                    );
                    if stream.write_all(response_headers.as_bytes()).is_err()
                        || stream.write_all(&response.body).is_err()
                    {
                        return;
                    }
                    let _ = stream.flush();
                }
            });
            Self {
                base_url: format!("http://{address}"),
                requests,
                shutdown,
                worker: Some(worker),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base_url)
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("request record lock").clone()
        }
    }

    impl Drop for MockHttpServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    struct MockProcessRunner {
        metadata: Output,
        staging: Output,
        installer: Output,
        invocations: Mutex<Vec<(PathBuf, Vec<OsString>)>>,
    }

    impl ProcessRunner for MockProcessRunner {
        fn run(&self, program: &Path, arguments: &[&std::ffi::OsStr]) -> std::io::Result<Output> {
            self.invocations.lock().expect("invocation lock").push((
                program.to_path_buf(),
                arguments
                    .iter()
                    .map(|argument| argument.to_os_string())
                    .collect(),
            ));
            match program {
                path if path == Path::new("/usr/bin/dpkg-deb") => Ok(self.metadata.clone()),
                path if path == Path::new("/usr/bin/pkexec")
                    && arguments.first().copied()
                        == Some(std::ffi::OsStr::new("/usr/bin/install")) =>
                {
                    if !self.staging.status.success() {
                        return Ok(self.staging.clone());
                    }
                    std::fs::copy(
                        Path::new(std::ffi::OsStr::new(arguments[5])),
                        Path::new(std::ffi::OsStr::new(arguments[6])),
                    )?;
                    Ok(mock_output(0, b"", b""))
                }
                path if path == Path::new("/usr/bin/pkexec") => Ok(self.installer.clone()),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "unexpected executable",
                )),
            }
        }
    }

    fn mock_output(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn mock_package(
        directory: &tempfile::TempDir,
        contents: &[u8],
        version: &str,
    ) -> VerifiedPackage {
        let path = directory.path().join("adham-ai_1.2.3_amd64.deb");
        std::fs::write(&path, contents).expect("write verified package fixture");
        VerifiedPackage {
            path,
            version: Version::parse(version).expect("valid fixture version"),
            digest: Sha256::digest(contents).into(),
        }
    }

    fn mock_runner(metadata: &str, installer: Output) -> MockProcessRunner {
        MockProcessRunner {
            metadata: mock_output(0, metadata.as_bytes(), b""),
            staging: mock_output(0, b"", b""),
            installer,
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn signed_test_manifest(package: &[u8]) -> (Vec<u8>, Vec<u8>, VerifyingKey) {
        let package_digest = Sha256::digest(package);
        let manifest = format!(
            "{}  adham-ai_1.2.3_amd64.deb\n",
            hex_digest(&package_digest)
        )
        .into_bytes();
        let mut test_seed = [0; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut test_seed))
            .expect("read ephemeral test-only signing seed from the system RNG");
        let signing_key = SigningKey::from_bytes(&test_seed);
        let signature = signing_key.sign(&manifest).to_bytes().to_vec();
        (manifest, signature, signing_key.verifying_key())
    }

    fn mock_release_responses(
        package_size: usize,
        manifest: Vec<u8>,
        signature: Vec<u8>,
        package_body: Vec<u8>,
        package_content_length: usize,
    ) -> std::collections::HashMap<String, MockHttpResponse> {
        let assets = [
            (
                "adham-ai_1.2.3_amd64.deb",
                package_size,
                "/adham-ai_1.2.3_amd64.deb",
            ),
            ("SHA256SUMS", manifest.len(), "/SHA256SUMS"),
            ("SHA256SUMS.sig", signature.len(), "/SHA256SUMS.sig"),
        ];
        let release_assets = assets
            .iter()
            .map(|(name, size, path)| {
                serde_json::json!({
                    "name": name,
                    "size": size,
                    "browser_download_url": format!("https://github.com/example/adham-ai/releases/download/v1.2.3/{path}")
                })
            })
            .collect::<Vec<_>>();
        let release_body = serde_json::to_vec(&serde_json::json!([{
            "tag_name": "v1.2.3",
            "name": "Test release",
            "body": "Local test release",
            "published_at": "2026-11-11T00:00:00Z",
            "draft": false,
            "prerelease": false,
            "assets": release_assets
        }]))
        .expect("serialize mocked release API");
        [
            (
                "/releases".to_owned(),
                MockHttpResponse {
                    status: "200 OK",
                    declared_length: release_body.len(),
                    body: release_body,
                },
            ),
            (
                "/SHA256SUMS".to_owned(),
                MockHttpResponse {
                    status: "200 OK",
                    declared_length: manifest.len(),
                    body: manifest,
                },
            ),
            (
                "/SHA256SUMS.sig".to_owned(),
                MockHttpResponse {
                    status: "200 OK",
                    declared_length: signature.len(),
                    body: signature,
                },
            ),
            (
                "/adham-ai_1.2.3_amd64.deb".to_owned(),
                MockHttpResponse {
                    status: "200 OK",
                    declared_length: package_content_length,
                    body: package_body,
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    fn run_mock_install(
        package: &VerifiedPackage,
        runner: &MockProcessRunner,
        effective_uid: u32,
        staging_directory: &Path,
    ) -> Result<(), UpdateError> {
        install_verified_package_with_runner(
            package,
            runner,
            effective_uid,
            staging_directory,
            false,
        )
    }

    #[test]
    fn compares_semantic_versions_without_treating_older_as_an_update() {
        let current = Version::parse("1.2.3").expect("valid current version");
        assert!(newer_version(
            &current,
            &Version::parse("1.2.4").expect("valid candidate")
        ));
        assert!(!newer_version(
            &current,
            &Version::parse("1.2.3").expect("valid candidate")
        ));
        assert!(!newer_version(
            &current,
            &Version::parse("1.2.2").expect("valid candidate")
        ));
        assert!(newer_version(
            &current,
            &Version::parse("1.3.0-alpha.1").expect("valid prerelease")
        ));
        assert!(!newer_version(
            &Version::parse("1.3.0").expect("valid stable version"),
            &Version::parse("1.3.0-alpha.1").expect("valid prerelease")
        ));
    }

    #[test]
    fn selects_highest_stable_release_and_ignores_prereleases_and_drafts() {
        let latest = select_latest_stable(vec![
            api_release("v1.0.0", false, false),
            api_release("v2.0.0-beta.1", false, false),
            api_release("v3.0.0", false, true),
            api_release("v1.8.0", false, false),
        ])
        .expect("stable release");
        assert_eq!(latest.tag_name, "v1.8.0");
    }

    #[test]
    fn reports_missing_release_assets() {
        let error = select_asset(&[], "SHA256SUMS").expect_err("missing manifest");
        assert_eq!(error, UpdateError::MissingAsset("SHA256SUMS".into()));
    }

    #[test]
    fn rejects_unexpected_release_assets() {
        let mut release = api_release("v1.2.3", false, false);
        release.assets.push(ApiAsset {
            name: "untrusted-installer.sh".into(),
            browser_download_url: "https://github.com/example/adham-ai/installer.sh".into(),
            size: 32,
        });
        assert_eq!(
            select_assets_for_arch(release, "linux", "x86_64"),
            Err(UpdateError::UnexpectedAsset(
                "untrusted-installer.sh".into()
            ))
        );
    }

    #[test]
    fn reports_missing_package_asset_for_supported_architecture() {
        assert_eq!(
            select_assets_for_arch(api_release("v1.0.0", false, false), "linux", "x86_64"),
            Err(UpdateError::MissingAsset("adham-ai_1.0.0_amd64.deb".into()))
        );
    }

    #[test]
    fn rejects_asset_urls_outside_https_github_hosts() {
        for invalid in [
            "http://github.com/releases/package.deb",
            "https://github.com.evil.example/package.deb",
            "https://githubusercontent.com@evil.example/package.deb",
            "https://user@github.com/releases/package.deb",
            "https://github.com:444/releases/package.deb",
        ] {
            assert!(
                validate_github_asset_url(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(
            validate_github_asset_url("https://objects.githubusercontent.com/release/package.deb")
                .is_ok()
        );
    }

    #[test]
    fn rejects_unsupported_platforms_before_selecting_release_assets() {
        assert_eq!(
            select_assets_for_arch(api_release("v1.0.0", false, false), "linux", "aarch64"),
            Err(UpdateError::UnsupportedArchitecture)
        );
    }

    #[test]
    fn parses_optional_v_prefix_and_rejects_invalid_versions() {
        assert_eq!(
            parse_version("v2.4.0").expect("valid tag"),
            Version::parse("2.4.0").expect("valid version")
        );
        assert!(matches!(
            parse_version("latest"),
            Err(UpdateError::InvalidVersion(_))
        ));
    }

    #[test]
    fn repository_url_requires_official_https_github_origin() {
        let repository =
            Repository::parse("https://github.com/example/adham-ai").expect("valid repository URL");
        assert_eq!(
            repository.releases_page_url(),
            "https://github.com/example/adham-ai/releases"
        );
        for invalid in [
            "http://github.com/example/adham-ai",
            "https://example.com/example/adham-ai",
            "https://github.com/example/adham-ai/issues",
            "https://user@github.com/example/adham-ai",
            "https://github.com/example/../adham-ai",
        ] {
            assert!(Repository::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn configured_repository_uses_the_user_confirmed_github_url() {
        let repository = configured_repository().expect("configured repository");
        assert_eq!(
            repository.releases_page_url(),
            "https://github.com/adhamblabel31-boop/Adham-AI/releases"
        );
    }

    #[test]
    fn configured_build_embeds_only_the_validated_public_key() {
        let public_key = embedded_public_key().expect("configured Ed25519 public key");
        assert!(!public_key.is_weak());
        assert_eq!(
            public_key.to_bytes(),
            decode_hex_key("a658a3971933f4435fd4c99f7d775c96ef9409be5c6cd63062b8a97409ee40b1")
                .expect("validated 32-byte public key")
        );
    }

    #[test]
    fn configured_public_key_verifies_owner_signed_self_test_challenge() {
        let public_key = embedded_public_key().expect("configured Ed25519 public key");
        let signature = decode_hex::<64>(
            "5897ed9f922ea1f310783ea17dad8ed7ec89ac96e4df2e584cc8a5cbfe3ffb8f\
             9d61ad4a89e229d939962f1adc6b9c792586a2f1c10aa209e091fc0af1ef3603",
        );
        assert_eq!(
            verify_detached_signature(
                &public_key,
                b"Adham AI update signing key self-test v1",
                &signature,
            ),
            Ok(())
        );
    }

    #[test]
    fn public_key_hex_decoder_requires_exactly_32_bytes() {
        assert_eq!(decode_hex_key(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(decode_hex_key(&"0".repeat(63)), None);
        assert_eq!(decode_hex_key(&"xz".repeat(32)), None);
        assert_eq!(
            parse_public_key_config(None),
            Err(UpdateError::NotConfigured)
        );
        assert_eq!(
            parse_public_key_config(Some(&"0".repeat(64))),
            Err(UpdateError::InvalidPublicKey)
        );
    }

    #[test]
    fn parses_release_api_failures_without_treating_them_as_success() {
        assert!(matches!(
            parse_release_response(b"not-json"),
            Err(UpdateError::Request(_))
        ));
        assert_eq!(
            parse_release_response(b"[]"),
            Err(UpdateError::NoStableRelease)
        );
    }

    #[test]
    fn distinguishes_github_rate_limits_from_other_api_errors() {
        assert_eq!(
            classify_response_error(StatusCode::FORBIDDEN, Some("0"), Some(1_700_000_000)),
            UpdateError::RateLimited(Some(1_700_000_000))
        );
        assert_eq!(
            classify_response_error(StatusCode::TOO_MANY_REQUESTS, None, None),
            UpdateError::RateLimited(None)
        );
        assert_eq!(
            classify_response_error(StatusCode::SERVICE_UNAVAILABLE, None, None),
            UpdateError::HttpStatus(503)
        );
    }

    fn run_async<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create test async runtime")
            .block_on(future)
    }

    fn local_release(server: &MockHttpServer, mut release: Release) -> Release {
        release.package.url = server.url("/adham-ai_1.2.3_amd64.deb");
        release.checksums.url = server.url("/SHA256SUMS");
        release.signature.url = server.url("/SHA256SUMS.sig");
        release
    }

    #[test]
    fn local_http_release_discovery_downloads_and_verifies_in_order() {
        let package = b"!<arch>\nAdham AI test package".to_vec();
        let (manifest, signature, public_key) = signed_test_manifest(&package);
        let responses = mock_release_responses(
            package.len(),
            manifest,
            signature,
            package.clone(),
            package.len(),
        );
        let server = MockHttpServer::start(responses, 4);
        let discovery = ReleaseDiscoveryClient::with_test_endpoint(server.url("/releases"))
            .expect("create mocked release discovery client");
        let release = local_release(
            &server,
            run_async(discovery.latest_release()).expect("discover mocked release"),
        );
        assert_eq!(
            release.version,
            Version::parse("1.2.3").expect("test version")
        );

        let client = VerifiedUpdateClient::for_test(
            update_http_client().expect("create test HTTP client"),
            public_key,
        );
        let cache = tempfile::tempdir().expect("create temporary update cache");
        let mut stages = Vec::new();
        let verified = run_async(client.download_verified_package(
            &release,
            cache.path(),
            |progress| stages.push(progress.stage),
        ))
        .expect("download and verify the mocked release");
        assert_eq!(
            std::fs::read(verified.path()).expect("read downloaded test package"),
            package
        );
        assert_eq!(
            server.requests(),
            [
                "/releases",
                "/SHA256SUMS",
                "/SHA256SUMS.sig",
                "/adham-ai_1.2.3_amd64.deb"
            ]
        );
        let signature_stage = stages
            .iter()
            .position(|stage| *stage == DownloadStage::VerifyingSignature)
            .expect("signature verification progress");
        let package_stage = stages
            .iter()
            .position(|stage| *stage == DownloadStage::Package)
            .expect("package download progress");
        assert!(signature_stage < package_stage);
    }

    #[test]
    fn local_http_modified_manifest_and_invalid_signature_never_fetch_package() {
        let package = b"!<arch>\nAdham AI test package".to_vec();
        let (manifest, signature, public_key) = signed_test_manifest(&package);
        let mut modified_manifest = manifest.clone();
        modified_manifest[0] = if modified_manifest[0] == b'0' {
            b'1'
        } else {
            b'0'
        };
        let mut modified_signature = signature.clone();
        modified_signature[0] ^= 1;
        for (manifest, signature) in [
            (modified_manifest, signature),
            (manifest, modified_signature),
        ] {
            let responses = mock_release_responses(
                package.len(),
                manifest,
                signature,
                package.clone(),
                package.len(),
            );
            let server = MockHttpServer::start(responses, 3);
            let discovery = ReleaseDiscoveryClient::with_test_endpoint(server.url("/releases"))
                .expect("create mocked release discovery client");
            let release = local_release(
                &server,
                run_async(discovery.latest_release()).expect("discover mocked release"),
            );
            let client = VerifiedUpdateClient::for_test(
                update_http_client().expect("create test HTTP client"),
                public_key,
            );
            let cache = tempfile::tempdir().expect("create temporary update cache");
            assert_eq!(
                run_async(client.download_verified_package(&release, cache.path(), |_| {})),
                Err(UpdateError::InvalidSignature)
            );
            assert_eq!(
                server.requests(),
                ["/releases", "/SHA256SUMS", "/SHA256SUMS.sig"]
            );
            assert_eq!(
                std::fs::read_dir(cache.path())
                    .expect("read test cache")
                    .count(),
                0,
                "rejected data must not be left in the update cache"
            );
        }
    }

    #[test]
    fn local_http_rejects_corrupted_and_interrupted_package_downloads() {
        let package = b"!<arch>\nAdham AI test package".to_vec();
        let (manifest, signature, public_key) = signed_test_manifest(&package);
        let mut corrupted = package.clone();
        *corrupted.last_mut().expect("non-empty test package") ^= 1;
        for (payload, content_length, interrupted) in [
            (corrupted, package.len(), false),
            (package[..package.len() - 3].to_vec(), package.len(), true),
        ] {
            let responses = mock_release_responses(
                package.len(),
                manifest.clone(),
                signature.clone(),
                payload,
                content_length,
            );
            let server = MockHttpServer::start(responses, 4);
            let discovery = ReleaseDiscoveryClient::with_test_endpoint(server.url("/releases"))
                .expect("create mocked release discovery client");
            let release = local_release(
                &server,
                run_async(discovery.latest_release()).expect("discover mocked release"),
            );
            let client = VerifiedUpdateClient::for_test(
                update_http_client().expect("create test HTTP client"),
                public_key,
            );
            let cache = tempfile::tempdir().expect("create temporary update cache");
            let result =
                run_async(client.download_verified_package(&release, cache.path(), |_| {}));
            if interrupted {
                assert!(matches!(result, Err(UpdateError::Download(_))));
            } else {
                assert_eq!(result, Err(UpdateError::ChecksumMismatch));
            }
            assert_eq!(
                server.requests(),
                [
                    "/releases",
                    "/SHA256SUMS",
                    "/SHA256SUMS.sig",
                    "/adham-ai_1.2.3_amd64.deb"
                ]
            );
            assert_eq!(
                std::fs::read_dir(cache.path())
                    .expect("read test cache")
                    .count(),
                0,
                "failed package files must be removed"
            );
        }
    }

    #[test]
    fn accepts_the_rfc8032_ed25519_signature_vector() {
        let public_key_bytes =
            decode_hex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let public_key = VerifyingKey::from_bytes(&public_key_bytes).expect("RFC 8032 public key");
        let signature = decode_hex::<64>(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );
        assert_eq!(
            verify_detached_signature(&public_key, b"", &signature),
            Ok(())
        );
    }

    #[test]
    fn rejects_tampered_wrong_sized_and_untrusted_signatures() {
        let public_key_bytes =
            decode_hex::<32>("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let public_key = VerifyingKey::from_bytes(&public_key_bytes).expect("RFC 8032 public key");
        let signature = decode_hex::<64>(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );

        assert_eq!(
            verify_detached_signature(&public_key, b"tampered", &signature),
            Err(UpdateError::InvalidSignature)
        );
        assert_eq!(
            signed_manifest_digest(
                &public_key,
                b"not a checksum manifest",
                &[0; 64],
                "adham-ai_1.2.3_amd64.deb"
            ),
            Err(UpdateError::InvalidSignature)
        );
        let mut tampered_signature = signature;
        tampered_signature[0] ^= 1;
        assert_eq!(
            verify_detached_signature(&public_key, b"", &tampered_signature),
            Err(UpdateError::InvalidSignature)
        );
        assert_eq!(
            verify_detached_signature(&public_key, b"", &[0; 63]),
            Err(UpdateError::InvalidSignature)
        );
        assert_eq!(
            verify_detached_signature(&public_key, b"", &[0; 65]),
            Err(UpdateError::InvalidSignature)
        );
        assert_eq!(
            verify_detached_signature(
                &embedded_public_key().expect("configured application key"),
                b"",
                &signature
            ),
            Err(UpdateError::InvalidSignature)
        );
    }

    #[test]
    fn checksum_manifest_parser_accepts_only_expected_well_formed_unique_entries() {
        let digest = [0x5a; 32];
        let manifest = format!("{}  adham-ai_1.2.3_amd64.deb\n", hex_digest(&digest));
        assert_eq!(
            checksum_manifest_digest(manifest.as_bytes(), "adham-ai_1.2.3_amd64.deb"),
            Ok(digest)
        );
        assert_eq!(
            checksum_manifest_digest(
                format!("{}  unexpected.deb\n", "a".repeat(64)).as_bytes(),
                "adham-ai_1.2.3_amd64.deb",
            ),
            Err(UpdateError::InvalidChecksumManifest)
        );
        assert_eq!(
            checksum_manifest_digest(
                format!(
                    "{}  adham-ai_1.2.3_amd64.deb\n{}  adham-ai_1.2.3_amd64.deb\n",
                    "a".repeat(64),
                    "b".repeat(64)
                )
                .as_bytes(),
                "adham-ai_1.2.3_amd64.deb"
            ),
            Err(UpdateError::InvalidChecksumManifest)
        );
    }

    #[test]
    fn verifies_deb_archive_header_before_checksum() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package_path = directory.path().join("package.deb");
        std::fs::write(&package_path, b"not a deb archive").expect("write fixture");
        assert_eq!(
            verify_package_digest(&package_path, &[0; 32]),
            Err(UpdateError::InvalidDeb)
        );
    }

    #[test]
    fn verifies_checksum_manifest_digest_against_deb_archive() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package_path = directory.path().join("package.deb");
        let package = b"!<arch>\nunit-test-package";
        std::fs::write(&package_path, package).expect("write fixture");
        let digest = Sha256::digest(package);
        let manifest = format!("{}  adham-ai_1.2.3_amd64.deb\n", hex_digest(&digest));
        let expected_digest =
            checksum_manifest_digest(manifest.as_bytes(), "adham-ai_1.2.3_amd64.deb")
                .expect("checksum manifest");
        assert_eq!(
            verify_package_digest(&package_path, &expected_digest),
            Ok(())
        );
        assert_eq!(
            verify_package_digest(&package_path, &[0; 32]),
            Err(UpdateError::ChecksumMismatch)
        );
    }

    #[test]
    fn installs_only_verified_deb_using_pkexec_and_apt_arguments() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let contents = b"!<arch>\nverified package fixture";
        let package = mock_package(&directory, contents, "1.2.3");
        let runner = mock_runner(
            "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"installed", b""),
        );
        let staging = directory.path().join("staging");
        std::fs::create_dir(&staging).expect("create test staging directory");

        assert_eq!(run_mock_install(&package, &runner, 1000, &staging), Ok(()));
        let invocations = runner.invocations.lock().expect("invocation lock");
        assert_eq!(invocations.len(), 4);
        assert_eq!(invocations[0].0, Path::new("/usr/bin/dpkg-deb"));
        assert_eq!(invocations[1].0, Path::new("/usr/bin/pkexec"));
        assert_eq!(
            invocations[1].1[..5],
            [
                OsString::from("/usr/bin/install"),
                OsString::from("--owner=0"),
                OsString::from("--group=0"),
                OsString::from("--mode=0644"),
                OsString::from("--"),
            ]
        );
        assert_eq!(invocations[2].0, Path::new("/usr/bin/dpkg-deb"));
        assert_eq!(invocations[3].0, Path::new("/usr/bin/pkexec"));
        assert_eq!(
            invocations[3].1,
            vec![
                OsString::from("/usr/bin/apt"),
                OsString::from("install"),
                OsString::from("--assume-yes"),
                OsString::from("--no-remove"),
                invocations[1].1[6].clone(),
            ]
        );
        assert_ne!(
            invocations[3].1[4],
            package.path.as_os_str().to_os_string(),
            "APT must consume the protected staged copy, not the user-writable source"
        );
    }

    #[test]
    fn refuses_tampered_package_before_invoking_any_process() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package = mock_package(&directory, b"!<arch>\ntampered package fixture", "1.2.3");
        let mut package = package;
        package.digest = [0; 32];
        let runner = mock_runner(
            "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"", b""),
        );

        assert_eq!(
            run_mock_install(&package, &runner, 1000, directory.path()),
            Err(UpdateError::ChecksumMismatch)
        );
        assert!(
            runner
                .invocations
                .lock()
                .expect("invocation lock")
                .is_empty()
        );
    }

    #[test]
    fn rejects_wrong_package_identity_before_requesting_authorization() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package = mock_package(&directory, b"!<arch>\nverified package", "1.2.3");
        let runner = mock_runner(
            "Package: unrelated\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"", b""),
        );

        assert_eq!(
            run_mock_install(&package, &runner, 1000, directory.path()),
            Err(UpdateError::InvalidDeb)
        );
        let invocations = runner.invocations.lock().expect("invocation lock");
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].0, Path::new("/usr/bin/dpkg-deb"));
    }

    #[test]
    fn refuses_to_install_when_application_runs_as_root() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package = mock_package(&directory, b"!<arch>\nverified package", "1.2.3");
        let runner = mock_runner(
            "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"", b""),
        );

        assert_eq!(
            run_mock_install(&package, &runner, 0, directory.path()),
            Err(UpdateError::InstallerUnavailable(
                "Adham AI must not run as root.".into()
            ))
        );
        assert!(
            runner
                .invocations
                .lock()
                .expect("invocation lock")
                .is_empty()
        );
    }

    #[test]
    fn reports_locks_missing_dependencies_installation_failure_and_cancellation() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package = mock_package(&directory, b"!<arch>\nverified package", "1.2.3");
        let metadata = "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n";

        for (status, detail, expected) in [
            (
                1,
                "Could not get lock /var/lib/dpkg/lock",
                UpdateError::PackageManagerLocked,
            ),
            (
                1,
                "Unmet dependencies: required package is not installable",
                UpdateError::MissingDependencies(
                    "Unmet dependencies: required package is not installable".into(),
                ),
            ),
            (
                126,
                "Authentication cancelled",
                UpdateError::InstallationCancelled,
            ),
            (
                127,
                "Authorization denied",
                UpdateError::AuthorizationFailed,
            ),
            (
                1,
                "The package maintainer script failed.",
                UpdateError::InstallationFailed("The package maintainer script failed.".into()),
            ),
        ] {
            let runner = mock_runner(metadata, mock_output(status, b"", detail.as_bytes()));
            assert_eq!(
                run_mock_install(&package, &runner, 1000, directory.path()),
                Err(expected),
                "unexpected result for {detail}"
            );
            assert_eq!(runner.invocations.lock().expect("invocation lock").len(), 4);
        }
    }

    #[test]
    fn reports_policykit_authorization_cancellation_before_apt_starts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let package = mock_package(&directory, b"!<arch>\nverified package", "1.2.3");
        let mut runner = mock_runner(
            "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"", b""),
        );
        runner.staging = mock_output(126, b"", b"Authentication dialog dismissed");

        assert_eq!(
            run_mock_install(&package, &runner, 1000, directory.path()),
            Err(UpdateError::InstallationCancelled)
        );
        assert_eq!(runner.invocations.lock().expect("invocation lock").len(), 2);
    }

    #[test]
    fn mock_installation_does_not_modify_user_data() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let user_data = directory.path().join("existing-user-data.sqlite");
        let original_data = b"existing chat history and preferences";
        std::fs::write(&user_data, original_data).expect("write preserved-data fixture");
        let package = mock_package(&directory, b"!<arch>\nverified package", "1.2.3");
        let runner = mock_runner(
            "Package: adham-ai\nVersion: 1.2.3\nArchitecture: amd64\n",
            mock_output(0, b"", b""),
        );

        assert_eq!(
            run_mock_install(&package, &runner, 1000, directory.path()),
            Ok(())
        );
        assert_eq!(
            std::fs::read(&user_data).expect("read preserved-data fixture"),
            original_data
        );
    }

    #[test]
    fn effective_uid_parser_reads_the_effective_uid_field() {
        assert_eq!(
            effective_uid_from_status("Uid:\t1000\t0\t1000\t1000\n"),
            Ok(0)
        );
        assert_eq!(
            effective_uid_from_status("Uid:\t0\t1000\t0\t1000\n"),
            Ok(1000)
        );
        assert!(effective_uid_from_status("Uid:\t1000\n").is_err());
    }

    #[test]
    fn staged_package_must_be_regular_and_match_the_signed_digest() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("staged.deb");
        let package = b"!<arch>\nverified staged package";
        std::fs::write(&path, package).expect("write staged package");
        let digest: [u8; 32] = Sha256::digest(package).into();
        assert_eq!(validate_staged_package(&path, &digest, false), Ok(()));
        assert_eq!(
            validate_staged_package(&path, &[0; 32], false),
            Err(UpdateError::ChecksumMismatch)
        );
        std::fs::remove_file(&path).expect("remove staged package");
        std::os::unix::fs::symlink(directory.path().join("missing"), &path)
            .expect("create staged symlink");
        assert!(matches!(
            validate_staged_package(&path, &digest, false),
            Err(UpdateError::InstallerUnavailable(_))
        ));
    }

    #[test]
    fn staged_package_permissions_reject_user_writable_or_non_root_files() {
        assert_eq!(validate_staged_metadata(true, 0, 0o100644, true), Ok(()));
        assert!(validate_staged_metadata(true, 1000, 0o100644, true).is_err());
        assert!(validate_staged_metadata(true, 0, 0o100664, true).is_err());
        assert!(validate_staged_metadata(false, 0, 0o100644, true).is_err());
    }

    fn decode_hex<const N: usize>(value: &str) -> [u8; N] {
        let value = value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        assert_eq!(value.len(), N * 2);
        let mut bytes = [0; N];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).expect("hex test vector");
        }
        bytes
    }

    fn hex_digest(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn api_release(tag: &str, prerelease: bool, draft: bool) -> ApiRelease {
        ApiRelease {
            tag_name: tag.into(),
            name: None,
            body: None,
            published_at: None,
            draft,
            prerelease,
            assets: Vec::new(),
        }
    }
}
