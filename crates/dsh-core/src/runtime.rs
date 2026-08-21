use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::{FileExt, lock_contended_error};
use reqwest::blocking::Client;
use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{ActivityCode, AppError, AppResult, ApplicationPaths, paths::atomic_write};

pub const NODE_VERSION: &str = "24.19.0";
const NODE_BASES: [&str; 2] = [
    "https://nodejs.org/dist",
    "https://npmmirror.com/mirrors/node",
];
const NPM_REGISTRIES: [&str; 2] = [
    "https://registry.npmjs.org",
    "https://registry.npmmirror.com",
];
const MAX_NODE_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_NODE_EXTRACTED_BYTES: u64 = 1024 * 1024 * 1024;
const NPM_CACHE_PRUNE_THRESHOLD_BYTES: u64 = 1024 * 1024 * 1024;
const NPM_CACHE_STALE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const NPM_CACHE_CHECK_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const NPM_PROCESS_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const NPM_WAITING_AFTER: Duration = Duration::from_secs(30);
const RELEASE_SOURCE_ATTEMPTS: usize = 3;
const RELEASE_NODE_ASSETS: [(&str, &str); 3] = [
    (
        "node-v24.19.0-darwin-arm64.tar.gz",
        "8294b7aa9b03997481c06babf1e8b270c859358f27da57a11509afe537ac381d",
    ),
    (
        "node-v24.19.0-darwin-x64.tar.gz",
        "d1b5e999db158c62fe8f7267a4476b035d8bd93b1a605bac24a3f0dd166e3316",
    ),
    (
        "node-v24.19.0-win-x64.zip",
        "57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentEvent {
    Activity {
        code: ActivityCode,
        values: BTreeMap<String, String>,
    },
    Progress {
        done: u64,
        total: Option<u64>,
    },
    ActivityUpdate {
        values: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DeploymentController {
    cancelled: Arc<AtomicBool>,
    cleanup_error: Arc<Mutex<Option<AppError>>>,
}

impl DeploymentController {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    pub fn cleanup_error(&self) -> Option<AppError> {
        self.cleanup_error
            .lock()
            .expect("deployment cleanup error poisoned")
            .clone()
    }
    fn record_cleanup_error(&self, error: AppError) {
        *self
            .cleanup_error
            .lock()
            .expect("deployment cleanup error poisoned") = Some(error);
    }
    fn check(&self) -> AppResult<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(AppError::new("deploymentCancelled"))
        } else {
            Ok(())
        }
    }
}

struct PartialDownload(PathBuf);

impl Drop for PartialDownload {
    fn drop(&mut self) {
        let _ = remove_owned(&self.0);
    }
}

pub fn installed_version(paths: &ApplicationPaths) -> Option<String> {
    let marker = fs::read_to_string(&paths.version_file).ok()?;
    let manifest = dsh_manifest_version(&paths.dsh_dir)?;
    (marker.trim() == manifest).then_some(manifest)
}

pub fn is_runtime_ready(paths: &ApplicationPaths) -> bool {
    let Some(version) = installed_version(paths) else {
        return false;
    };
    let Ok(expected_node) = resolve_node_version() else {
        return false;
    };
    node_version(paths, &paths.node_dir).as_deref() == Some(expected_node.as_str())
        && dsh_valid(paths, &paths.node_dir, &paths.dsh_dir, &version)
}

pub fn latest_harness_version(controller: &DeploymentController) -> AppResult<String> {
    controller.check()?;
    let client = http_client()?;
    let registries = npm_registries();
    let authority = registries
        .first()
        .ok_or_else(|| AppError::new("versionQueryFailed").detail("no npm registry configured"))?;
    query_registry_version(&client, authority).map(|version| version.to_string())
}

pub fn verify_release_sources() -> AppResult<Vec<String>> {
    let client = http_client()?;
    let mut verified = Vec::new();
    for base in NODE_BASES {
        let manifest_url = format!("{base}/v{NODE_VERSION}/SHASUMS256.txt");
        let manifest = retry_release_source(|| {
            client
                .get(&manifest_url)
                .send()
                .and_then(|response| response.error_for_status())
                .and_then(|response| response.text())
                .map_err(|error| {
                    AppError::new("releaseSourceFailed")
                        .detail(format!("{}: {error}", display_source(&manifest_url)))
                })
        })?;
        for (filename, expected) in RELEASE_NODE_ASSETS {
            let actual = manifest
                .lines()
                .find_map(|line| {
                    let mut fields = line.split_whitespace();
                    let checksum = fields.next()?;
                    let listed = fields.next()?;
                    (listed.trim_start_matches('*') == filename)
                        .then(|| checksum.to_ascii_lowercase())
                })
                .ok_or_else(|| {
                    AppError::new("releaseSourceFailed")
                        .detail(format!("{} does not list {filename}", display_source(base)))
                })?;
            if actual != expected {
                return Err(AppError::new("releaseSourceFailed").detail(format!(
                    "{} checksum mismatch for {filename}",
                    display_source(base)
                )));
            }
        }
        verified.push(format!(
            "Node {NODE_VERSION} release targets via {}",
            display_source(base)
        ));
    }
    let authority = NPM_REGISTRIES[0];
    let version = retry_release_source(|| query_registry_version(&client, authority))?;
    verified.push(format!(
        "Harness latest {version} via {}",
        display_source(authority)
    ));
    for registry in &NPM_REGISTRIES[1..] {
        retry_release_source(|| query_registry_exact_version(&client, registry, &version))?;
        verified.push(format!(
            "Harness {version} mirror via {}",
            display_source(registry)
        ));
    }
    Ok(verified)
}

fn retry_release_source<T>(mut operation: impl FnMut() -> AppResult<T>) -> AppResult<T> {
    for attempt in 1..=RELEASE_SOURCE_ATTEMPTS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if attempt == RELEASE_SOURCE_ATTEMPTS => return Err(error),
            Err(_) => thread::sleep(Duration::from_secs(attempt as u64)),
        }
    }
    unreachable!("release source attempts is non-zero")
}

fn query_registry_version(client: &Client, registry: &str) -> AppResult<Version> {
    validate_network_source(registry)?;
    let url = format!("{registry}/@deepseek-ai%2Fdsh/latest");
    let value = client
        .get(&url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<Value>())
        .map_err(|error| {
            AppError::new("versionQueryFailed")
                .detail(format!("{}: {error}", display_source(registry)))
        })?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .and_then(|raw| Version::parse(raw).ok())
        .ok_or_else(|| {
            AppError::new("versionQueryFailed").detail(format!(
                "{}: invalid version metadata",
                display_source(registry)
            ))
        })?;
    query_registry_exact_version(client, registry, &version)?;
    Ok(version)
}

fn query_registry_exact_version(
    client: &Client,
    registry: &str,
    expected: &Version,
) -> AppResult<()> {
    validate_network_source(registry)?;
    let url = format!("{registry}/@deepseek-ai%2Fdsh/{expected}");
    let value = client
        .get(&url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json::<Value>())
        .map_err(|error| {
            AppError::new("versionQueryFailed").detail(format!(
                "{}: Harness {expected} is unavailable: {error}",
                display_source(registry)
            ))
        })?;
    let actual = value
        .get("version")
        .and_then(Value::as_str)
        .and_then(|raw| Version::parse(raw).ok());
    if actual.as_ref() != Some(expected) {
        return Err(AppError::new("versionQueryFailed").detail(format!(
            "{}: Harness {expected} returned invalid version metadata",
            display_source(registry)
        )));
    }
    Ok(())
}

