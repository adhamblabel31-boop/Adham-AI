use crate::media::{self, BackendStatus, ServiceOwnership};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 180;
const MIN_STARTUP_TIMEOUT_SECS: u64 = 1;
const MAX_STARTUP_TIMEOUT_SECS: u64 = 600;
const STARTUP_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessState {
    Stopped,
    Checking,
    AlreadyRunning,
    Starting,
    Ready,
    Failed,
    Stopping,
}

impl ProcessState {
    fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Stopped, Self::Checking | Self::Stopping)
                | (
                    Self::Checking,
                    Self::AlreadyRunning
                        | Self::Starting
                        | Self::Ready
                        | Self::Failed
                        | Self::Stopping
                        | Self::Stopped
                )
                | (
                    Self::AlreadyRunning,
                    Self::Ready | Self::Checking | Self::Stopping | Self::Stopped
                )
                | (Self::Starting, Self::Ready | Self::Failed | Self::Stopping)
                | (Self::Ready, Self::Checking | Self::Failed | Self::Stopping)
                | (
                    Self::Failed,
                    Self::Checking | Self::Ready | Self::Stopped | Self::Stopping
                )
                | (Self::Stopping, Self::Stopped | Self::Failed)
        )
    }

    pub(super) fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Checking | Self::AlreadyRunning | Self::Starting | Self::Stopping
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LaunchConfig {
    python: PathBuf,
    main_script: PathBuf,
    working_directory: PathBuf,
    listen_host: String,
    port: u16,
    startup_timeout_secs: u64,
}

impl LaunchConfig {
    fn validate(&self, api_host: &str, api_port: u16) -> Result<(), String> {
        if self.listen_host != api_host || self.port != api_port {
            return Err(
                "The saved ComfyUI launch configuration does not match the current API endpoint. \
                 Review ADHAM_AI_COMFYUI_URL and the local launch configuration."
                    .into(),
            );
        }
        if !(MIN_STARTUP_TIMEOUT_SECS..=MAX_STARTUP_TIMEOUT_SECS)
            .contains(&self.startup_timeout_secs)
        {
            return Err(format!(
                "ComfyUI startup timeout must be between {MIN_STARTUP_TIMEOUT_SECS} and \
                 {MAX_STARTUP_TIMEOUT_SECS} seconds."
            ));
        }
        if !self.python.is_absolute()
            || !self.main_script.is_absolute()
            || !self.working_directory.is_absolute()
        {
            return Err(
                "ComfyUI Python, main.py, and working-directory paths must be absolute.".into(),
            );
        }

        let python = self.python.canonicalize().map_err(|error| {
            format!("The configured ComfyUI Python executable is unavailable: {error}")
        })?;
        if !python.is_file() || !is_executable(&python) {
            return Err("The configured ComfyUI Python path is not an executable file.".into());
        }
        let main_script = self
            .main_script
            .canonicalize()
            .map_err(|error| format!("The configured ComfyUI main.py is unavailable: {error}"))?;
        if main_script.file_name().and_then(|name| name.to_str()) != Some("main.py") {
            return Err("The configured ComfyUI entry point must be main.py.".into());
        }
        let install_directory = main_script
            .parent()
            .ok_or_else(|| "Could not determine the ComfyUI installation directory.".to_owned())?;
        if !install_directory.join("comfy/cli_args.py").is_file() {
            return Err(
                "The configured directory is not a ComfyUI installation (comfy/cli_args.py is missing)."
                    .into(),
            );
        }
        let cli_args = std::fs::read_to_string(install_directory.join("comfy/cli_args.py"))
            .map_err(|error| format!("Could not validate ComfyUI CPU support: {error}"))?;
        if !cli_args.contains("\"--cpu\"") && !cli_args.contains("'--cpu'") {
            return Err(
                "This ComfyUI installation does not advertise the required --cpu option.".into(),
            );
        }
        if !install_directory
            .join("models/checkpoints/DreamShaper_8_pruned.safetensors")
            .is_file()
        {
            return Err(
                "The configured ComfyUI installation does not contain the required \
                 DreamShaper_8_pruned.safetensors checkpoint."
                    .into(),
            );
        }
        if !self.working_directory.is_dir() {
            return Err("The configured ComfyUI working directory does not exist.".into());
        }
        Ok(())
    }
}

trait BackendProbe: Send + Sync {
    fn discover(&self) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>>;
}

struct LiveBackendProbe;

impl BackendProbe for LiveBackendProbe {
    fn discover(&self) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>> {
        Box::pin(media::discover_backend())
    }
}

trait ProcessSpawner: Send + Sync {
    fn spawn(&self, config: &LaunchConfig) -> Result<Child, String>;
}

struct SystemProcessSpawner;

