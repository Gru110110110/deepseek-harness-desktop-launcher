use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
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
}

#[derive(Debug, Clone, Default)]
pub struct DeploymentController {
    cancelled: Arc<AtomicBool>,
}

impl DeploymentController {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
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
    let client = http_client()?;
    let registries = npm_registries();
    let mut versions = Vec::new();
    let mut errors = Vec::new();
    for registry in registries {
        controller.check()?;
        match query_registry_version(&client, &registry) {
            Ok(version) => versions.push(version),
            Err(error) => errors.push(format!("{registry}: {error}")),
        }
    }
    versions
        .into_iter()
        .max()
        .map(|version| version.to_string())
        .ok_or_else(|| AppError::new("versionQueryFailed").detail(errors.join("; ")))
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
    for registry in NPM_REGISTRIES {
        let version = retry_release_source(|| query_registry_version(&client, registry))?;
        verified.push(format!(
            "Harness latest {version} via {}",
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
    value
        .get("version")
        .and_then(Value::as_str)
        .and_then(|raw| Version::parse(raw).ok())
        .ok_or_else(|| {
            AppError::new("versionQueryFailed").detail(format!(
                "{}: invalid version metadata",
                display_source(registry)
            ))
        })
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
    let registries = npm_registries();
    for registry in registries {
        controller.check()?;
        validate_network_source(&registry)?;
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
        isolated_command(&mut command, paths);
        command
            .env("NPM_CONFIG_REGISTRY", &registry)
            .current_dir(&staging);
        if run_logged(
            &mut command,
            &paths.install_log,
            Duration::from_secs(15 * 60),
            controller,
        )
        .is_ok()
        {
            activity(
                notify,
                ActivityCode::ValidatingHarness,
                [("version", version.to_owned())],
            );
            fix_spawn_helper(&staging);
            if dsh_valid(paths, &paths.node_dir, &staging, version) {
                return Ok(staging);
            }
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
    remove_owned(partial)?;
    let mut response = client
        .get(url)
        .send()
        .and_then(|item| item.error_for_status())
        .map_err(|error| AppError::new("downloadFailed").detail(error.to_string()))?;
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
        let count = response
            .read(&mut buffer)
            .map_err(|error| AppError::io("downloadFailed", &error))?;
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

fn run_logged(
    command: &mut Command,
    log_path: &Path,
    timeout: Duration,
    controller: &DeploymentController,
) -> AppResult<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .stdin(Stdio::null());
    configure_process_group(command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if controller.check().is_err() || Instant::now() >= deadline {
            terminate_tree(child.id(), false);
            thread::sleep(Duration::from_secs(2));
            terminate_tree(child.id(), true);
            let _ = child.wait();
            return Err(AppError::new(
                if controller.cancelled.load(Ordering::SeqCst) {
                    "deploymentCancelled"
                } else {
                    "processTimeout"
                },
            ));
        }
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(AppError::new("processFailed").value("status", status))
            };
        }
        thread::sleep(Duration::from_millis(100));
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
    command.creation_flags(0x0000_0200 | 0x0800_0000);
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
#[cfg(windows)]
pub(crate) fn terminate_tree(pid: u32, _force: bool) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

fn acquire_lock(file: &File, controller: &DeploymentController) -> AppResult<()> {
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        controller.check()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(200))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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

fn fix_spawn_helper(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in walkdir::WalkDir::new(dir.join("node_modules/node-pty/prebuilds"))
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
    use std::{net::TcpListener, thread};

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
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
            for _ in 0..10 {
                if stream.write_all(&[1u8; 1024]).is_err() {
                    break;
                }
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(20));
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let error = download_once(
            &http_client().unwrap(),
            &url,
            &temp.path().join("archive.part"),
            Instant::now() + Duration::from_millis(10),
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
}