fn ranked_install_registries(
    client: &Client,
    registries: Vec<String>,
    expected: &Version,
    preferred: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut available = Vec::new();
    for (index, registry) in registries.into_iter().enumerate() {
        validate_network_source(&registry)?;
        let started = Instant::now();
        match query_registry_exact_version(client, &registry, expected) {
            Ok(()) => available.push((started.elapsed(), index, registry)),
            Err(error) => log::warn!(
                "skipping Harness {expected} source {}: {error}",
                display_source(&registry)
            ),
        }
    }
    available.sort_by_key(|(latency, index, registry)| {
        (preferred != Some(registry.as_str()), *latency, *index)
    });
    Ok(available
        .into_iter()
        .map(|(_, _, registry)| registry)
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NpmInstallPhase {
    Preparing,
    Resolving,
    Downloading,
    Writing,
}

struct NpmInstallActivity {
    version: String,
    source: String,
    pending: String,
    phase: NpmInstallPhase,
    resolved: u64,
    packages: u64,
    written: u64,
    last_emitted: Option<(NpmInstallPhase, u64, bool)>,
}

impl NpmInstallActivity {
    fn new(version: &str, registry: &str) -> Self {
        Self {
            version: version.to_owned(),
            source: display_source(registry),
            pending: String::new(),
            phase: NpmInstallPhase::Preparing,
            resolved: 0,
            packages: 0,
            written: 0,
            last_emitted: None,
        }
    }

    fn observe(&mut self, output: &str, idle: Duration, notify: &impl Fn(DeploymentEvent)) {
        self.pending.push_str(output);
        if let Some(end) = self.pending.rfind('\n') {
            let tail = self.pending.split_off(end + 1);
            let complete = std::mem::replace(&mut self.pending, tail);
            for line in complete.lines() {
                if line.contains("silly fetch manifest ") {
                    self.resolved = self.resolved.saturating_add(1);
                    self.phase = self.phase.max(NpmInstallPhase::Resolving);
                }
                if line.contains("silly placeDep ") {
                    self.resolved = self.resolved.saturating_add(1);
                    self.phase = self.phase.max(NpmInstallPhase::Resolving);
                }
                if (line.contains("http fetch ") || line.contains("http cache "))
                    && line.contains(".tgz")
                {
                    self.packages = self.packages.saturating_add(1);
                    self.phase = self.phase.max(NpmInstallPhase::Downloading);
                }
                if line.contains("silly ADD ") {
                    self.written = self.written.saturating_add(1);
                    self.phase = NpmInstallPhase::Writing;
                }
            }
        }

        let waiting = idle >= NPM_WAITING_AFTER;
        let processed = match self.phase {
            NpmInstallPhase::Preparing => 0,
            NpmInstallPhase::Resolving => self.resolved,
            NpmInstallPhase::Downloading => self.packages,
            NpmInstallPhase::Writing => self.written,
        };
        let current = (self.phase, processed, waiting);
        if self.last_emitted == Some(current) {
            return;
        }

        let code = match self.phase {
            NpmInstallPhase::Preparing => ActivityCode::InstallingHarness,
            NpmInstallPhase::Resolving => ActivityCode::ResolvingHarnessDependencies,
            NpmInstallPhase::Downloading => ActivityCode::DownloadingHarnessPackages,
            NpmInstallPhase::Writing => ActivityCode::WritingHarnessRuntime,
        };
        let values = BTreeMap::from([
            ("version".to_owned(), self.version.clone()),
            ("source".to_owned(), self.source.clone()),
            ("processed".to_owned(), processed.to_string()),
            (
                "status".to_owned(),
                if waiting { "waiting" } else { "active" }.to_owned(),
            ),
        ]);
        if self
            .last_emitted
            .is_none_or(|(previous, _, _)| previous != self.phase)
        {
            notify(DeploymentEvent::Activity { code, values });
        } else {
            notify(DeploymentEvent::ActivityUpdate { values });
        }
        self.last_emitted = Some(current);
    }
}

pub fn deploy_runtime(
    paths: &ApplicationPaths,
    force: bool,
    target_version: Option<&str>,
    controller: &DeploymentController,
    notify: impl Fn(DeploymentEvent),
) -> AppResult<String> {
    paths.ensure_dirs()?;
    activity(&notify, ActivityCode::WaitingForLock, []);
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.deployment_lock)?;
    acquire_lock(&lock_file, controller)?;
    let result = (|| {
        recover_interrupted(paths)?;
        recover_valid_previous(paths)?;
        prune_stale_npm_cache(paths);
        activity(&notify, ActivityCode::CheckingRuntime, []);
        if !force && is_runtime_ready(paths) {
            return installed_version(paths)
                .ok_or_else(|| AppError::new("runtimeValidationFailed"));
        }
        let previous_version = installed_version(paths);
        let previous_was_valid = previous_version.as_deref().is_some_and(|version| {
            runtime_pair_valid(paths, &paths.node_dir, &paths.dsh_dir, version)
        });
        let version = match target_version {
            Some(value) => Version::parse(value)
                .map_err(|_| AppError::new("runtimeVersionInvalid").value("version", value))?
                .to_string(),
            None => {
                activity(&notify, ActivityCode::ResolvingVersion, []);
                latest_harness_version(controller)?
            }
        };
        let node_previous = ensure_node(paths, controller, &notify)?;
        notify(DeploymentEvent::Progress {
            done: 0,
            total: None,
        });
        let staging = match install_harness(paths, &version, controller, &notify) {
            Ok(staging) => staging,
            Err(error) => {
                if previous_was_valid && let Some(previous) = node_previous.as_deref() {
                    rollback_directory(&paths.node_dir, Some(previous))?;
                }
                return Err(error);
            }
        };
        activity(
            &notify,
            ActivityCode::ActivatingHarness,
            [("version", version.clone())],
        );
        let dsh_previous = match publish_directory(&staging, &paths.dsh_dir) {
            Ok(previous) => previous,
            Err(error) => {
                if previous_was_valid && let Some(previous) = node_previous.as_deref() {
                    rollback_directory(&paths.node_dir, Some(previous))?;
                }
                return Err(error);
            }
        };
        if !dsh_valid(paths, &paths.node_dir, &paths.dsh_dir, &version) {
            rollback_directory(&paths.dsh_dir, dsh_previous.as_deref())?;
            if previous_was_valid && let Some(previous) = node_previous.as_deref() {
                rollback_directory(&paths.node_dir, Some(previous))?;
            }
            return Err(
                AppError::new("runtimeValidationFailed").value("component", "DeepSeek Harness")
            );
        }
        if let Err(error) = atomic_write(&paths.version_file, format!("{version}\n").as_bytes()) {
            rollback_directory(&paths.dsh_dir, dsh_previous.as_deref())?;
            if previous_was_valid && let Some(previous) = node_previous.as_deref() {
                rollback_directory(&paths.node_dir, Some(previous))?;
            }
            if let Some(old) = previous_version {
                let _ = atomic_write(&paths.version_file, format!("{old}\n").as_bytes());
            }
            return Err(error);
        }
        Ok(version)
    })();
    let _ = FileExt::unlock(&lock_file);
    result
}

fn ensure_node(
    paths: &ApplicationPaths,
    controller: &DeploymentController,
    notify: &impl Fn(DeploymentEvent),
) -> AppResult<Option<PathBuf>> {
    let version = resolve_node_version()?;
    if node_version(paths, &paths.node_dir).as_deref() == Some(version.as_str()) {
        return Ok(None);
    }
    let filename = node_filename(&version)?;
    let checksum = node_checksum(&filename)?;
    let archive = paths.cache_dir.join(&filename);
    activity(
        notify,
        ActivityCode::DownloadingNode,
        [("version", version.clone())],
    );
    download_verified(
        &node_bases()
            .iter()
            .map(|base| format!("{base}/v{version}/{filename}"))
            .collect::<Vec<_>>(),
        &archive,
        &checksum,
        controller,
        notify,
    )?;
    activity(
        notify,
        ActivityCode::VerifyingNode,
        [("version", version.clone())],
    );
    let staging = paths
        .runtime_dir
        .join(format!("node.staging-{}", Uuid::new_v4()));
    extract_node(&archive, &staging)?;
    if node_version(paths, &staging).as_deref() != Some(version.as_str()) {
        remove_owned(&staging)?;
        return Err(AppError::new("runtimeValidationFailed").value("component", "Node.js"));
    }
    let previous = publish_directory(&staging, &paths.node_dir)?;
    if node_version(paths, &paths.node_dir).as_deref() != Some(version.as_str()) {
        rollback_directory(&paths.node_dir, previous.as_deref())?;
        return Err(AppError::new("runtimeValidationFailed").value("component", "Node.js"));
    }
    Ok(previous)
}

fn install_harness(
    paths: &ApplicationPaths,
    version: &str,
    controller: &DeploymentController,
    notify: &impl Fn(DeploymentEvent),
) -> AppResult<PathBuf> {
    activity(
        notify,
        ActivityCode::CheckingSources,
        [("version", version.to_owned())],
    );
    let staging = paths
        .runtime_dir
        .join(format!("dsh.staging-{}", Uuid::new_v4()));
    let client = http_client()?;
    let expected = Version::parse(version)
        .map_err(|_| AppError::new("runtimeVersionInvalid").value("version", version))?;
    let preferred_registry = fs::read_to_string(paths.cache_dir.join("npm.registry")).ok();
    let registries = ranked_install_registries(
        &client,
        npm_registries(),
        &expected,
        preferred_registry.as_deref().map(str::trim),
    )?;
    for registry in registries {
        controller.check()?;
        let _ = remove_owned(&staging);
        fs::create_dir(&staging)?;
        atomic_write(
            &staging.join("package.json"),
            b"{\"name\":\"dsh-runtime\",\"private\":true}\n",
        )?;
        activity(
            notify,
            ActivityCode::InstallingHarness,
            [
                ("version", version.to_owned()),
                ("source", display_source(&registry)),
            ],
        );
        let npm = npm_cli(&paths.node_dir);
        let mut command = Command::new(&paths.node_bin);
        command
            .arg(npm)
            .arg("install")
            .arg(format!("@deepseek-ai/dsh@{version}"))
            .args([
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--package-lock=false",
                "--prefer-offline",
                "--fetch-retries=2",
                "--fetch-retry-factor=2",
                "--fetch-retry-mintimeout=1000",
                "--fetch-retry-maxtimeout=10000",
                "--fetch-timeout=60000",
            ])
            .arg(format!("--cache={}", paths.cache_dir.join("npm").display()));
        command.arg("--loglevel=silly");
        isolated_command(&mut command, paths);
        command
            .env("NPM_CONFIG_REGISTRY", &registry)
            .current_dir(&staging);
        mark_npm_cache_used(paths);
        let mut npm_activity = NpmInstallActivity::new(version, &registry);
        let install_result = run_logged(
            &mut command,
            &paths.install_log,
            ProcessTimeouts {
                total: NPM_PROCESS_TOTAL_TIMEOUT,
            },
            controller,
            |output, idle| npm_activity.observe(output, idle, notify),
        );
        match install_result {
            Ok(()) => {
                activity(
                    notify,
                    ActivityCode::ValidatingHarness,
                    [("version", version.to_owned())],
                );
                fix_spawn_helper(&staging);
                if dsh_valid(paths, &paths.node_dir, &staging, version) {
                    if let Err(error) = atomic_write(
                        &paths.cache_dir.join("npm.registry"),
                        format!("{registry}\n").as_bytes(),
                    ) {
                        log::warn!("preferred npm registry could not be saved: {error}");
                    }
                    return Ok(staging);
                }
            }
            Err(error) if error.code == "deploymentCancelled" => return Err(error),
            // Registry fallback helps transport failures, but it cannot make
            // npm's local peer-dependency solver faster. Do not repeat a full
            // 30-minute resolution timeout against an equivalent source.
            Err(error) if error.code == "processTimeout" => break,
            Err(_) => {}
        }
    }
    let _ = remove_owned(&staging);
    Err(AppError::new("installFailed").value("log", paths.install_log.display()))
}

fn download_verified(
    urls: &[String],
    destination: &Path,
    checksum: &str,
    controller: &DeploymentController,
    notify: &impl Fn(DeploymentEvent),
) -> AppResult<()> {
    if destination.exists() || destination.is_symlink() {
        if destination.is_file() && !destination.is_symlink() {
            let size = destination.metadata()?.len();
            if size <= MAX_NODE_ARCHIVE_BYTES && sha256(destination)? == checksum {
                notify(DeploymentEvent::Progress {
                    done: size,
                    total: Some(size),
                });
                return Ok(());
            }
        }
        remove_owned(destination)?;
    }
    let partial = destination.with_extension(format!(
        "{}part",
        destination
            .extension()
            .and_then(|item| item.to_str())
            .unwrap_or_default()
    ));
    let _partial_cleanup = PartialDownload(partial.clone());
    let client = http_client()?;
    let deadline = Instant::now()
        + Duration::from_secs(env_seconds("DSH_DESKTOP_DOWNLOAD_TIMEOUT_SECONDS", 600));
    let mut errors = Vec::new();
    for attempt in 1..=2 {
        for url in urls {
            controller.check()?;
            if Instant::now() >= deadline {
                break;
            }
            match download_once(&client, url, &partial, deadline, controller, notify) {
                Ok(()) => {
                    let actual = sha256(&partial)?;
                    if actual == checksum {
                        fs::rename(&partial, destination)?;
                        return Ok(());
                    }
                    let _ = fs::remove_file(&partial);
                    errors.push(format!(
                        "{} attempt {attempt}: checksum mismatch",
                        display_source(url)
                    ));
                }
                Err(error) => {
                    let terminal = matches!(
                        error.code.as_str(),
                        "downloadTimedOut" | "downloadTooLarge" | "deploymentCancelled"
                    );
                    errors.push(format!(
                        "{} attempt {attempt}: {}",
                        display_source(url),
                        error
                            .safe_detail
                            .clone()
                            .unwrap_or_else(|| error.code.clone())
                    ));
                    if terminal {
                        let _ = remove_owned(&partial);
                        return Err(error);
                    }
                }
            }
        }
    }
    let _ = remove_owned(&partial);
    if Instant::now() >= deadline {
        return Err(AppError::new("downloadTimedOut"));
    }
    Err(AppError::new("downloadFailed").detail(errors.join("; ")))
}

fn download_once(
    client: &Client,
    url: &str,
    partial: &Path,
    deadline: Instant,
    controller: &DeploymentController,
    notify: &impl Fn(DeploymentEvent),
) -> AppResult<()> {
    validate_network_source(url)?;
    if Instant::now() >= deadline {
        return Err(AppError::new("downloadTimedOut"));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    remove_owned(partial)?;
    let mut response = client
        .get(url)
        .timeout(remaining)
        .send()
        .and_then(|item| item.error_for_status())
        .map_err(|error| {
            if error.is_timeout() || Instant::now() >= deadline {
                AppError::new("downloadTimedOut")
            } else {
                AppError::new("downloadFailed").detail(error.to_string())
            }
        })?;
    let total = response.content_length();
    if total.is_some_and(|size| size > MAX_NODE_ARCHIVE_BYTES) {
        return Err(AppError::new("downloadTooLarge"));
    }
    let mut file = File::create(partial)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut done = 0;
    loop {
        controller.check()?;
        if Instant::now() >= deadline {
            return Err(AppError::new("downloadTimedOut"));
        }
        let count = response.read(&mut buffer).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut || Instant::now() >= deadline {
                AppError::new("downloadTimedOut")
            } else {
                AppError::io("downloadFailed", &error)
            }
        })?;
        if Instant::now() >= deadline {
            return Err(AppError::new("downloadTimedOut"));
        }
        if count == 0 {
            break;
        }
        if done + count as u64 > MAX_NODE_ARCHIVE_BYTES {
            return Err(AppError::new("downloadTooLarge"));
        }
        file.write_all(&buffer[..count])?;
        done += count as u64;
        notify(DeploymentEvent::Progress { done, total });
    }
    file.sync_all()?;
    Ok(())
}