impl ProcessSpawner for SystemProcessSpawner {
    fn spawn(&self, config: &LaunchConfig) -> Result<Child, String> {
        Command::new(&config.python)
            .args(launch_arguments(config))
            .current_dir(&config.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start the configured ComfyUI process: {error}"))
    }
}

fn launch_arguments(config: &LaunchConfig) -> Vec<std::ffi::OsString> {
    [
        config.main_script.as_os_str().to_owned(),
        "--cpu".into(),
        "--listen".into(),
        config.listen_host.as_str().into(),
        "--port".into(),
        config.port.to_string().into(),
    ]
    .into()
}

pub(super) struct MediaProcessManager {
    operation: tokio::sync::Mutex<()>,
    process_state: Mutex<ProcessState>,
    owned_child: Mutex<Option<Child>>,
    owned_startup_timeout_secs: Mutex<Option<u64>>,
    shutting_down: AtomicBool,
    probe: Arc<dyn BackendProbe>,
    spawner: Arc<dyn ProcessSpawner>,
    endpoint_override: Option<(String, u16)>,
    launch_config_override: Option<LaunchConfig>,
    persist_discovery: bool,
}

impl Default for MediaProcessManager {
    fn default() -> Self {
        Self::new(Arc::new(LiveBackendProbe), Arc::new(SystemProcessSpawner))
    }
}

impl MediaProcessManager {
    fn new(probe: Arc<dyn BackendProbe>, spawner: Arc<dyn ProcessSpawner>) -> Self {
        Self {
            operation: tokio::sync::Mutex::new(()),
            process_state: Mutex::new(ProcessState::Stopped),
            owned_child: Mutex::new(None),
            owned_startup_timeout_secs: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            probe,
            spawner,
            endpoint_override: None,
            launch_config_override: None,
            persist_discovery: true,
        }
    }

    pub(super) fn state(&self) -> ProcessState {
        *self
            .process_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) async fn ensure_available(self: Arc<Self>) -> BackendStatus {
        let _operation = self.operation.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return BackendStatus::Unavailable(
                "ComfyUI startup was cancelled because Adham AI is closing.".into(),
            );
        }
        self.transition(ProcessState::Checking);

        let (api_host, api_port) = match self
            .endpoint_override
            .clone()
            .map(Ok)
            .unwrap_or_else(media::api_host_port)
        {
            Ok(endpoint) => endpoint,
            Err(error) => return self.fail(error),
        };
        let initial_status = self.probe.discover().await;
        if initial_status.is_available() {
            let owned = self.owned_process_is_running();
            if owned {
                self.transition(ProcessState::Ready);
                return with_ownership(initial_status, ServiceOwnership::Application);
            }

            self.transition(ProcessState::AlreadyRunning);
            if self.persist_discovery {
                self.discover_and_store_config(api_host, api_port).await;
            }
            self.transition(ProcessState::Ready);
            eprintln!("Reusing the available external ComfyUI service.");
            return with_ownership(initial_status, ServiceOwnership::External);
        }

        if self.owned_process_is_running() {
            self.transition(ProcessState::Starting);
            let timeout_secs = self
                .owned_startup_timeout_secs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS);
            return self.wait_for_owned_process(timeout_secs).await;
        }

        match endpoint_is_listening(api_host.clone(), api_port).await {
            Ok(true) => {
                return self.fail(format!(
                    "{} A service is already listening at the configured endpoint, so Adham AI did \
                     not launch another process.",
                    initial_status.user_message()
                ));
            }
            Ok(false) => {}
            Err(error) => return self.fail(error),
        }

