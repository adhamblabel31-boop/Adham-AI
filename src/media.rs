use bytes::Bytes;
use iced::futures::StreamExt;
use reqwest::{Client, Response};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::{Host, Url};

pub(super) const DEFAULT_API_BASE_URL: &str = "http://127.0.0.1:8188";
const API_BASE_URL_ENV: &str = "ADHAM_AI_COMFYUI_URL";
const CHECKPOINT_FILENAME: &str = "DreamShaper_8_pruned.safetensors";
const MAX_PROMPT_BYTES: usize = 2_000;
const MIN_STEPS: u32 = 1;
const MAX_STEPS: u32 = 100;
const MAX_RESPONSE_BYTES: usize = 42 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const GENERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const QUEUE_DISAPPEAR_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BackendStatus {
    Checking,
    Starting,
    Available {
        checkpoint: String,
        device: String,
        ownership: ServiceOwnership,
    },
    Unavailable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceOwnership {
    External,
    Application,
}

impl BackendStatus {
    pub(super) fn user_message(&self) -> String {
        match self {
            Self::Checking => "Checking the local ComfyUI API…".into(),
            Self::Starting => "Starting ComfyUI in CPU-only mode…".into(),
            Self::Available {
                checkpoint,
                device,
                ownership,
            } => {
                let owner = match ownership {
                    ServiceOwnership::External => "using the existing service",
                    ServiceOwnership::Application => "started by Adham AI",
                };
                format!("ComfyUI is ready on {device} ({owner}). Checkpoint: {checkpoint}")
            }
            Self::Unavailable(reason) => reason.clone(),
        }
    }

    pub(super) fn is_available(&self) -> bool {
        matches!(self, Self::Available { checkpoint, .. } if !checkpoint.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackendInfo {
    checkpoint: String,
    device: String,
}

#[derive(Debug, Clone)]
pub(super) struct MediaImage {
    pub(super) bytes: Bytes,
    pub(super) handle: iced::widget::image::Handle,
}

impl MediaImage {
    pub(super) fn new(bytes: Vec<u8>) -> Result<Self, String> {
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("ComfyUI did not return a supported PNG image.".into());
        }
        let bytes = Bytes::from(bytes);
        let handle = iced::widget::image::Handle::from_bytes(bytes.clone());
        Ok(Self { bytes, handle })
    }
}

#[derive(Clone)]
pub(super) struct ImageBackend {
    http: Client,
    base_url: String,
}

impl ImageBackend {
    pub(super) fn new() -> Result<Self, String> {
        Self::with_base_url(&configured_api_base_url()?)
    }

    fn with_base_url(base_url: &str) -> Result<Self, String> {
        let base_url = validate_api_base_url(base_url)?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|error| format!("Could not configure the local ComfyUI client: {error}"))?;
        Ok(Self { http, base_url })
    }

    async fn discover(&self) -> Result<BackendInfo, String> {
        let system_stats = self.get_json("system_stats").await?;
        let device = system_stats
            .get("devices")
            .and_then(Value::as_array)
            .and_then(|devices| {
                devices.iter().find_map(|device| {
                    (device.get("type").and_then(Value::as_str) == Some("cpu"))
                        .then(|| device.get("name").and_then(Value::as_str).unwrap_or("CPU"))
                })
            })
            .ok_or_else(|| {
                "ComfyUI is responding, but its system status does not report a CPU device. \
                 CPU-only generation is unavailable."
                    .to_owned()
            })?
            .to_owned();

        let node_info = self.get_json("object_info/CheckpointLoaderSimple").await?;
        let node = node_info
            .get("CheckpointLoaderSimple")
            .unwrap_or(&node_info);
        let checkpoints = node
            .get("input")
            .and_then(|input| input.get("required"))
            .and_then(|required| required.get("ckpt_name"))
            .and_then(Value::as_array)
            .and_then(|options| options.first())
            .and_then(Value::as_array)
            .ok_or_else(|| {
                "ComfyUI did not return the CheckpointLoaderSimple checkpoint list.".to_owned()
            })?;
        let has_checkpoint = checkpoints
            .iter()
            .any(|checkpoint| checkpoint.as_str() == Some(CHECKPOINT_FILENAME));
        if !has_checkpoint {
            return Err(format!(
                "ComfyUI is responding, but it does not list the required checkpoint \
                 {CHECKPOINT_FILENAME}."
            ));
        }

        Ok(BackendInfo {
            checkpoint: CHECKPOINT_FILENAME.into(),
            device,
        })
    }

    pub(super) async fn generate(
        &self,
        prompt: String,
        steps: u32,
        active_prompt_id: Arc<Mutex<Option<String>>>,
    ) -> Result<Vec<u8>, String> {
        validate_prompt(&prompt)?;
        validate_steps(&steps.to_string())?;
        active_prompt_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();

        let result = self
            .generate_inner(prompt, steps, active_prompt_id.clone())
            .await;
        active_prompt_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        result
    }

    async fn generate_inner(
        &self,
        prompt: String,
        steps: u32,
        active_prompt_id: Arc<Mutex<Option<String>>>,
    ) -> Result<Vec<u8>, String> {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let client_id = format!("adham-ai-{}-{seed}", std::process::id());
        let response = self
            .http
            .post(self.endpoint("prompt"))
            .json(&json!({
                "client_id": client_id,
                "prompt": build_workflow(&prompt, CHECKPOINT_FILENAME, seed, steps)
            }))
            .send()
            .await
            .map_err(|error| format!("Could not submit the image workflow to ComfyUI: {error}"))?;
        let status = response.status();
        let body = read_limited_body(response, MAX_RESPONSE_BYTES).await?;
        if !status.is_success() {
            return Err(prompt_http_error(status, &body));
        }
        let response: Value = serde_json::from_slice(&body)
            .map_err(|error| format!("ComfyUI returned an invalid /prompt response: {error}"))?;
        let prompt_id = response
            .get("prompt_id")
            .and_then(Value::as_str)
            .filter(|prompt_id| !prompt_id.is_empty())
            .ok_or_else(|| {
                let errors = response
                    .get("node_errors")
                    .map(Value::to_string)
                    .unwrap_or_default();
                if errors.is_empty() {
                    "ComfyUI accepted no prompt ID. Check the workflow and checkpoint.".into()
                } else {
                    format!("ComfyUI rejected the workflow: {errors}")
                }
            })?
            .to_owned();

        *active_prompt_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(prompt_id.clone());
        self.wait_for_image(&prompt_id).await
    }

    async fn wait_for_image(&self, prompt_id: &str) -> Result<Vec<u8>, String> {
        let started = Instant::now();
        let mut absent_since = None;
        loop {
            let history = self
                .get_json(&format!("history/{prompt_id}"))
                .await
                .map_err(|error| format!("Could not check ComfyUI generation history: {error}"))?;
            if let Some(entry) = history.get(prompt_id) {
                if let Some(error) = history_failure(entry) {
                    return Err(format!("ComfyUI could not generate the image: {error}"));
                }
                if let Some(image) = history_image(entry) {
                    return self.fetch_image(image).await;
                }
                if entry
                    .get("status")
                    .and_then(|status| status.get("completed"))
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    return Err("ComfyUI finished the workflow without saving an image.".into());
                }
            }

            let queue = self.get_json("queue").await.map_err(|error| {
                format!("Could not check the ComfyUI generation queue: {error}")
            })?;
            if queue_contains_prompt(&queue, prompt_id) {
                absent_since = None;
            } else {
                let since = *absent_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= QUEUE_DISAPPEAR_TIMEOUT {
                    return Err("ComfyUI no longer lists the job in its queue or history.".into());
                }
            }
            if started.elapsed() >= GENERATION_TIMEOUT {
                return Err("ComfyUI generation timed out after 30 minutes.".into());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn fetch_image(&self, image: HistoryImage) -> Result<Vec<u8>, String> {
        let response = self
            .http
            .get(self.endpoint("view"))
            .query(&[
                ("filename", image.filename.as_str()),
                ("subfolder", image.subfolder.as_str()),
                ("type", image.image_type.as_str()),
            ])
            .send()
            .await
            .map_err(|error| {
                format!("Could not retrieve the generated image from ComfyUI: {error}")
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "ComfyUI could not return the generated image (HTTP {}).",
                response.status()
            ));
        }
        let bytes = read_limited_body(response, MAX_RESPONSE_BYTES).await?;
        tokio::task::spawn_blocking(move || {
            if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err("ComfyUI returned an output that is not a PNG image.".into());
            }
            validate_decodable_png(&bytes)?;
            Ok(bytes)
        })
        .await
        .map_err(|error| format!("Image validation worker failed: {error}"))?
    }

    pub(super) async fn cancel(&self, prompt_id: &str) -> Result<bool, String> {
        let response = self
            .http
            .post(self.endpoint(&format!("api/jobs/{prompt_id}/cancel")))
            .send()
            .await
            .map_err(|error| {
                format!("Could not send the cancellation request to ComfyUI: {error}")
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(
                "This ComfyUI server does not expose per-job cancellation; the backend may continue processing."
                    .into(),
            );
        }
        if !response.status().is_success() {
            return Err(format!(
                "ComfyUI rejected the cancellation request (HTTP {}).",
                response.status()
            ));
        }
        let body = read_limited_body(response, 1024 * 1024).await?;
        let result: Value = serde_json::from_slice(&body).map_err(|error| {
            format!("ComfyUI returned an invalid cancellation response: {error}")
        })?;
        result
            .get("cancelled")
            .and_then(Value::as_bool)
            .ok_or_else(|| "ComfyUI did not report whether the job was cancelled.".into())
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let response = self
            .http
            .get(self.endpoint(path))
            .send()
            .await
            .map_err(|error| {
                format!("Could not connect to ComfyUI at {}: {error}", self.base_url)
            })?;
        if !response.status().is_success() {
            return Err(format!(
                "ComfyUI returned HTTP {} for /{path}.",
                response.status()
            ));
        }
        let body = read_limited_body(response, MAX_RESPONSE_BYTES).await?;
        serde_json::from_slice(&body)
            .map_err(|error| format!("ComfyUI returned invalid JSON for /{path}: {error}"))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }
}

struct HistoryImage {
    filename: String,
    subfolder: String,
    image_type: String,
}

fn build_workflow(prompt: &str, checkpoint: &str, seed: u64, steps: u32) -> Value {
    json!({
        "1": {
            "class_type": "CheckpointLoaderSimple",
            "inputs": { "ckpt_name": checkpoint }
        },
        "2": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": prompt, "clip": ["1", 1] }
        },
        "3": {
            "class_type": "CLIPTextEncode",
            "inputs": { "text": "", "clip": ["1", 1] }
        },
        "4": {
            "class_type": "EmptyLatentImage",
            "inputs": { "width": 512, "height": 512, "batch_size": 1 }
        },
        "5": {
            "class_type": "KSampler",
            "inputs": {
                "seed": seed,
                "steps": steps,
                "cfg": 7.0,
                "sampler_name": "euler",
                "scheduler": "normal",
                "denoise": 1.0,
                "model": ["1", 0],
                "positive": ["2", 0],
                "negative": ["3", 0],
                "latent_image": ["4", 0]
            }
        },
        "6": {
            "class_type": "VAEDecode",
            "inputs": { "samples": ["5", 0], "vae": ["1", 2] }
        },
        "7": {
            "class_type": "SaveImage",
            "inputs": { "filename_prefix": "adham-ai", "images": ["6", 0] }
        }
    })
}

fn history_image(entry: &Value) -> Option<HistoryImage> {
    entry
        .get("outputs")?
        .as_object()?
        .values()
        .filter_map(|output| output.get("images").and_then(Value::as_array))
        .flatten()
        .find_map(|image| {
            Some(HistoryImage {
                filename: image.get("filename")?.as_str()?.to_owned(),
                subfolder: image
                    .get("subfolder")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                image_type: image
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("output")
                    .to_owned(),
            })
        })
}

fn history_failure(entry: &Value) -> Option<String> {
    let status = entry.get("status")?;
    if status.get("status_str").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let details = status
        .get("messages")
        .map(Value::to_string)
        .filter(|details| details != "null")
        .unwrap_or_default();
    Some(if details.is_empty() {
        "workflow execution failed".into()
    } else {
        details
    })
}

fn prompt_http_error(status: reqwest::StatusCode, body: &[u8]) -> String {
    let details = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|response| {
            response
                .get("node_errors")
                .or_else(|| response.get("error"))
                .or_else(|| response.get("message"))
                .map(Value::to_string)
        });
    match details {
        Some(details) => format!("ComfyUI rejected the image workflow (HTTP {status}): {details}"),
        None => format!("ComfyUI rejected the image workflow (HTTP {status})."),
    }
}

fn queue_contains_prompt(queue: &Value, prompt_id: &str) -> bool {
    ["queue_running", "queue_pending"].into_iter().any(|key| {
        queue
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_array()
                        .and_then(|fields| fields.get(1))
                        .and_then(Value::as_str)
                        == Some(prompt_id)
                        || item.get("prompt_id").and_then(Value::as_str) == Some(prompt_id)
                })
            })
    })
}