fn extract_node(archive: &Path, destination: &Path) -> AppResult<()> {
    remove_owned(destination)?;
    fs::create_dir(destination)?;
    let result = if archive.extension().and_then(|value| value.to_str()) == Some("zip") {
        extract_zip(archive, destination)
    } else {
        extract_tar(archive, destination)
    };
    if let Err(error) = result {
        let _ = remove_owned(destination);
        return Err(error);
    }
    let children: Vec<_> = fs::read_dir(destination)?.collect::<Result<_, _>>()?;
    if children.len() != 1
        || !children[0].file_type()?.is_dir()
        || children[0].file_type()?.is_symlink()
    {
        return Err(AppError::new("nodeArchiveInvalid"));
    }
    let top = children[0].path();
    for child in fs::read_dir(&top)? {
        let child = child?;
        fs::rename(child.path(), destination.join(child.file_name()))?;
    }
    fs::remove_dir(top)?;
    Ok(())
}

fn extract_tar(archive: &Path, destination: &Path) -> AppResult<()> {
    let decoder = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut bundle = tar::Archive::new(decoder);
    let mut extracted_bytes = 0;
    for item in bundle
        .entries()
        .map_err(|error| AppError::new("nodeArchiveInvalid").detail(error.to_string()))?
    {
        let mut item =
            item.map_err(|error| AppError::new("nodeArchiveInvalid").detail(error.to_string()))?;
        let entry_type = item.header().entry_type();
        if !(entry_type.is_file()
            || entry_type.is_dir()
            || entry_type.is_symlink()
            || entry_type.is_hard_link())
        {
            return Err(AppError::new("nodeArchiveUnsafe"));
        }
        if entry_type.is_file() {
            include_extracted_bytes(&mut extracted_bytes, item.size())?;
        }
        let path = item
            .path()
            .map_err(|error| AppError::new("nodeArchiveUnsafe").detail(error.to_string()))?
            .into_owned();
        validate_archive_path(&path)?;
        if let Some(link) = item
            .link_name()
            .map_err(|error| AppError::new("nodeArchiveUnsafe").detail(error.to_string()))?
        {
            validate_link(&path, &link)?;
        }
        if !item
            .unpack_in(destination)
            .map_err(|error| AppError::new("nodeArchiveUnsafe").detail(error.to_string()))?
        {
            return Err(AppError::new("nodeArchiveUnsafe"));
        }
    }
    Ok(())
}