        match auto_start_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return self.fail(
                    "ComfyUI is not available and automatic startup is disabled. Set \
                     ADHAM_AI_COMFYUI_AUTOSTART=1 to enable it."
                        .into(),
                );
            }
            Err(error) => return self.fail(error),
        }

        let config = match self.load_launch_config(api_host.clone(), api_port).await {
            Ok(config) => config,
            Err(error) => return self.fail(error),
        };
        if self.shutting_down.load(Ordering::Acquire) {
            return BackendStatus::Unavailable(
                "ComfyUI startup was cancelled because Adham AI is closing.".into(),
            );
        }
        self.transition(ProcessState::Starting);
        eprintln!(
            "Starting ComfyUI in CPU-only mode at {}:{}.",
            config.listen_host, config.port
        );
        let spawner = self.spawner.clone();
        let spawn_config = config.clone();
        let manager = self.clone();
        let child = match tokio::task::spawn_blocking(move || {
            if manager.shutting_down.load(Ordering::Acquire) {
                return Err("ComfyUI startup was cancelled because Adham AI is closing.".into());
            }
            spawner.spawn(&spawn_config)
        })
        .await
        {
            Ok(Ok(child)) => child,
            Ok(Err(_)) if self.shutting_down.load(Ordering::Acquire) => {
                return BackendStatus::Unavailable(
                    "ComfyUI startup was cancelled because Adham AI is closing.".into(),
                );
            }
            Ok(Err(error)) => return self.fail(error),
            Err(error) => {
                return self.fail(format!("ComfyUI process-launch worker failed: {error}"));
            }
        };
        {
            let mut owned_child = self
                .owned_child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.shutting_down.load(Ordering::Acquire) {
                drop(owned_child);
                terminate_child(child);
                return BackendStatus::Unavailable(
                    "ComfyUI startup was cancelled because Adham AI is closing.".into(),
                );
            }
            *owned_child = Some(child);
        }
        *self
            .owned_startup_timeout_secs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(config.startup_timeout_secs);

        self.wait_for_owned_process(config.startup_timeout_secs)
            .await
    }

    pub(super) async fn check_health(self: Arc<Self>) -> BackendStatus {
        let status = self.probe.discover().await;
        if status.is_available() {
            let ownership = if self.owned_process_is_running() {
                ServiceOwnership::Application
            } else {
                ServiceOwnership::External
            };
            if self.state() == ProcessState::Failed {
                self.transition(ProcessState::Ready);
            }
            return with_ownership(status, ownership);
        }

        match self.owned_process_exit_status() {
            Ok(Some(status)) => {
                eprintln!("The ComfyUI process owned by Adham AI exited unexpectedly ({status}).");
            }
            Ok(None) => {}
            Err(error) => eprintln!("{error}"),
        }
        if self.state() == ProcessState::Ready {
            self.transition(ProcessState::Failed);
        }
        status
    }

    async fn wait_for_owned_process(&self, timeout_secs: u64) -> BackendStatus {
        let timeout = Duration::from_secs(timeout_secs);
        let deadline = Instant::now() + timeout;
        loop {
            if self.shutting_down.load(Ordering::Acquire) {
                return BackendStatus::Unavailable(
                    "ComfyUI startup was cancelled because Adham AI is closing.".into(),
                );
            }
            match self.owned_process_exit_status() {
                Ok(Some(exit_status)) => {
                    let error = format!(
                        "The ComfyUI process started by Adham AI exited before its API became \
                         ready ({exit_status}). Check the configured Python environment, \
                         ComfyUI dependencies, and startup options."
                    );
                    eprintln!("{error}");
                    return self.fail(error);
                }
                Ok(None) => {}
                Err(error) => {
                    return self.fail(format!(
                        "{error}. Adham AI retains the child handle and will stop only that \
                         process when the application exits."
                    ));
                }
            }
            let status = self.probe.discover().await;
            if status.is_available() {
                self.transition(ProcessState::Ready);
                eprintln!("ComfyUI API health check succeeded.");
                return with_ownership(status, ServiceOwnership::Application);
            }
            if Instant::now() >= deadline {
                self.stop_owned_child();
                return self.fail(format!(
                    "ComfyUI did not become ready within {} seconds. Last health check: {}",
                    timeout.as_secs(),
                    status.user_message()
                ));
            }
            tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
        }
    }

    async fn discover_and_store_config(&self, api_host: String, api_port: u16) {
        let config = tokio::task::spawn_blocking(move || {
            discover_running_config(&api_host, api_port).and_then(|config| {
                if let Some(config) = config {
                    config.validate(&api_host, api_port)?;
                    let newly_stored = store_launch_config_if_missing(&config)?;
                    Ok(Some(newly_stored))
                } else {
                    Ok(None)
                }
            })
        })
        .await;
        match config {
            Ok(Ok(Some(true))) => {
                eprintln!(
                    "Validated and saved the launch configuration from the active ComfyUI process."
                );
            }
            Ok(Ok(Some(false))) => {
                eprintln!(
                    "The validated launch configuration already exists; it was left unchanged."
                );
            }
            Ok(Ok(None)) => {
                eprintln!(
                    "The active ComfyUI service is external; no safe launch configuration was discovered."
                );
            }
            Ok(Err(error)) => eprintln!("Could not save an auto-start configuration: {error}"),
            Err(error) => eprintln!("ComfyUI process discovery worker failed: {error}"),
        }
    }

    fn owned_process_is_running(&self) -> bool {
        let mut child = self
            .owned_child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = child.as_mut() else {
            return false;
        };
        match process.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                eprintln!("The ComfyUI child process owned by Adham AI exited ({status}).");
                *child = None;
                *self
                    .owned_startup_timeout_secs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                false
            }
            Err(error) => {
                eprintln!("Could not inspect the owned ComfyUI child process: {error}");
                true
            }
        }
    }

    fn owned_process_exit_status(&self) -> Result<Option<String>, String> {
        let mut child = self
            .owned_child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(process) = child.as_mut() else {
            return Ok(None);
        };
        match process.try_wait() {
            Ok(None) => Ok(None),
            Ok(Some(status)) => {
                *child = None;
                *self
                    .owned_startup_timeout_secs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                Ok(Some(status.to_string()))
            }
            Err(error) => Err(format!(
                "Could not inspect the owned ComfyUI process: {error}"
            )),
        }
    }

    fn fail(&self, error: String) -> BackendStatus {
        self.transition(ProcessState::Failed);
        BackendStatus::Unavailable(error)
    }

    async fn load_launch_config(
        &self,
        api_host: String,
        api_port: u16,
    ) -> Result<LaunchConfig, String> {
        if let Some(config) = self.launch_config_override.clone() {
            return tokio::task::spawn_blocking(move || {
                config.validate(&api_host, api_port)?;
                Ok(config)
            })
            .await
            .map_err(|error| format!("ComfyUI configuration worker failed: {error}"))?;
        }
        load_launch_config(api_host, api_port).await
    }

    fn transition(&self, next: ProcessState) {
        let mut state = self
            .process_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state == next {
            return;
        }
        if !state.can_transition_to(next) {
            eprintln!("Ignoring invalid ComfyUI process-state transition: {state:?} -> {next:?}");
            return;
        }
        *state = next;
    }

    pub(super) fn shutdown_owned(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.transition(ProcessState::Stopping);
        self.stop_owned_child();
        self.transition(ProcessState::Stopped);
    }

    fn stop_owned_child(&self) {
        let child = self
            .owned_child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        *self
            .owned_startup_timeout_secs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        if let Some(child) = child {
            eprintln!("Stopping the exact ComfyUI child process owned by Adham AI.");
            terminate_child(child);
        }
    }
}