pub(super) fn api_base_url_display() -> String {
    configured_api_base_url().unwrap_or_else(|_| {
        format!("Invalid {API_BASE_URL_ENV}; use an http://localhost or loopback URL")
    })
}

pub(super) fn api_host_port() -> Result<(String, u16), String> {
    let base_url = configured_api_base_url()?;
    let parsed = Url::parse(&base_url)
        .map_err(|error| format!("{API_BASE_URL_ENV} must be a valid localhost URL: {error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{API_BASE_URL_ENV} must include a localhost host."))?
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("{API_BASE_URL_ENV} must include a valid port."))?;
    Ok((host, port))
}

fn configured_api_base_url() -> Result<String, String> {
    let configured = match std::env::var(API_BASE_URL_ENV) {
        Ok(configured) => configured,
        Err(std::env::VarError::NotPresent) => DEFAULT_API_BASE_URL.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(format!(
                "{API_BASE_URL_ENV} must contain a valid localhost URL."
            ));
        }
    };
    validate_api_base_url(&configured)
}

fn validate_api_base_url(base_url: &str) -> Result<String, String> {
    let parsed = Url::parse(base_url.trim())
        .map_err(|error| format!("{API_BASE_URL_ENV} must be a valid localhost URL: {error}"))?;
    if parsed.scheme() != "http"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || (parsed.path() != "/" && !parsed.path().is_empty())
    {
        return Err(format!(
            "{API_BASE_URL_ENV} must be a plain HTTP localhost URL with no path, credentials, query, or fragment."
        ));
    }
    let loopback_host = match parsed.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if !loopback_host {
        return Err(format!(
            "{API_BASE_URL_ENV} must point to localhost or a loopback IP address."
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

pub(super) async fn discover_backend() -> BackendStatus {
    let result = async {
        let backend = ImageBackend::new()?;
        backend.discover().await
    }
    .await;
    match result {
        Ok(info) => BackendStatus::Available {
            checkpoint: info.checkpoint,
            device: info.device,
            ownership: ServiceOwnership::External,
        },
        Err(error) => BackendStatus::Unavailable(error),
    }
}

pub(super) async fn save_image_async(
    path: std::path::PathBuf,
    bytes: Bytes,
    overwrite: bool,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || save_image(&path, &bytes, overwrite))
        .await
        .map_err(|error| format!("Image export worker failed: {error}"))?
}

pub(super) async fn save_generated_image_async(bytes: Bytes) -> Result<std::path::PathBuf, String> {
    tokio::task::spawn_blocking(move || {
        save_generated_image_in(&generated_images_directory()?, &bytes)
    })
    .await
    .map_err(|error| format!("Automatic image-save worker failed: {error}"))?
}

pub(super) fn generated_images_directory() -> Result<std::path::PathBuf, String> {
    let history_path = crate::history::storage_path()?;
    let data_directory = history_path.parent().ok_or_else(|| {
        "Could not determine the application data directory for generated images.".to_owned()
    })?;
    Ok(data_directory.join("generated-images"))
}

pub(super) fn open_generated_images_directory() -> Result<(), String> {
    let directory = generated_images_directory()?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not open the generated-images folder: {error}"))?;
    let url = url::Url::from_directory_path(&directory)
        .map_err(|()| "Could not convert the generated-images folder path to a local URL.")?;
    webbrowser::open(url.as_str())
        .map_err(|error| format!("Could not open the generated-images folder: {error}"))
}

fn save_generated_image_in(
    directory: &std::path::Path,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Only complete PNG images can be saved automatically.".into());
    }
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Could not create the generated-images folder: {error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Could not determine the current time: {error}"))?
        .as_millis();

    for sequence in 0..u32::MAX {
        let filename = format!("adham-ai-{timestamp}-{sequence:04}.png");
        let path = directory.join(filename);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(automatic_save_io_error(&error)),
        };
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(automatic_save_io_error(&error));
        }
        return Ok(path);
    }

    Err("Could not find an unused filename for the generated image.".into())
}

fn automatic_save_io_error(error: &std::io::Error) -> String {
    if matches!(error.raw_os_error(), Some(28 | 112)) {
        format!("Could not save the generated image: insufficient disk space ({error}).")
    } else {
        format!("Could not save the generated image: {error}")
    }
}

fn save_image(path: &std::path::Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Err("Generated images are PNG files. Choose a destination ending in .png.".into());
    }
    if overwrite {
        std::fs::write(path, bytes)
            .map_err(|error| format!("Could not overwrite the selected file: {error}"))
    } else {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "A file already exists at that path. Confirm overwrite to replace it.".into()
                } else {
                    format!("Could not create the selected file: {error}")
                }
            })?;
        file.write_all(bytes)
            .map_err(|error| format!("Could not write the generated image: {error}"))
    }
}