fn extract_zip(archive: &Path, destination: &Path) -> AppResult<()> {
    let mut bundle = zip::ZipArchive::new(File::open(archive)?)
        .map_err(|error| AppError::new("nodeArchiveInvalid").detail(error.to_string()))?;
    let mut extracted_bytes = 0;
    for index in 0..bundle.len() {
        let mut item = bundle
            .by_index(index)
            .map_err(|error| AppError::new("nodeArchiveInvalid").detail(error.to_string()))?;
        let path = item
            .enclosed_name()
            .ok_or_else(|| AppError::new("nodeArchiveUnsafe"))?;
        validate_archive_path(&path)?;
        if item
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(AppError::new("nodeArchiveUnsafe"));
        }
        let output = destination.join(path);
        if item.is_dir() {
            fs::create_dir_all(&output)?;
        } else {
            include_extracted_bytes(&mut extracted_bytes, item.size())?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = File::create(&output)?;
            std::io::copy(&mut item, &mut file)?;
        }
    }
    Ok(())
}

fn include_extracted_bytes(total: &mut u64, size: u64) -> AppResult<()> {
    *total = total
        .checked_add(size)
        .ok_or_else(|| AppError::new("nodeArchiveTooLarge"))?;
    if *total > MAX_NODE_EXTRACTED_BYTES {
        return Err(AppError::new("nodeArchiveTooLarge"));
    }
    Ok(())
}

fn validate_archive_path(path: &Path) -> AppResult<()> {
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        Err(AppError::new("nodeArchiveUnsafe").value("entry", path.display()))
    } else {
        Ok(())
    }
}

fn validate_link(path: &Path, link: &Path) -> AppResult<()> {
    if link.is_absolute() {
        return Err(AppError::new("nodeArchiveUnsafe"));
    }
    let root = path
        .components()
        .next()
        .and_then(|part| match part {
            Component::Normal(value) => Some(value.to_owned()),
            _ => None,
        })
        .ok_or_else(|| AppError::new("nodeArchiveUnsafe"))?;
    let link_starts_at_root = link
        .components()
        .next()
        .is_some_and(|part| matches!(part, Component::Normal(value) if value == root));
    let combined = if link_starts_at_root {
        link.to_owned()
    } else {
        path.parent().unwrap_or_else(|| Path::new("")).join(link)
    };
    let mut normalized = Vec::new();
    for part in combined.components() {
        match part {
            Component::ParentDir => {
                if normalized.pop().is_none() {
                    return Err(AppError::new("nodeArchiveUnsafe"));
                }
            }
            Component::Normal(value) => normalized.push(value.to_owned()),
            Component::CurDir => {}
            _ => return Err(AppError::new("nodeArchiveUnsafe")),
        }
    }
    if normalized.first() == Some(&root) {
        Ok(())
    } else {
        Err(AppError::new("nodeArchiveUnsafe"))
    }
}

fn recover_interrupted(paths: &ApplicationPaths) -> AppResult<()> {
    for name in ["node", "dsh"] {
        let active = paths.runtime_dir.join(name);
        let previous = paths.runtime_dir.join(format!("{name}.previous"));
        if !active.exists() && previous.is_dir() && !previous.is_symlink() {
            fs::rename(previous, active)?;
        }
    }
    for entry in fs::read_dir(&paths.runtime_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("node.staging-")
            || name.starts_with("dsh.staging-")
            || name.contains(".failed-")
        {
            remove_owned(&entry.path())?;
        }
    }
    Ok(())
}

fn prune_stale_npm_cache(paths: &ApplicationPaths) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Err(error) = prune_npm_cache_at(
        &paths.cache_dir,
        now,
        NPM_CACHE_PRUNE_THRESHOLD_BYTES,
        NPM_CACHE_STALE_AFTER,
    ) {
        log::warn!("stale npm cache cleanup was skipped: {error}");
    }
}

fn prune_npm_cache_at(
    cache_dir: &Path,
    now: u64,
    threshold: u64,
    stale_after: Duration,
) -> AppResult<bool> {
    for entry in fs::read_dir(cache_dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("npm.expired-")
        {
            remove_owned(&entry.path())?;
        }
    }

    let npm_cache = cache_dir.join("npm");
    if !npm_cache.exists() && !npm_cache.is_symlink() {
        return Ok(false);
    }
    let usage_marker = cache_dir.join("npm.last-used");
    let check_marker = cache_dir.join("npm.last-prune-check");
    let last_used = fs::read_to_string(&usage_marker)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let Some(last_used) = last_used else {
        atomic_write(&usage_marker, format!("{now}\n").as_bytes())?;
        atomic_write(&check_marker, format!("{now}\n").as_bytes())?;
        return Ok(false);
    };
    if now.saturating_sub(last_used) < stale_after.as_secs() {
        return Ok(false);
    }
    let checked_recently = fs::read_to_string(&check_marker)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_some_and(|checked| now.saturating_sub(checked) < NPM_CACHE_CHECK_INTERVAL.as_secs());
    if checked_recently {
        return Ok(false);
    }
    atomic_write(&check_marker, format!("{now}\n").as_bytes())?;
    if owned_tree_size(&npm_cache)? < threshold {
        return Ok(false);
    }

    let expired = cache_dir.join(format!("npm.expired-{}", Uuid::new_v4()));
    fs::rename(&npm_cache, &expired)?;
    if let Err(error) = remove_owned(&expired) {
        log::warn!("expired npm cache will be retried later: {error}");
    }
    atomic_write(&usage_marker, format!("{now}\n").as_bytes())?;
    Ok(true)
}