impl Drop for MediaProcessManager {
    fn drop(&mut self) {
        self.shutdown_owned();
    }
}

fn terminate_child(mut child: Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            if let Err(error) = child.kill() {
                eprintln!("Could not stop the owned ComfyUI child process: {error}");
            } else if let Err(error) = child.wait() {
                eprintln!("Could not wait for the owned ComfyUI child process: {error}");
            }
        }
    }
}

fn with_ownership(status: BackendStatus, ownership: ServiceOwnership) -> BackendStatus {
    match status {
        BackendStatus::Available {
            checkpoint, device, ..
        } => BackendStatus::Available {
            checkpoint,
            device,
            ownership,
        },
        other => other,
    }
}

async fn load_launch_config(api_host: String, api_port: u16) -> Result<LaunchConfig, String> {
    tokio::task::spawn_blocking(move || {
        let timeout = startup_timeout()?;
        let config = match explicit_environment_config(&api_host, api_port, timeout)? {
            Some(config) => config,
            None => read_stored_launch_config()?,
        };
        config.validate(&api_host, api_port)?;
        Ok(config)
    })
    .await
    .map_err(|error| format!("ComfyUI configuration worker failed: {error}"))?
}

fn startup_timeout() -> Result<u64, String> {
    match std::env::var("ADHAM_AI_COMFYUI_STARTUP_TIMEOUT_SECS") {
        Ok(value) => {
            let timeout = value.parse::<u64>().map_err(|_| {
                "ADHAM_AI_COMFYUI_STARTUP_TIMEOUT_SECS must be an integer.".to_owned()
            })?;
            if !(MIN_STARTUP_TIMEOUT_SECS..=MAX_STARTUP_TIMEOUT_SECS).contains(&timeout) {
                return Err(format!(
                    "ADHAM_AI_COMFYUI_STARTUP_TIMEOUT_SECS must be between \
                     {MIN_STARTUP_TIMEOUT_SECS} and {MAX_STARTUP_TIMEOUT_SECS}."
                ));
            }
            Ok(timeout)
        }
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_STARTUP_TIMEOUT_SECS),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("ADHAM_AI_COMFYUI_STARTUP_TIMEOUT_SECS must contain a valid integer.".into())
        }
    }
}

fn auto_start_enabled() -> Result<bool, String> {
    let value = match std::env::var("ADHAM_AI_COMFYUI_AUTOSTART") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("ADHAM_AI_COMFYUI_AUTOSTART must be a boolean value.".into());
        }
    };
    parse_auto_start(value.as_deref())
}

fn parse_auto_start(value: Option<&str>) -> Result<bool, String> {
    let Some(value) = value else {
        return Ok(true);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err("ADHAM_AI_COMFYUI_AUTOSTART must be true or false.".into()),
    }
}