pub(super) fn validate_prompt(prompt: &str) -> Result<(), String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Enter an image prompt first.".into());
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(format!(
            "Prompts must be no longer than {MAX_PROMPT_BYTES} UTF-8 bytes."
        ));
    }
    Ok(())
}

pub(super) fn validate_steps(steps: &str) -> Result<u32, String> {
    let parsed = steps
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("Steps must be a whole number from {MIN_STEPS} to {MAX_STEPS}."))?;
    if !(MIN_STEPS..=MAX_STEPS).contains(&parsed) {
        return Err(format!(
            "Steps must be between {MIN_STEPS} and {MAX_STEPS}."
        ));
    }
    Ok(parsed)
}

async fn read_limited_body(response: Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!(
            "The ComfyUI response exceeds the {limit}-byte limit."
        ));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| format!("Could not read the ComfyUI response: {error}"))?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(format!(
                "The ComfyUI response exceeds the {limit}-byte limit."
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_decodable_png(bytes: &[u8]) -> Result<(), String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("Could not inspect the generated image: {error}"))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| format!("Could not read the generated image dimensions: {error}"))?;
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS {
        return Err("ComfyUI generated an image larger than 16 megapixels.".into());
    }
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map(|_| ())
        .map_err(|error| format!("ComfyUI returned an unreadable PNG image: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        BackendStatus, DEFAULT_API_BASE_URL, ImageBackend, MediaImage, ServiceOwnership,
        build_workflow, generated_images_directory, history_failure, history_image,
        queue_contains_prompt, save_generated_image_in, save_image, validate_api_base_url,
        validate_decodable_png, validate_prompt, validate_steps,
    };
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct MockReply {
        expected_request: &'static str,
        response_body: Vec<u8>,
        content_type: &'static str,
        status: u16,
        inspect_body: Option<Box<dyn FnOnce(Value) + Send>>,
    }

    impl MockReply {
        fn json(
            expected_request: &'static str,
            body: Value,
            inspect_body: impl FnOnce(Value) + Send + 'static,
        ) -> Self {
            Self {
                expected_request,
                response_body: body.to_string().into_bytes(),
                content_type: "application/json",
                status: 200,
                inspect_body: Some(Box::new(inspect_body)),
            }
        }

        fn image(expected_request: &'static str, bytes: Vec<u8>) -> Self {
            Self {
                expected_request,
                response_body: bytes,
                content_type: "image/png",
                status: 200,
                inspect_body: None,
            }
        }

        fn with_status(mut self, status: u16) -> Self {
            self.status = status;
            self
        }
    }

    fn test_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn mock_server(replies: Vec<MockReply>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for mut reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                let (body_start, content_length) = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert_ne!(count, 0, "client closed before sending request");
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(headers_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        let body_start = headers_end + 4;
                        if request.len() >= body_start + content_length {
                            break (body_start, content_length);
                        }
                    }
                };
                let headers_end = body_start - 4;
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let request_line = headers.lines().next().unwrap_or_default();
                assert!(
                    request_line.contains(reply.expected_request),
                    "expected {}, got {request_line}",
                    reply.expected_request
                );
                let body = if content_length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&request[body_start..body_start + content_length])
                        .unwrap_or_else(|error| panic!("invalid request JSON: {error}"))
                };
                if let Some(inspect_body) = reply.inspect_body.take() {
                    inspect_body(body);
                }
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    if reply.status == 200 { "OK" } else { "Bad Request" },
                    reply.content_type,
                    reply.response_body.len()
                )
                .unwrap();
                stream.write_all(&reply.response_body).unwrap();
            }
        });
        (format!("http://{address}"), server)
    }

    fn run_async<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn checkpoint_node_info() -> Value {
        json!({
            "CheckpointLoaderSimple": {
                "input": {
                    "required": {
                        "ckpt_name": [[ "DreamShaper_8_pruned.safetensors" ]]
                    }
                }
            }
        })
    }

    #[test]
    fn endpoint_defaults_to_localhost_and_rejects_non_loopback_urls() {
        assert_eq!(DEFAULT_API_BASE_URL, "http://127.0.0.1:8188");
        assert_eq!(
            validate_api_base_url("http://localhost:8188/").unwrap(),
            "http://localhost:8188"
        );
        assert!(validate_api_base_url("http://example.com:8188").is_err());
        assert!(validate_api_base_url("https://127.0.0.1:8188").is_err());
        assert!(validate_api_base_url("http://user@127.0.0.1:8188").is_err());
    }

    #[test]
    fn prompt_and_backend_status_validation_are_clear() {
        assert!(validate_prompt(" draw a landscape ").is_ok());
        assert!(validate_prompt(" \n ").is_err());
        assert!(validate_prompt(&"x".repeat(2_001)).is_err());
        assert_eq!(validate_steps("8").unwrap(), 8);
        assert_eq!(validate_steps("1").unwrap(), 1);
        assert_eq!(validate_steps("100").unwrap(), 100);
        assert!(validate_steps("").is_err());
        assert!(validate_steps("abc").is_err());
        assert!(validate_steps("0").is_err());
        assert!(validate_steps("-1").is_err());
        assert!(validate_steps("101").is_err());
        assert!(validate_steps("999999999999999999999999").is_err());
        assert!(!BackendStatus::Checking.is_available());
        assert!(
            !BackendStatus::Available {
                checkpoint: String::new(),
                device: "CPU".into(),
                ownership: ServiceOwnership::External
            }
            .is_available()
        );
        assert!(
            BackendStatus::Available {
                checkpoint: "model.safetensors".into(),
                device: "CPU".into(),
                ownership: ServiceOwnership::External
            }
            .is_available()
        );
        assert!(
            BackendStatus::Unavailable("Not found".into())
                .user_message()
                .contains("Not found")
        );
    }

    #[test]
    fn workflow_uses_the_selected_step_count_and_required_comfy_nodes() {
        let workflow = build_workflow("A mountain lake", "DreamShaper_8_pruned.safetensors", 42, 8);
        assert_eq!(workflow["1"]["class_type"], "CheckpointLoaderSimple");
        assert_eq!(
            workflow["1"]["inputs"]["ckpt_name"],
            "DreamShaper_8_pruned.safetensors"
        );
        assert_eq!(workflow["2"]["class_type"], "CLIPTextEncode");
        assert_eq!(workflow["2"]["inputs"]["text"], "A mountain lake");
        assert_eq!(workflow["3"]["class_type"], "CLIPTextEncode");
        assert_eq!(workflow["4"]["class_type"], "EmptyLatentImage");
        assert_eq!(workflow["4"]["inputs"]["width"], 512);
        assert_eq!(workflow["4"]["inputs"]["height"], 512);
        assert_eq!(workflow["5"]["class_type"], "KSampler");
        assert_eq!(workflow["5"]["inputs"]["steps"], 8);
        assert_eq!(workflow["6"]["class_type"], "VAEDecode");
        assert_eq!(workflow["7"]["class_type"], "SaveImage");
        assert_eq!(workflow["7"]["inputs"]["images"], json!(["6", 0]));
    }

    #[test]
    fn discovers_cpu_device_and_requires_the_existing_checkpoint() {
        let (base_url, server) = mock_server(vec![
            MockReply::json(
                "/system_stats",
                json!({"devices": [{"name": "cpu", "type": "cpu"}]}),
                |_| {},
            ),
            MockReply::json(
                "/object_info/CheckpointLoaderSimple",
                checkpoint_node_info(),
                |_| {},
            ),
        ]);
        let backend = ImageBackend::with_base_url(&base_url).unwrap();
        let info = run_async(backend.discover()).unwrap();
        assert_eq!(info.checkpoint, "DreamShaper_8_pruned.safetensors");
        assert_eq!(info.device, "cpu");
        server.join().unwrap();
    }

    #[test]
    fn submits_workflow_tracks_queue_and_history_then_loads_saved_png() {
        let png = test_png();
        let prompt_id = "test-job-id";
        let history_result = json!({
            (prompt_id): {
                "outputs": {
                    "7": {
                        "images": [{
                            "filename": "result.png",
                            "subfolder": "",
                            "type": "output"
                        }]
                    }
                },
                "status": {"status_str": "success", "completed": true}
            }
        });
        let (base_url, server) = mock_server(vec![
            MockReply::json("/prompt", json!({"prompt_id": prompt_id}), |body| {
                assert_eq!(
                    body["prompt"]["1"]["inputs"]["ckpt_name"],
                    "DreamShaper_8_pruned.safetensors"
                );
                assert_eq!(
                    body["prompt"]["2"]["inputs"]["text"],
                    "A quiet mountain lake"
                );
                assert_eq!(body["prompt"]["4"]["inputs"]["width"], 512);
                assert_eq!(body["prompt"]["5"]["inputs"]["steps"], 8);
                assert_eq!(body["prompt"]["7"]["class_type"], "SaveImage");
                assert!(body["client_id"].as_str().unwrap().starts_with("adham-ai-"));
            }),
            MockReply::json("/history/test-job-id", json!({}), |_| {}),
            MockReply::json(
                "/queue",
                json!({"queue_running": [[1, prompt_id]], "queue_pending": []}),
                |_| {},
            ),
            MockReply::json("/history/test-job-id", history_result, |_| {}),
            MockReply::image("/view?filename=result.png", png.clone()),
        ]);
        let backend = ImageBackend::with_base_url(&base_url).unwrap();
        let active_prompt_id = Arc::new(Mutex::new(None));
        let result = run_async(backend.generate(
            "A quiet mountain lake".into(),
            8,
            active_prompt_id.clone(),
        ))
        .unwrap();
        assert_eq!(result, png);
        assert_eq!(*active_prompt_id.lock().unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn prompt_http_errors_are_reported_with_comfyui_details() {
        let (base_url, server) = mock_server(vec![
            MockReply::json(
                "/prompt",
                json!({"node_errors": {"5": {"errors": [{"message": "Missing model"}]}}}),
                |_| {},
            )
            .with_status(400),
        ]);
        let backend = ImageBackend::with_base_url(&base_url).unwrap();
        let result = run_async(backend.generate(
            "A quiet mountain lake".into(),
            1,
            Arc::new(Mutex::new(None)),
        ));
        let error = result.unwrap_err();
        assert!(error.contains("HTTP 400"));
        assert!(error.contains("Missing model"));
        server.join().unwrap();
    }

    #[test]
    #[ignore = "requires the existing local ComfyUI service and generates a real image"]
    fn real_comfyui_generation_and_automatic_save() {
        let backend = ImageBackend::new().unwrap();
        let active_prompt_id = Arc::new(Mutex::new(None));
        let bytes = run_async(async {
            let info = backend.discover().await?;
            if !info.device.to_ascii_lowercase().contains("cpu") {
                return Err(format!("Expected CPU ComfyUI device, got {}.", info.device));
            }
            backend
                .generate(
                    "A simple blue square on a white background".into(),
                    1,
                    active_prompt_id,
                )
                .await
        })
        .unwrap();
        validate_decodable_png(&bytes).unwrap();

        let output_directory = generated_images_directory().unwrap();
        let path = save_generated_image_in(&output_directory, &bytes).unwrap();
        let saved_bytes = std::fs::read(&path).unwrap();
        validate_decodable_png(&saved_bytes).unwrap();
        assert_eq!(saved_bytes, bytes);
        assert!(path.starts_with(&output_directory));
        eprintln!("Real ComfyUI image saved to {}", path.display());
    }

    #[test]
    fn cancellation_uses_the_server_job_specific_api() {
        let prompt_id = "123e4567-e89b-12d3-a456-426614174000";
        let (base_url, server) = mock_server(vec![MockReply::json(
            "/api/jobs/123e4567-e89b-12d3-a456-426614174000/cancel",
            json!({"cancelled": true}),
            |body| assert!(body.is_null()),
        )]);
        let backend = ImageBackend::with_base_url(&base_url).unwrap();
        assert!(run_async(backend.cancel(prompt_id)).unwrap());
        server.join().unwrap();
    }

    #[test]
    fn history_failure_and_queue_states_are_parsed() {
        assert!(history_image(&json!({"outputs": {}})).is_none());
        assert_eq!(
            history_failure(&json!({"status": {"status_str": "error"}})).as_deref(),
            Some("workflow execution failed")
        );
        assert!(queue_contains_prompt(
            &json!({"queue_pending": [[1, "job-1"]]}),
            "job-1"
        ));
        assert!(!queue_contains_prompt(
            &json!({"queue_pending": []}),
            "job-1"
        ));
    }

    #[test]
    fn media_image_rejects_non_png_backend_output() {
        assert!(MediaImage::new(test_png()).is_ok());
        assert!(MediaImage::new(b"not png".to_vec()).is_err());
    }

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "adham-media-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn automatic_save_creates_output_directory_and_writes_png_bytes() {
        let root = temporary_directory("automatic-save");
        let directory = root.join("generated-images");
        let bytes = test_png();

        let path = save_generated_image_in(&directory, &bytes).unwrap();

        assert!(path.starts_with(&directory));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".png")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automatic_save_uses_unique_names_and_retry_never_overwrites() {
        let root = temporary_directory("unique-save");
        let bytes = test_png();

        let first = save_generated_image_in(&root, &bytes).unwrap();
        let second = save_generated_image_in(&root, &bytes).unwrap();

        assert_ne!(first, second);
        assert_eq!(std::fs::read(first).unwrap(), bytes);
        assert_eq!(std::fs::read(second).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn automatic_save_reports_invalid_images_and_unwritable_output_paths() {
        let root = temporary_directory("failed-save");
        assert!(
            save_generated_image_in(&root.join("not-png"), b"not png")
                .unwrap_err()
                .contains("complete PNG")
        );
        std::fs::create_dir_all(&root).unwrap();
        let obstructing_file = root.join("file-instead-of-directory");
        std::fs::write(&obstructing_file, b"file").unwrap();
        let error =
            save_generated_image_in(&obstructing_file.join("generated-images"), &test_png())
                .unwrap_err();
        assert!(error.contains("Could not create the generated-images folder"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_refuses_existing_files_until_overwrite_is_explicit_and_requires_png() {
        let path = temporary_directory("export").with_extension("png");
        std::fs::write(&path, b"existing").unwrap();
        let error = save_image(&path, b"replacement", false).unwrap_err();
        assert!(error.starts_with("A file already exists"));
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
        save_image(&path, b"replacement", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        let wrong_extension = path.with_extension("jpg");
        assert!(save_image(&wrong_extension, b"png", false).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