fn owned_tree_size(path: &Path) -> AppResult<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    let mut total = 0_u64;
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = entry.map_err(|error| {
            AppError::new("readDirectoryFailed")
                .detail(error.to_string())
                .value("path", path.display())
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_file() || metadata.file_type().is_symlink() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn mark_npm_cache_used(paths: &ApplicationPaths) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Err(error) = atomic_write(
        &paths.cache_dir.join("npm.last-used"),
        format!("{now}\n").as_bytes(),
    ) {
        log::warn!("npm cache usage marker could not be updated: {error}");
    }
}

fn recover_valid_previous(paths: &ApplicationPaths) -> AppResult<()> {
    let node_previous = paths.runtime_dir.join("node.previous");
    let dsh_previous = paths.runtime_dir.join("dsh.previous");
    let active_version = dsh_manifest_version(&paths.dsh_dir);
    if active_version
        .as_deref()
        .is_some_and(|version| runtime_pair_valid(paths, &paths.node_dir, &paths.dsh_dir, version))
    {
        if installed_version(paths).is_none() {
            let version = active_version.expect("validated active version");
            atomic_write(&paths.version_file, format!("{version}\n").as_bytes())?;
        }
        return Ok(());
    }

    let previous_version = dsh_manifest_version(&dsh_previous);
    let candidates = [
        (
            &node_previous,
            &paths.dsh_dir,
            active_version.as_deref(),
            true,
            false,
        ),
        (
            &paths.node_dir,
            &dsh_previous,
            previous_version.as_deref(),
            false,
            true,
        ),
        (
            &node_previous,
            &dsh_previous,
            previous_version.as_deref(),
            true,
            true,
        ),
    ];
    for (node, dsh, version, restore_node, restore_dsh) in candidates {
        let Some(version) = version else { continue };
        if !runtime_pair_valid(paths, node, dsh, version) {
            continue;
        }
        if restore_node {
            rollback_directory(&paths.node_dir, Some(&node_previous))?;
        }
        if restore_dsh {
            rollback_directory(&paths.dsh_dir, Some(&dsh_previous))?;
        }
        atomic_write(&paths.version_file, format!("{version}\n").as_bytes())?;
        return Ok(());
    }
    Ok(())
}

fn publish_directory(staging: &Path, active: &Path) -> AppResult<Option<PathBuf>> {
    let previous = active.with_file_name(format!(
        "{}.previous",
        active
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("runtime")
    ));
    remove_owned(&previous)?;
    let moved = active.exists();
    if moved {
        fs::rename(active, &previous)?;
    }
    if let Err(error) = fs::rename(staging, active) {
        if moved && !active.exists() {
            let _ = fs::rename(&previous, active);
        }
        return Err(error.into());
    }
    Ok(moved.then_some(previous))
}

fn rollback_directory(active: &Path, previous: Option<&Path>) -> AppResult<()> {
    let failed = active.with_file_name(format!(
        "{}.failed-{}",
        active
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("runtime"),
        Uuid::new_v4()
    ));
    if active.exists() {
        fs::rename(active, &failed)?;
    }
    if let Some(previous) = previous
        && previous.exists()
    {
        fs::rename(previous, active)?;
    }
    remove_owned(&failed)
}

fn remove_owned(path: &Path) -> AppResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?
        }
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn node_version(paths: &ApplicationPaths, node_dir: &Path) -> Option<String> {
    let executable = node_executable(node_dir);
    let mut command = Command::new(executable);
    command.arg("--version");
    isolated_command(&mut command, paths);
    configure_process_group(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .strip_prefix('v')?
        .to_owned();
    Version::parse(&value).ok().map(|_| value)
}

fn dsh_valid(paths: &ApplicationPaths, node_dir: &Path, dsh_dir: &Path, version: &str) -> bool {
    if dsh_manifest_version(dsh_dir).as_deref() != Some(version) {
        return false;
    }
    let binary = dsh_dir.join("node_modules/@deepseek-ai/dsh/lib/bin.js");
    let mut command = Command::new(node_executable(node_dir));
    command.arg(binary).arg("--version");
    isolated_command(&mut command, paths);
    configure_process_group(&mut command);
    command.output().is_ok_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == version
    })
}

fn runtime_pair_valid(
    paths: &ApplicationPaths,
    node_dir: &Path,
    dsh_dir: &Path,
    version: &str,
) -> bool {
    node_version(paths, node_dir).is_some() && dsh_valid(paths, node_dir, dsh_dir, version)
}

fn dsh_manifest_version(dir: &Path) -> Option<String> {
    let value: Value = serde_json::from_slice(
        &fs::read(dir.join("node_modules/@deepseek-ai/dsh/package.json")).ok()?,
    )
    .ok()?;
    let version = value.get("version")?.as_str()?;
    Version::parse(version).ok().map(|_| version.to_owned())
}

#[derive(Debug, Clone, Copy)]
struct ProcessTimeouts {
    total: Duration,
}