fn explicit_environment_config(
    api_host: &str,
    api_port: u16,
    startup_timeout_secs: u64,
) -> Result<Option<LaunchConfig>, String> {
    let python = std::env::var_os("ADHAM_AI_COMFYUI_PYTHON");
    let main_script = std::env::var_os("ADHAM_AI_COMFYUI_MAIN");
    if python.is_none() && main_script.is_none() {
        return Ok(None);
    }
    let (Some(python), Some(main_script)) = (python, main_script) else {
        return Err(
            "Set both ADHAM_AI_COMFYUI_PYTHON and ADHAM_AI_COMFYUI_MAIN to enable automatic startup."
                .into(),
        );
    };
    let main_script = PathBuf::from(main_script);
    let working_directory = std::env::var_os("ADHAM_AI_COMFYUI_WORKDIR")
        .map(PathBuf::from)
        .or_else(|| main_script.parent().map(Path::to_path_buf))
        .ok_or_else(|| "Could not determine the ComfyUI working directory.".to_owned())?;
    Ok(Some(LaunchConfig {
        python: PathBuf::from(python),
        main_script,
        working_directory,
        listen_host: api_host.to_owned(),
        port: api_port,
        startup_timeout_secs,
    }))
}

fn read_stored_launch_config() -> Result<LaunchConfig, String> {
    let path = config_path()?;
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "No validated ComfyUI launch configuration is available at {} ({error}). \
             Set ADHAM_AI_COMFYUI_PYTHON and ADHAM_AI_COMFYUI_MAIN to enable automatic startup.",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("The saved ComfyUI launch configuration is invalid: {error}"))
}

fn store_launch_config_if_missing(config: &LaunchConfig) -> Result<bool, String> {
    let path = config_path()?;
    let Some(parent) = path.parent() else {
        return Err("Could not determine the ComfyUI configuration directory.".into());
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        format!("Could not create the ComfyUI configuration directory: {error}")
    })?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|error| {
        format!("Could not serialize the ComfyUI launch configuration: {error}")
    })?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&bytes).map_err(|error| {
                format!("Could not save the ComfyUI launch configuration: {error}")
            })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(format!(
            "Could not save the ComfyUI launch configuration: {error}"
        )),
    }
}

fn config_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|home| home.join("Library").join("Application Support"));

    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        });

    base.map(|path| path.join("adham-ai").join("comfyui-launch.json"))
        .ok_or_else(|| "Could not determine the local application configuration directory.".into())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

async fn endpoint_is_listening(host: String, port: u16) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || {
        let addresses = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|error| format!("Could not check the ComfyUI API endpoint: {error}"))?;
        Ok(addresses.into_iter().any(|address| {
            address.ip().is_loopback()
                && TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
        }))
    })
    .await
    .map_err(|error| format!("ComfyUI endpoint-check worker failed: {error}"))?
}

#[cfg(target_os = "linux")]
fn discover_running_config(api_host: &str, api_port: u16) -> Result<Option<LaunchConfig>, String> {
    use std::collections::HashSet;

    let port = format!("{api_port:04X}");
    let mut socket_inodes = HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let contents = match std::fs::read_to_string(table) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Could not inspect local TCP listeners: {error}")),
        };
        for line in contents.lines().skip(1) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() > 9
                && fields[1]
                    .rsplit(':')
                    .next()
                    .is_some_and(|value| value.eq_ignore_ascii_case(&port))
                && fields[3] == "0A"
            {
                socket_inodes.insert(fields[9].to_owned());
            }
        }
    }
    if socket_inodes.is_empty() {
        return Ok(None);
    }

    let timeout = startup_timeout()?;
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| format!("Could not inspect local ComfyUI processes: {error}"))?;
    'processes: for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let process_dir = entry.path();
        let Some(owns_listener) = process_owns_socket(&process_dir, &socket_inodes) else {
            continue;
        };
        if !owns_listener {
            continue;
        }

        let Ok(command_line) = std::fs::read(process_dir.join("cmdline")) else {
            continue;
        };
        let arguments = command_line
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8_lossy(argument).into_owned())
            .collect::<Vec<_>>();
        let Some(main_argument_index) =
            arguments
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(index, argument)| {
                    Path::new(argument)
                        .file_name()
                        .is_some_and(|name| name == "main.py")
                        .then_some(index)
                })
        else {
            continue;
        };
        let mut has_cpu = false;
        let mut command_args = arguments[main_argument_index + 1..].iter();
        while let Some(argument) = command_args.next() {
            match argument.as_str() {
                "--cpu" => has_cpu = true,
                "--listen" => {
                    if command_args.next().is_none_or(|value| value != api_host) {
                        continue 'processes;
                    }
                }
                "--port" => {
                    if command_args
                        .next()
                        .and_then(|value| value.parse::<u16>().ok())
                        != Some(api_port)
                    {
                        continue 'processes;
                    }
                }
                _ => continue 'processes,
            }
        }
        if !has_cpu {
            continue;
        }
        let working_directory = match std::fs::read_link(process_dir.join("cwd")) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let main_argument = &arguments[main_argument_index];
        let main_script = if Path::new(main_argument).is_absolute() {
            PathBuf::from(main_argument)
        } else {
            working_directory.join(main_argument)
        };
        let python = match std::fs::canonicalize(process_dir.join("exe")) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let candidate = LaunchConfig {
            python,
            main_script,
            working_directory,
            listen_host: api_host.to_owned(),
            port: api_port,
            startup_timeout_secs: timeout,
        };
        if candidate.validate(api_host, api_port).is_ok() {
            candidates.push(candidate);
        }
    }

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err(
            "Multiple validated ComfyUI processes own the configured API port; automatic startup \
             configuration was not selected."
                .into(),
        ),
    }
}

#[cfg(not(target_os = "linux"))]
fn discover_running_config(_: &str, _: u16) -> Result<Option<LaunchConfig>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn process_owns_socket(
    process_dir: &Path,
    socket_inodes: &std::collections::HashSet<String>,
) -> Option<bool> {
    let descriptors = std::fs::read_dir(process_dir.join("fd")).ok()?;
    for descriptor in descriptors.flatten() {
        if let Ok(target) = std::fs::read_link(descriptor.path()) {
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
                && socket_inodes.contains(inode)
            {
                return Some(true);
            }
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::{
        BackendProbe, LaunchConfig, MediaProcessManager, ProcessSpawner, ProcessState, config_path,
        launch_arguments, parse_auto_start, terminate_child,
    };
    use crate::app::{App, Message};
    use crate::media::{BackendStatus, ServiceOwnership};
    use crate::update;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct SequenceProbe {
        statuses: Arc<Mutex<VecDeque<BackendStatus>>>,
        fallback: BackendStatus,
        first_delay: Duration,
        calls: Arc<AtomicUsize>,
    }

    impl BackendProbe for SequenceProbe {
        fn discover(&self) -> Pin<Box<dyn Future<Output = BackendStatus> + Send + '_>> {
            let statuses = self.statuses.clone();
            let fallback = self.fallback.clone();
            let delay = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first_delay
            } else {
                Duration::ZERO
            };
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                statuses.lock().unwrap().pop_front().unwrap_or(fallback)
            })
        }
    }

    struct FakeSpawner {
        launches: Arc<AtomicUsize>,
        child_exits_immediately: bool,
    }

    impl ProcessSpawner for FakeSpawner {
        fn spawn(&self, _: &LaunchConfig) -> Result<Child, String> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let mode = if self.child_exits_immediately {
                "exit"
            } else {
                "sleep"
            };
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "media_process::tests::fake_child_until_killed",
                    "--nocapture",
                ])
                .env("ADHAM_AI_PROCESS_MANAGER_FAKE_CHILD", mode)
                .spawn()
                .map_err(|error| error.to_string())
        }
    }

    fn available_status(owner: ServiceOwnership) -> BackendStatus {
        BackendStatus::Available {
            checkpoint: "DreamShaper_8_pruned.safetensors".into(),
            device: "CPU".into(),
            ownership: owner,
        }
    }

    fn valid_test_config(host: &str, port: u16) -> LaunchConfig {
        let root =
            std::env::temp_dir().join(format!("adham-ai-comfy-test-{}-{port}", std::process::id()));
        std::fs::create_dir_all(root.join("comfy")).unwrap();
        std::fs::create_dir_all(root.join("models/checkpoints")).unwrap();
        std::fs::write(root.join("main.py"), "").unwrap();
        std::fs::write(
            root.join("comfy/cli_args.py"),
            "parser.add_argument(\"--cpu\")",
        )
        .unwrap();
        std::fs::write(
            root.join("models/checkpoints/DreamShaper_8_pruned.safetensors"),
            "",
        )
        .unwrap();
        let config = LaunchConfig {
            python: std::env::current_exe().unwrap(),
            main_script: root.join("main.py"),
            working_directory: root,
            listen_host: host.into(),
            port,
            startup_timeout_secs: 10,
        };
        config
    }

    fn remove_test_install(config: &LaunchConfig) {
        std::fs::remove_dir_all(&config.working_directory).unwrap();
    }

    fn test_manager(
        statuses: Vec<BackendStatus>,
        fallback: BackendStatus,
        host: &str,
        port: u16,
        config: Option<LaunchConfig>,
        launches: Arc<AtomicUsize>,
        child_exits_immediately: bool,
        first_delay: Duration,
    ) -> Arc<MediaProcessManager> {
        let probe = SequenceProbe {
            statuses: Arc::new(Mutex::new(statuses.into())),
            fallback,
            first_delay,
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let spawner = FakeSpawner {
            launches,
            child_exits_immediately,
        };
        let mut manager = MediaProcessManager::new(Arc::new(probe), Arc::new(spawner));
        manager.endpoint_override = Some((host.into(), port));
        manager.launch_config_override = config;
        manager.persist_discovery = false;
        Arc::new(manager)
    }

    fn unused_local_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    fn run_async<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn fake_child_until_killed() {
        if std::env::var("ADHAM_AI_PROCESS_MANAGER_FAKE_CHILD").as_deref() == Ok("sleep") {
            std::thread::sleep(Duration::from_secs(120));
        }
    }

    #[test]
    fn process_states_allow_expected_startup_and_shutdown_transitions() {
        assert!(ProcessState::Stopped.can_transition_to(ProcessState::Checking));
        assert!(ProcessState::Checking.can_transition_to(ProcessState::AlreadyRunning));
        assert!(ProcessState::Checking.can_transition_to(ProcessState::Starting));
        assert!(ProcessState::Starting.can_transition_to(ProcessState::Ready));
        assert!(ProcessState::Starting.can_transition_to(ProcessState::Failed));
        assert!(ProcessState::Ready.can_transition_to(ProcessState::Stopping));
        assert!(ProcessState::Stopping.can_transition_to(ProcessState::Stopped));
        assert!(!ProcessState::Stopped.can_transition_to(ProcessState::Ready));
        assert!(ProcessState::Starting.is_busy());
    }

    #[test]
    fn launch_configuration_rejects_endpoint_mismatch_and_invalid_timeout() {
        let config = LaunchConfig {
            python: PathBuf::from("/python"),
            main_script: PathBuf::from("/comfy/main.py"),
            working_directory: PathBuf::from("/comfy"),
            listen_host: "127.0.0.1".into(),
            port: 8188,
            startup_timeout_secs: 180,
        };
        assert!(config.validate("localhost", 8188).is_err());
        let mut invalid_timeout = config;
        invalid_timeout.startup_timeout_secs = 0;
        assert!(invalid_timeout.validate("127.0.0.1", 8188).is_err());
    }

    #[test]
    fn automatic_start_configuration_defaults_on_and_accepts_explicit_boolean_values() {
        assert!(parse_auto_start(None).unwrap());
        assert!(parse_auto_start(Some("true")).unwrap());
        assert!(parse_auto_start(Some("1")).unwrap());
        assert!(!parse_auto_start(Some("false")).unwrap());
        assert!(!parse_auto_start(Some("0")).unwrap());
        assert!(parse_auto_start(Some("sometimes")).is_err());
    }

    #[test]
    fn launch_command_always_uses_cpu_and_the_configured_local_endpoint() {
        let config = LaunchConfig {
            python: PathBuf::from("/python"),
            main_script: PathBuf::from("/comfy/main.py"),
            working_directory: PathBuf::from("/comfy"),
            listen_host: "127.0.0.1".into(),
            port: 8191,
            startup_timeout_secs: 180,
        };
        assert_eq!(
            launch_arguments(&config),
            [
                "/comfy/main.py",
                "--cpu",
                "--listen",
                "127.0.0.1",
                "--port",
                "8191",
            ]
            .map(std::ffi::OsString::from)
        );
    }

    #[test]
    fn configuration_file_is_scoped_to_the_application_config_directory() {
        assert!(
            config_path()
                .unwrap()
                .ends_with("adham-ai/comfyui-launch.json")
        );
    }

    #[test]
    fn available_external_service_is_reused_without_launching_or_terminating_it() {
        let external_config = valid_test_config("127.0.0.1", 0);
        let mut external_child = FakeSpawner {
            launches: Arc::new(AtomicUsize::new(0)),
            child_exits_immediately: false,
        }
        .spawn(&external_config)
        .unwrap();
        let launches = Arc::new(AtomicUsize::new(0));
        let manager = test_manager(
            vec![available_status(ServiceOwnership::External)],
            available_status(ServiceOwnership::External),
            "127.0.0.1",
            0,
            None,
            launches.clone(),
            false,
            Duration::ZERO,
        );

        let status = run_async(manager.clone().ensure_available());
        assert!(status.is_available());
        assert_eq!(manager.state(), ProcessState::Ready);
        assert_eq!(launches.load(Ordering::SeqCst), 0);

        manager.shutdown_owned();
        assert!(external_child.try_wait().unwrap().is_none());
        terminate_child(external_child);
        remove_test_install(&external_config);
    }

    #[test]
    fn concurrent_start_requests_launch_one_owned_child_and_shutdown_only_that_child() {
        let port = unused_local_port();
        let launches = Arc::new(AtomicUsize::new(0));
        let config = valid_test_config("127.0.0.1", port);
        let manager = test_manager(
            vec![
                BackendStatus::Unavailable("API is not ready".into()),
                available_status(ServiceOwnership::Application),
                available_status(ServiceOwnership::Application),
            ],
            available_status(ServiceOwnership::Application),
            "127.0.0.1",
            port,
            Some(config.clone()),
            launches.clone(),
            false,
            Duration::from_millis(50),
        );

        let (first, second) = run_async(async {
            iced::futures::future::join(
                manager.clone().ensure_available(),
                manager.clone().ensure_available(),
            )
            .await
        });
        assert!(first.is_available());
        assert!(second.is_available());
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(manager.state(), ProcessState::Ready);
        let owned_pid = manager.owned_child.lock().unwrap().as_ref().unwrap().id();

        manager.shutdown_owned();
        assert_eq!(manager.state(), ProcessState::Stopped);
        assert_eq!(manager.owned_child.lock().unwrap().is_none(), true);
        #[cfg(target_os = "linux")]
        assert!(!PathBuf::from(format!("/proc/{owned_pid}")).exists());
        remove_test_install(&config);
    }

    #[test]
    fn child_exit_before_api_readiness_reports_failure_without_restart_loop() {
        let port = unused_local_port();
        let launches = Arc::new(AtomicUsize::new(0));
        let config = valid_test_config("127.0.0.1", port);
        let manager = test_manager(
            vec![BackendStatus::Unavailable("API is not ready".into())],
            BackendStatus::Unavailable("API is not ready".into()),
            "127.0.0.1",
            port,
            Some(config.clone()),
            launches.clone(),
            true,
            Duration::ZERO,
        );

        let status = run_async(manager.clone().ensure_available());
        assert!(!status.is_available());
        assert!(
            status
                .user_message()
                .contains("exited before its API became ready")
        );
        assert_eq!(manager.state(), ProcessState::Failed);
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        manager.shutdown_owned();
        remove_test_install(&config);
    }

    #[test]
    fn startup_timeout_stops_only_the_child_it_launched() {
        let port = unused_local_port();
        let launches = Arc::new(AtomicUsize::new(0));
        let mut config = valid_test_config("127.0.0.1", port);
        config.startup_timeout_secs = 1;
        let manager = test_manager(
            vec![BackendStatus::Unavailable("API is not ready".into())],
            BackendStatus::Unavailable("API is not ready".into()),
            "127.0.0.1",
            port,
            Some(config.clone()),
            launches.clone(),
            false,
            Duration::ZERO,
        );

        let status = run_async(manager.clone().ensure_available());
        assert!(status.user_message().contains("did not become ready"));
        assert_eq!(manager.state(), ProcessState::Failed);
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert!(manager.owned_child.lock().unwrap().is_none());
        manager.shutdown_owned();
        remove_test_install(&config);
    }

    #[test]
    fn cancelling_generation_does_not_stop_the_application_owned_service() {
        let port = unused_local_port();
        let launches = Arc::new(AtomicUsize::new(0));
        let config = valid_test_config("127.0.0.1", port);
        let manager = test_manager(
            vec![
                BackendStatus::Unavailable("API is not ready".into()),
                available_status(ServiceOwnership::Application),
            ],
            available_status(ServiceOwnership::Application),
            "127.0.0.1",
            port,
            Some(config.clone()),
            launches,
            false,
            Duration::ZERO,
        );
        assert!(run_async(manager.clone().ensure_available()).is_available());

        let mut app = App::default();
        app.media_process_manager = manager.clone();
        app.media_generating = true;
        let task = update::update(&mut app, Message::CancelMediaGeneration);
        drop(task);
        assert!(manager.owned_process_is_running());
        manager.shutdown_owned();
        remove_test_install(&config);
    }

    #[test]
    fn periodic_health_failure_does_not_restart_or_replace_the_service() {
        let launches = Arc::new(AtomicUsize::new(0));
        let manager = test_manager(
            vec![
                available_status(ServiceOwnership::External),
                BackendStatus::Unavailable("API stopped responding".into()),
            ],
            BackendStatus::Unavailable("API stopped responding".into()),
            "127.0.0.1",
            0,
            None,
            launches.clone(),
            false,
            Duration::ZERO,
        );

        assert!(run_async(manager.clone().ensure_available()).is_available());
        let status = run_async(manager.clone().check_health());
        assert!(matches!(status, BackendStatus::Unavailable(_)));
        assert_eq!(manager.state(), ProcessState::Failed);
        assert_eq!(launches.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[ignore = "requires the existing local ComfyUI service"]
    fn live_comfyui_is_reused_as_external_and_remains_available_after_manager_shutdown() {
        let manager = Arc::new(MediaProcessManager::default());
        let status = run_async(manager.clone().ensure_available());
        assert!(matches!(
            status,
            BackendStatus::Available {
                ownership: ServiceOwnership::External,
                ..
            }
        ));
        assert!(manager.owned_child.lock().unwrap().is_none());
        manager.shutdown_owned();

        assert!(run_async(crate::media::discover_backend()).is_available());
    }
}