fn run_logged(
    command: &mut Command,
    log_path: &Path,
    timeouts: ProcessTimeouts,
    controller: &DeploymentController,
    mut observe: impl FnMut(&str, Duration),
) -> AppResult<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let output_start = log.metadata()?.len();
    let mut log_reader = File::open(log_path)?;
    log_reader.seek(SeekFrom::Start(output_start))?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .stdin(Stdio::null());
    configure_process_group(command);
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let job = WindowsProcessGuard::attach(&child)?;
    let started = Instant::now();
    let mut last_output = started;
    let mut last_observation = started;
    loop {
        let output = read_appended_log(&mut log_reader)?;
        if !output.is_empty() {
            last_output = Instant::now();
        }
        let now = Instant::now();
        if !output.is_empty() || now.duration_since(last_observation) >= Duration::from_secs(1) {
            observe(&output, now.duration_since(last_output));
            last_observation = now;
        }
        if controller.check().is_err() || now.duration_since(started) >= timeouts.total {
            #[cfg(unix)]
            stop_unix_command_tree(&mut child, controller)?;
            #[cfg(windows)]
            stop_windows_command_tree(&mut child, &job, controller)?;
            return Err(AppError::new(
                if controller.cancelled.load(Ordering::SeqCst) {
                    "deploymentCancelled"
                } else {
                    "processTimeout"
                },
            ));
        }
        #[cfg(windows)]
        job.observe()?;
        if let Some(status) = child.try_wait()? {
            let output = read_appended_log(&mut log_reader)?;
            if !output.is_empty() {
                observe(&output, Duration::ZERO);
            }
            #[cfg(unix)]
            stop_unix_command_tree(&mut child, controller)?;
            #[cfg(windows)]
            stop_windows_command_tree(&mut child, &job, controller)?;
            return if status.success() {
                Ok(())
            } else {
                Err(AppError::new("processFailed").value("status", status))
            };
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn read_appended_log(log: &mut File) -> AppResult<String> {
    let mut output = Vec::new();
    log.read_to_end(&mut output)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(unix)]
fn stop_unix_command_tree(
    child: &mut std::process::Child,
    controller: &DeploymentController,
) -> AppResult<()> {
    let pid = child.id();
    if process_tree_alive(pid) {
        terminate_tree(pid, false);
    }
    let graceful_deadline = Instant::now() + Duration::from_secs(2);
    while process_tree_alive(pid) && Instant::now() < graceful_deadline {
        let _ = child.try_wait();
        thread::sleep(Duration::from_millis(50));
    }
    if process_tree_alive(pid) {
        terminate_tree(pid, true);
    }
    let _ = child.wait();
    let forced_deadline = Instant::now() + Duration::from_secs(5);
    while process_tree_alive(pid) && Instant::now() < forced_deadline {
        terminate_tree(pid, true);
        thread::sleep(Duration::from_millis(50));
    }
    if process_tree_alive(pid) {
        let error = AppError::new("serviceProcessTreeStillRunning").value("processId", pid);
        controller.record_cleanup_error(error.clone());
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
fn stop_windows_command_tree(
    child: &mut std::process::Child,
    job: &WindowsProcessGuard,
    controller: &DeploymentController,
) -> AppResult<()> {
    if let Err(error) = job.terminate() {
        controller.record_cleanup_error(error.clone());
        return Err(error);
    }
    let _ = child.wait();
    match job.wait_until_empty(Duration::from_secs(5)) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let error = AppError::new("serviceProcessTreeStillRunning");
            controller.record_cleanup_error(error.clone());
            Err(error)
        }
        Err(error) => {
            controller.record_cleanup_error(error.clone());
            Err(error)
        }
    }
}

fn isolated_command(command: &mut Command, paths: &ApplicationPaths) {
    let allowed = [
        "COMSPEC",
        "LANG",
        "LC_ALL",
        "NODE_EXTRA_CA_CERTS",
        "NO_PROXY",
        "PATH",
        "SSL_CERT_DIR",
        "SSL_CERT_FILE",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "TMPDIR",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
    ];
    let values: Vec<_> = allowed
        .iter()
        .filter_map(|key| std::env::var_os(key).map(|value| ((*key).to_owned(), value)))
        .collect();
    command
        .env_clear()
        .envs(values)
        .env("HOME", &paths.app_home)
        .env("USERPROFILE", &paths.app_home)
        .env(
            "NPM_CONFIG_USERCONFIG",
            paths.cache_dir.join("isolated-npmrc"),
        )
        .env("DSH_HOME", paths.cache_dir.join("validation-home"))
        .env("DSH_TELEMETRY_DISABLED", "1");
}

#[cfg(unix)]
pub(crate) fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}
#[cfg(windows)]
pub(crate) fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}
#[cfg(unix)]
pub(crate) fn terminate_tree(pid: u32, force: bool) {
    unsafe {
        libc::kill(
            -(pid as i32),
            if force { libc::SIGKILL } else { libc::SIGTERM },
        );
    }
}
#[cfg(unix)]
pub(crate) fn process_tree_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let result = unsafe { libc::kill(-pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
#[cfg(windows)]
pub(crate) fn terminate_tree(pid: u32, _force: bool) {
    let mut command = Command::new("taskkill");
    command.args(["/PID", &pid.to_string(), "/T", "/F"]);
    configure_process_group(&mut command);
    let _ = command.output();
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) enum WindowsProcessGuard {
    Job(windows_sys::Win32::Foundation::HANDLE),
    Snapshot {
        processes: Mutex<std::collections::HashMap<u32, windows_sys::Win32::Foundation::HANDLE>>,
    },
}

#[cfg(windows)]
unsafe impl Send for WindowsProcessGuard {}
// Job handles are safe to query and terminate concurrently. The snapshot
// fallback protects its mutable handle map with a mutex.
#[cfg(windows)]
unsafe impl Sync for WindowsProcessGuard {}

#[cfg(windows)]
impl WindowsProcessGuard {
    pub(crate) fn attach(child: &std::process::Child) -> AppResult<Self> {
        match Self::attach_job(child) {
            Ok(handle) => Ok(Self::Job(handle)),
            Err(job_error) => {
                log::warn!(
                    "Windows Job Object unavailable; using direct process-tree cleanup: {job_error}"
                );
                Self::attach_snapshot(child)
            }
        }
    }

    pub(crate) fn attach_snapshot(child: &std::process::Child) -> AppResult<Self> {
        let root_handle = duplicate_process_handle(child)?;
        Ok(Self::Snapshot {
            processes: Mutex::new(std::iter::once((child.id(), root_handle)).collect()),
        })
    }

    fn attach_job(
        child: &std::process::Child,
    ) -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
        use std::{mem::size_of, os::windows::io::AsRawHandle, ptr};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = WindowsOwnedHandle(handle);
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&information as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job.into_raw())
    }

    pub(crate) fn observe(&self) -> AppResult<()> {
        let Self::Snapshot { processes } = self else {
            return Ok(());
        };
        let entries = windows_process_entries()
            .map_err(|error| AppError::io("serviceProcessGuardFailed", &error))?;
        let mut processes = processes.lock().expect("Windows process guard poisoned");
        loop {
            let mut changed = false;
            for &(pid, parent_pid) in &entries {
                if pid != 0 && processes.contains_key(&parent_pid) && !processes.contains_key(&pid)
                {
                    match open_process_handle(pid) {
                        Ok(handle) => {
                            let handle = WindowsOwnedHandle(handle);
                            if !process_handle_running(handle.0).map_err(|error| {
                                AppError::io("serviceProcessGuardFailed", &error)
                            })? {
                                continue;
                            }
                            let confirmed_parent = windows_process_entries()?.into_iter().find_map(
                                |(candidate, parent)| (candidate == pid).then_some(parent),
                            );
                            if confirmed_parent
                                .is_some_and(|parent| processes.contains_key(&parent))
                            {
                                processes.insert(pid, handle.into_raw());
                                changed = true;
                            }
                        }
                        Err(error) if error.raw_os_error() == Some(87) => {}
                        Err(error) => {
                            return Err(AppError::io("serviceProcessGuardFailed", &error));
                        }
                    }
                }
            }
            if !changed {
                return Ok(());
            }
        }
    }

    pub(crate) fn terminate(&self) -> AppResult<()> {
        use windows_sys::Win32::System::{
            JobObjects::TerminateJobObject, Threading::TerminateProcess,
        };

        match self {
            Self::Job(handle) => {
                if unsafe { TerminateJobObject(*handle, 1) } == 0 {
                    return Err(AppError::io(
                        "serviceProcessGuardFailed",
                        &std::io::Error::last_os_error(),
                    ));
                }
            }
            Self::Snapshot { processes } => {
                self.observe()?;
                for handle in processes
                    .lock()
                    .expect("Windows process guard poisoned")
                    .values()
                {
                    if process_handle_running(*handle)
                        .map_err(|error| AppError::io("serviceProcessGuardFailed", &error))?
                        && unsafe { TerminateProcess(*handle, 1) } == 0
                    {
                        return Err(AppError::io(
                            "serviceProcessGuardFailed",
                            &std::io::Error::last_os_error(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wait_until_empty(&self, timeout: Duration) -> AppResult<bool> {
        use std::{mem::size_of, ptr};
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        let deadline = Instant::now() + timeout;
        loop {
            match self {
                Self::Job(handle) => {
                    let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                    if unsafe {
                        QueryInformationJobObject(
                            *handle,
                            JobObjectBasicAccountingInformation,
                            (&mut information as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION)
                                .cast(),
                            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                            ptr::null_mut(),
                        )
                    } == 0
                    {
                        return Err(AppError::io(
                            "serviceProcessGuardFailed",
                            &std::io::Error::last_os_error(),
                        ));
                    }
                    if information.ActiveProcesses == 0 {
                        return Ok(true);
                    }
                }
                Self::Snapshot { processes } => {
                    self.observe()?;
                    self.terminate()?;
                    if processes
                        .lock()
                        .expect("Windows process guard poisoned")
                        .values()
                        .try_fold(true, |all_stopped, handle| {
                            process_handle_running(*handle).map(|running| all_stopped && !running)
                        })
                        .map_err(|error| AppError::io("serviceProcessGuardFailed", &error))?
                    {
                        return Ok(true);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::TerminateProcess};

        match self {
            Self::Job(handle) => unsafe {
                CloseHandle(*handle);
            },
            Self::Snapshot { processes } => {
                for handle in processes
                    .get_mut()
                    .expect("Windows process guard poisoned")
                    .values()
                {
                    unsafe {
                        if process_handle_running(*handle).unwrap_or(true) {
                            TerminateProcess(*handle, 1);
                        }
                        CloseHandle(*handle);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
struct WindowsOwnedHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsOwnedHandle {
    fn into_raw(self) -> windows_sys::Win32::Foundation::HANDLE {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

#[cfg(windows)]
impl Drop for WindowsOwnedHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn duplicate_process_handle(
    child: &std::process::Child,
) -> AppResult<windows_sys::Win32::Foundation::HANDLE> {
    use std::{os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
        System::Threading::GetCurrentProcess,
    };

    let current = unsafe { GetCurrentProcess() };
    let mut duplicate = ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            current,
            child.as_raw_handle().cast(),
            current,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(AppError::io(
            "serviceProcessGuardFailed",
            &std::io::Error::last_os_error(),
        ));
    }
    Ok(duplicate)
}

#[cfg(windows)]
fn open_process_handle(pid: u32) -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}

#[cfg(windows)]
fn process_handle_running(handle: windows_sys::Win32::Foundation::HANDLE) -> std::io::Result<bool> {
    use windows_sys::Win32::{
        Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };

    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        WAIT_FAILED => Err(std::io::Error::last_os_error()),
        result => Err(std::io::Error::other(format!(
            "unexpected process wait result {result}"
        ))),
    }
}

#[cfg(windows)]
fn windows_process_entries() -> std::io::Result<Vec<(u32, u32)>> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = WindowsOwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut entries = Vec::new();
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(entries)
        } else {
            Err(error)
        };
    }
    loop {
        entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
    }
    Ok(entries)
}

fn acquire_lock(file: &File, controller: &DeploymentController) -> AppResult<()> {
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let contended = lock_contended_error().kind();
    loop {
        controller.check()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == contended && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(200))
            }
            Err(error) if error.kind() == contended => {
                return Err(AppError::new("deploymentBusy"));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn http_client() -> AppResult<Client> {
    Client::builder()
        .user_agent("dsh-desktop")
        .connect_timeout(Duration::from_secs(env_seconds(
            "DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS",
            10,
        )))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| AppError::new("networkClientFailed").detail(error.to_string()))
}
fn sha256(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)
        .map_err(|error| AppError::io("checksumFailed", &error))?;
    Ok(hex::encode(digest.finalize()))
}
fn resolve_node_version() -> AppResult<String> {
    let value = std::env::var("DSH_DESKTOP_NODE_VERSION")
        .unwrap_or_else(|_| NODE_VERSION.into())
        .trim_start_matches('v')
        .to_owned();
    Version::parse(&value).map(|_| value.clone()).map_err(|_| {
        AppError::new("environmentInvalid")
            .value("variable", "DSH_DESKTOP_NODE_VERSION")
            .value("value", value)
    })
}
fn node_executable(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("node.exe")
    } else {
        dir.join("bin/node")
    }
}
fn npm_cli(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("node_modules/npm/bin/npm-cli.js")
    } else {
        dir.join("lib/node_modules/npm/bin/npm-cli.js")
    }
}
fn node_filename(version: &str) -> AppResult<String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        return Err(AppError::new("unsupportedPlatform"));
    };
    if cfg!(target_os = "macos") {
        Ok(format!("node-v{version}-darwin-{arch}.tar.gz"))
    } else if cfg!(windows) {
        Ok(format!("node-v{version}-win-{arch}.zip"))
    } else {
        Err(AppError::new("unsupportedPlatform"))
    }
}
fn node_checksum(filename: &str) -> AppResult<String> {
    if let Ok(value) = std::env::var("DSH_DESKTOP_NODE_SHA256") {
        return if value.len() == 64 && value.chars().all(|item| item.is_ascii_hexdigit()) {
            Ok(value.to_ascii_lowercase())
        } else {
            Err(AppError::new("environmentInvalid").value("variable", "DSH_DESKTOP_NODE_SHA256"))
        };
    }
    RELEASE_NODE_ASSETS
        .iter()
        .find_map(|(asset, checksum)| (*asset == filename).then(|| (*checksum).to_owned()))
        .ok_or_else(|| AppError::new("nodeChecksumMissing").value("filename", filename))
}
fn node_bases() -> Vec<String> {
    env_list("DSH_DESKTOP_NODE_BASES")
        .or_else(|| {
            std::env::var("DSH_DESKTOP_NODE_BASE")
                .ok()
                .map(|item| vec![item])
        })
        .unwrap_or_else(|| NODE_BASES.iter().map(ToString::to_string).collect())
}
fn npm_registries() -> Vec<String> {
    env_list("DSH_DESKTOP_NPM_REGISTRIES")
        .or_else(|| {
            std::env::var("DSH_DESKTOP_NPM_REGISTRY")
                .ok()
                .map(|item| vec![item])
        })
        .unwrap_or_else(|| NPM_REGISTRIES.iter().map(ToString::to_string).collect())
}
fn env_list(name: &str) -> Option<Vec<String>> {
    let values = std::env::var(name)
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}
fn env_seconds(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
fn display_source(raw: &str) -> String {
    url::Url::parse(raw)
        .map(|mut value| {
            value.set_query(None);
            value.set_fragment(None);
            let _ = value.set_username("");
            let _ = value.set_password(None);
            value.to_string().trim_end_matches('/').to_owned()
        })
        .unwrap_or_else(|_| "<invalid source>".into())
}
fn validate_network_source(raw: &str) -> AppResult<url::Url> {
    let value = url::Url::parse(raw).map_err(|_| AppError::new("downloadSourceInvalid"))?;
    let local = matches!(value.host_str(), Some("127.0.0.1" | "localhost"));
    let transport_allowed = value.scheme() == "https" || (value.scheme() == "http" && local);
    if !transport_allowed
        || !value.username().is_empty()
        || value.password().is_some()
        || value.query().is_some()
        || value.fragment().is_some()
    {
        return Err(AppError::new("downloadSourceInvalid"));
    }
    Ok(value)
}
fn activity<const N: usize>(
    notify: &impl Fn(DeploymentEvent),
    code: ActivityCode,
    values: [(&str, String); N],
) {
    notify(DeploymentEvent::Activity {
        code,
        values: values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    });
}

fn fix_spawn_helper(_dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in walkdir::WalkDir::new(_dir.join("node_modules/node-pty/prebuilds"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() == "spawn-helper")
        {
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, net::TcpListener, thread};

    #[test]
    fn node_assets_are_pinned_for_release_targets() {
        for (filename, _) in RELEASE_NODE_ASSETS {
            assert_eq!(node_checksum(filename).unwrap().len(), 64);
        }
    }

    #[test]
    fn archive_paths_reject_traversal() {
        assert!(validate_archive_path(Path::new("node/../../secret")).is_err());
        assert!(validate_link(Path::new("node/bin/npm"), Path::new("../../outside")).is_err());
    }

    #[test]
    fn extracted_archive_size_is_bounded() {
        let mut total = MAX_NODE_EXTRACTED_BYTES;
        assert_eq!(
            include_extracted_bytes(&mut total, 1).unwrap_err().code,
            "nodeArchiveTooLarge"
        );
    }

    #[test]
    fn network_sources_require_https_except_for_loopback_tests() {
        assert!(validate_network_source("https://registry.npmjs.org").is_ok());
        assert!(validate_network_source("http://127.0.0.1:8123").is_ok());
        assert!(validate_network_source("http://registry.example.test").is_err());
        assert!(validate_network_source("https://token@example.test").is_err());
    }

    #[test]
    fn registry_latest_must_resolve_to_an_existing_exact_version() {
        let (registry, server) = serve_responses(vec![
            json_response(r#"{"version":"0.1.1-rc.1"}"#),
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);

        let error = query_registry_version(&http_client().unwrap(), &registry).unwrap_err();
        server.join().unwrap();

        assert_eq!(error.code, "versionQueryFailed");
        assert!(
            error
                .safe_detail
                .as_deref()
                .is_some_and(|detail| detail.contains("0.1.1-rc.1 is unavailable"))
        );
    }

    #[test]
    fn registry_latest_is_accepted_after_exact_version_confirmation() {
        let metadata = json_response(r#"{"version":"0.1.0-rc.7"}"#);
        let (registry, server) = serve_responses(vec![metadata.clone(), metadata]);

        let version = query_registry_version(&http_client().unwrap(), &registry).unwrap();
        server.join().unwrap();

        assert_eq!(version, Version::parse("0.1.0-rc.7").unwrap());
    }

    #[test]
    fn exact_install_sources_are_ranked_by_observed_latency() {
        let metadata = json_response(r#"{"version":"0.1.0-rc.7"}"#);
        let slow_response = metadata.clone();
        let (slow, slow_server) = serve_once(move |mut stream| {
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            thread::sleep(Duration::from_millis(40));
            stream.write_all(slow_response.as_bytes()).unwrap();
        });
        let (fast, fast_server) = serve_once(move |mut stream| {
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream.write_all(metadata.as_bytes()).unwrap();
        });

        let ranked = ranked_install_registries(
            &http_client().unwrap(),
            vec![slow.clone(), fast.clone()],
            &Version::parse("0.1.0-rc.7").unwrap(),
            None,
        )
        .unwrap();
        slow_server.join().unwrap();
        fast_server.join().unwrap();

        assert_eq!(ranked, vec![fast, slow]);
    }

    #[test]
    fn last_successful_install_source_is_kept_for_cache_reuse() {
        let metadata = json_response(r#"{"version":"0.1.0-rc.7"}"#);
        let slow_response = metadata.clone();
        let (slow, slow_server) = serve_once(move |mut stream| {
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            thread::sleep(Duration::from_millis(40));
            stream.write_all(slow_response.as_bytes()).unwrap();
        });
        let (fast, fast_server) = serve_once(move |mut stream| {
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            stream.write_all(metadata.as_bytes()).unwrap();
        });

        let ranked = ranked_install_registries(
            &http_client().unwrap(),
            vec![slow.clone(), fast.clone()],
            &Version::parse("0.1.0-rc.7").unwrap(),
            Some(&slow),
        )
        .unwrap();
        slow_server.join().unwrap();
        fast_server.join().unwrap();

        assert_eq!(ranked, vec![slow, fast]);
    }

    #[test]
    fn npm_output_reports_real_install_phases_and_waiting() {
        let events = RefCell::new(Vec::new());
        let notify = |event| events.borrow_mut().push(event);
        let mut activity = NpmInstallActivity::new("0.1.0-rc.7", "https://registry.npmjs.org");

        activity.observe(
            "14 silly fetch manifest @deepseek-ai/dsh@0.1.0-rc.7\n",
            Duration::ZERO,
            &notify,
        );
        activity.observe(
            "20 http fetch GET 200 https://registry.npmjs.org/a/-/a-1.0.0.tgz\n",
            Duration::ZERO,
            &notify,
        );
        activity.observe("21 silly ADD node_modules/a\n", Duration::ZERO, &notify);
        activity.observe("", NPM_WAITING_AFTER, &notify);

        let events = events.into_inner();
        assert!(matches!(
            events.first(),
            Some(DeploymentEvent::Activity {
                code: ActivityCode::ResolvingHarnessDependencies,
                ..
            })
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            DeploymentEvent::Activity {
                code: ActivityCode::DownloadingHarnessPackages,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            DeploymentEvent::Activity {
                code: ActivityCode::WritingHarnessRuntime,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(DeploymentEvent::ActivityUpdate { values })
                if values.get("status").map(String::as_str) == Some("waiting")
        ));
    }

    #[test]
    fn run_logged_streams_appended_output_to_the_observer() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "runtime::tests::run_logged_output_helper",
                "--nocapture",
            ])
            .env("DSH_RUN_LOGGED_OUTPUT_HELPER", "1");
        let observed = RefCell::new(Vec::new());

        run_logged(
            &mut command,
            &temp.path().join("install.log"),
            ProcessTimeouts {
                total: Duration::from_secs(10),
            },
            &DeploymentController::default(),
            |output, _| {
                if !output.is_empty() {
                    observed.borrow_mut().push(output.to_owned());
                }
            },
        )
        .unwrap();

        let observed = observed.into_inner();
        assert!(observed.iter().any(|chunk| chunk.contains("first-output")));
        assert!(observed.iter().any(|chunk| chunk.contains("second-output")));
    }

    #[test]
    fn run_logged_output_helper() {
        if std::env::var_os("DSH_RUN_LOGGED_OUTPUT_HELPER").is_none() {
            return;
        }
        println!("first-output");
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(250));
        println!("second-output");
        std::io::stdout().flush().unwrap();
    }

    #[test]
    fn stale_npm_cache_is_pruned_only_after_reaching_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let npm = cache.join("npm");
        fs::create_dir_all(&npm).unwrap();
        fs::write(npm.join("cached-package"), [0_u8; 8]).unwrap();
        fs::write(cache.join("npm.last-used"), "100\n").unwrap();

        assert!(!prune_npm_cache_at(&cache, 200, 9, Duration::from_secs(10)).unwrap());
        assert!(npm.exists());
        assert!(
            prune_npm_cache_at(
                &cache,
                200 + NPM_CACHE_CHECK_INTERVAL.as_secs(),
                8,
                Duration::from_secs(10),
            )
            .unwrap()
        );
        assert!(!npm.exists());
    }

    #[test]
    fn recently_used_npm_cache_is_preserved_even_at_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let npm = cache.join("npm");
        fs::create_dir_all(&npm).unwrap();
        fs::write(npm.join("cached-package"), [0_u8; 8]).unwrap();
        fs::write(cache.join("npm.last-used"), "195\n").unwrap();

        assert!(!prune_npm_cache_at(&cache, 200, 8, Duration::from_secs(10)).unwrap());
        assert!(npm.exists());
    }

    #[cfg(unix)]
    #[test]
    fn npm_cache_pruning_never_follows_links_outside_the_cache() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let npm = cache.join("npm");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&npm).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"user-data").unwrap();
        symlink(&outside, npm.join("external-link")).unwrap();
        fs::write(cache.join("npm.last-used"), "100\n").unwrap();

        assert!(prune_npm_cache_at(&cache, 200, 1, Duration::from_secs(10)).unwrap());
        assert_eq!(fs::read(outside.join("keep")).unwrap(), b"user-data");
    }

    #[test]
    fn failed_publication_can_restore_previous_directory() {
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("runtime");
        let staging = temp.path().join("runtime.staging-test");
        fs::create_dir(&active).unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(active.join("value"), "old").unwrap();
        fs::write(staging.join("value"), "candidate").unwrap();
        let previous = publish_directory(&staging, &active).unwrap();
        assert_eq!(
            fs::read_to_string(active.join("value")).unwrap(),
            "candidate"
        );
        rollback_directory(&active, previous.as_deref()).unwrap();
        assert_eq!(fs::read_to_string(active.join("value")).unwrap(), "old");
    }

    #[test]
    fn download_rejects_an_oversized_content_length() {
        let (url, server) = serve_once(|mut stream| {
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_NODE_ARCHIVE_BYTES + 1
            );
            let _ = stream.flush();
            // Keep the fixture alive long enough for the client to return the
            // response headers. Closing immediately can race reqwest into an
            // incomplete-body error before download_once checks Content-Length.
            thread::sleep(Duration::from_millis(20));
        });
        let temp = tempfile::tempdir().unwrap();
        let error = download_once(
            &http_client().unwrap(),
            &url,
            &temp.path().join("archive.part"),
            Instant::now() + Duration::from_secs(2),
            &DeploymentController::default(),
            &|_| {},
        )
        .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.code, "downloadTooLarge");
    }

    #[test]
    fn download_deadline_is_enforced_while_streaming() {
        let (url, server) = serve_once(|mut stream| {
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2048\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(&[1u8; 1024]);
            let _ = stream.flush();
            thread::sleep(Duration::from_millis(100));
            let _ = stream.write_all(&[1u8; 1024]);
        });
        let temp = tempfile::tempdir().unwrap();
        let error = download_once(
            &http_client().unwrap(),
            &url,
            &temp.path().join("archive.part"),
            Instant::now() + Duration::from_millis(20),
            &DeploymentController::default(),
            &|_| {},
        )
        .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.code, "downloadTimedOut");
    }

    #[test]
    fn oversized_cached_archive_is_not_reused() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("archive");
        File::create(&archive)
            .unwrap()
            .set_len(MAX_NODE_ARCHIVE_BYTES + 1)
            .unwrap();
        let error = download_verified(
            &[],
            &archive,
            &"0".repeat(64),
            &DeploymentController::default(),
            &|_| {},
        )
        .unwrap_err();
        assert_eq!(error.code, "downloadFailed");
        assert!(!archive.exists());
    }

    fn serve_once(
        handler: impl FnOnce(std::net::TcpStream) + Send + 'static,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handler(stream);
        });
        (format!("http://{address}/archive"), server)
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn serve_responses(responses: Vec<String>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        (format!("http://{address}"), server)
    }
}
