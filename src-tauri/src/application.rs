use std::{
    fs::{File, OpenOptions},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use dsh_core::{
    ActivityCode, ActivityState, AppError, AppResult, ApplicationPaths, DesktopUpdateState,
    HarnessUpdateState, Language, LauncherPhase, LauncherSnapshot, LauncherStep, MigrationState,
    ProgressState, ThemePreference,
    browser::BrowserCatalog,
    migration::MigrationService,
    preferences::Preferences,
    runtime::{
        DeploymentController, DeploymentEvent, deploy_runtime, installed_version,
        latest_harness_version,
    },
    service::ServerManager,
};
use fs2::{FileExt, lock_contended_error};
use semver::Version;
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
    menu::{Menu, MenuItem},
    tray::{TrayIcon, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_updater::UpdaterExt;

use crate::commands;

const WEBSITE: &str = "https://dsdesktop.com/";
const GITHUB_REPOSITORY: &str = "https://github.com/Gru110110110/deepseek-harness-desktop-launcher";
const HARNESS_GITHUB_REPOSITORY: &str = "https://github.com/deepseek-ai/deepseek-harness";
const DEEPSEEK_PLATFORM: &str = "https://platform.deepseek.com/";
const INSTANCE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const DESKTOP_UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const DESKTOP_UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const HARNESS_UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

fn external_link_url(target: &str) -> Option<&'static str> {
    match target {
        "github" => Some(GITHUB_REPOSITORY),
        "harnessGithub" => Some(HARNESS_GITHUB_REPOSITORY),
        "deepseek" => Some(DEEPSEEK_PLATFORM),
        _ => None,
    }
}

pub(crate) struct AppState {
    app: AppHandle,
    paths: ApplicationPaths,
    _instance_lock: File,
    snapshot: Mutex<LauncherSnapshot>,
    preferences: Mutex<Preferences>,
    browsers: BrowserCatalog,
    server: Mutex<ServerManager>,
    deployment: Mutex<Option<DeploymentController>>,
    migration: MigrationService,
    desktop_update_busy: AtomicBool,
    harness_update_check_busy: AtomicBool,
    startup_thread: Mutex<Option<thread::JoinHandle<()>>>,
    quitting: AtomicBool,
    exit_ready: AtomicBool,
    tray: Mutex<Option<TrayIcon>>,
}

impl AppState {
    fn new(app: AppHandle, paths: ApplicationPaths) -> AppResult<Arc<Self>> {
        let instance_lock = acquire_instance_lock(&paths)?;
        let preferences = Preferences::load(&paths.preferences_file, &paths.language_file);
        let browsers = BrowserCatalog::discover();
        let mut snapshot = LauncherSnapshot::initial(env!("CARGO_PKG_VERSION"));
        snapshot.language = preferences.language;
        snapshot.theme = preferences.theme;
        snapshot.browsers = browsers.choices();
        snapshot.selected_browser_id = if browsers.contains(&preferences.browser_id) {
            preferences.browser_id.clone()
        } else {
            "system".into()
        };
        snapshot.harness_version = installed_version(&paths);
        let migration = MigrationService::from_environment(paths.clone())?;
        Ok(Arc::new(Self {
            app,
            _instance_lock: instance_lock,
            server: Mutex::new(ServerManager::new(paths.clone())),
            paths,
            snapshot: Mutex::new(snapshot),
            preferences: Mutex::new(preferences),
            browsers,
            deployment: Mutex::new(None),
            migration,
            desktop_update_busy: AtomicBool::new(false),
            harness_update_check_busy: AtomicBool::new(false),
            startup_thread: Mutex::new(None),
            quitting: AtomicBool::new(false),
            exit_ready: AtomicBool::new(false),
            tray: Mutex::new(None),
        }))
    }

    pub(crate) fn snapshot(&self) -> LauncherSnapshot {
        self.snapshot.lock().expect("snapshot poisoned").clone()
    }

    fn mutate(&self, update: impl FnOnce(&mut LauncherSnapshot)) {
        let _ = self.mutate_if(|snapshot| {
            update(snapshot);
            true
        });
    }

    fn mutate_if(&self, update: impl FnOnce(&mut LauncherSnapshot) -> bool) -> bool {
        let value = {
            let mut snapshot = self.snapshot.lock().expect("snapshot poisoned");
            if !update(&mut snapshot) {
                return false;
            }
            snapshot.revision = snapshot.revision.saturating_add(1);
            snapshot.clone()
        };
        let _ = self.app.emit("launcher://state", value);
        true
    }

    pub(crate) fn start(self: &Arc<Self>, force: bool, target_version: Option<String>) {
        self.start_worker(force, target_version, false);
    }

    pub(crate) fn stop_service(&self) -> AppResult<()> {
        if !self.mutate_if(|snapshot| {
            if snapshot.phase != LauncherPhase::Ready {
                return false;
            }
            snapshot.phase = LauncherPhase::Stopping;
            true
        }) {
            return Err(AppError::new("serviceNotReady"));
        }
        match self.server.lock().expect("server poisoned").stop() {
            Ok(()) => {
                self.mutate(|snapshot| {
                    snapshot.phase = LauncherPhase::Stopped;
                    snapshot.web_url = None;
                    snapshot.service_started_at_ms = None;
                    snapshot.activity = None;
                    snapshot.error = None;
                });
                Ok(())
            }
            Err(error) => {
                self.fail(error.clone());
                Err(error)
            }
        }
    }

    pub(crate) fn restart_service(&self) -> AppResult<()> {
        if !self.mutate_if(|snapshot| {
            if snapshot.phase != LauncherPhase::Ready {
                return false;
            }
            snapshot.phase = LauncherPhase::Starting;
            snapshot.web_url = None;
            snapshot.service_started_at_ms = None;
            snapshot.activity = Some(ActivityState {
                code: ActivityCode::StartingService,
                values: Default::default(),
                started_at_ms: now_ms(),
            });
            true
        }) {
            return Err(AppError::new("serviceNotReady"));
        }
        let restarted = {
            let mut server = self.server.lock().expect("server poisoned");
            server.stop().and_then(|()| server.start())
        };
        match restarted {
            Ok(url) => {
                self.mutate(|snapshot| {
                    snapshot.phase = LauncherPhase::Ready;
                    snapshot.web_url = Some(url);
                    snapshot.service_started_at_ms = Some(now_ms());
                    snapshot.activity = None;
                    snapshot.error = None;
                });
                Ok(())
            }
            Err(error) => {
                self.fail(error.clone());
                Err(error)
            }
        }
    }

    fn start_worker(
        self: &Arc<Self>,
        force: bool,
        target_version: Option<String>,
        migration_approved: bool,
    ) {
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        let mut slot = self.startup_thread.lock().expect("startup thread poisoned");
        if slot.as_ref().is_some_and(|thread| !thread.is_finished()) {
            return;
        }
        if let Some(finished) = slot.take() {
            let _ = finished.join();
        }
        let controller = DeploymentController::default();
        *self.deployment.lock().expect("deployment poisoned") = Some(controller.clone());
        let state = Arc::clone(self);
        let worker = thread::Builder::new()
            .name("launcher-startup".into())
            .spawn(move || {
                state.run_startup(force, target_version, migration_approved, &controller);
                *state.deployment.lock().expect("deployment poisoned") = None;
            })
            .expect("launcher worker spawn failed");
        *slot = Some(worker);
    }

    fn run_startup(
        self: &Arc<Self>,
        force: bool,
        target_version: Option<String>,
        migration_approved: bool,
        controller: &DeploymentController,
    ) {
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        self.mutate(|snapshot| {
            snapshot.phase = LauncherPhase::Preparing;
            snapshot.step = LauncherStep::Prepare;
            snapshot.error = None;
            snapshot.web_url = None;
            snapshot.service_started_at_ms = None;
            snapshot.progress = ProgressState::Indeterminate;
            if force {
                snapshot.harness_update = HarnessUpdateState::Installing {
                    version: target_version.clone().unwrap_or_else(|| "latest".into()),
                };
            }
        });
        if !force {
            if let Err(error) = self.migration.recover() {
                self.fail_unless_quitting(error);
                return;
            }
            if migration_approved {
                self.mutate(|snapshot| {
                    snapshot.activity = Some(ActivityState {
                        code: ActivityCode::MigratingData,
                        values: Default::default(),
                        started_at_ms: now_ms(),
                    });
                    snapshot.progress = ProgressState::Indeterminate;
                });
                match self.migration.apply() {
                    Ok(outcome) => {
                        if let Some(warning) = outcome.warning {
                            log::warn!("optional CC Switch import was skipped: {warning}");
                            self.mutate(|snapshot| {
                                snapshot.migration =
                                    MigrationState::CompletedWithWarning { warning }
                            });
                        } else {
                            self.mutate(|snapshot| snapshot.migration = MigrationState::Completed);
                        }
                    }
                    Err(error) => {
                        log::warn!("local data import failed and will be skipped: {error:?}");
                        if let Err(skip_error) = self.migration.skip() {
                            log::error!(
                                "the failed import could not be safely recovered and skipped: {skip_error:?}"
                            );
                            self.fail_unless_quitting(skip_error);
                            return;
                        }
                        let detail = error.safe_detail.unwrap_or(error.code);
                        self.mutate(|snapshot| {
                            snapshot.migration = MigrationState::CompletedWithWarning {
                                warning: AppError::new("migrationImportSkipped").detail(detail),
                            }
                        });
                    }
                }
            } else {
                match self.migration.discover() {
                    Ok(Some(plan)) => {
                        self.mutate(|snapshot| {
                            snapshot.phase = LauncherPhase::AwaitingMigration;
                            snapshot.activity = None;
                            snapshot.progress = ProgressState::Indeterminate;
                            snapshot.migration = MigrationState::Pending { plan };
                        });
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.fail_unless_quitting(error);
                        return;
                    }
                }
            }
        }
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        if force && let Err(error) = self.server.lock().expect("server poisoned").stop() {
            self.fail_unless_quitting(error);
            return;
        }
        let weak = Arc::downgrade(self);
        let deployed = deploy_runtime(
            &self.paths,
            force,
            target_version.as_deref(),
            controller,
            move |event| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                match event {
                    DeploymentEvent::Activity { code, values } => state.mutate(|snapshot| {
                        snapshot.activity = Some(ActivityState {
                            code,
                            values,
                            started_at_ms: now_ms(),
                        });
                        snapshot.progress = ProgressState::Indeterminate;
                    }),
                    DeploymentEvent::Progress { done, total } => state.mutate(|snapshot| {
                        snapshot.progress = total
                            .filter(|total| *total > 0)
                            .map_or(ProgressState::Indeterminate, |total| {
                                ProgressState::Determinate { done, total }
                            })
                    }),
                }
            },
        );
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        match deployed {
            Ok(version) => self.mutate(|snapshot| {
                snapshot.harness_version = Some(version);
                snapshot.harness_update = HarnessUpdateState::None;
            }),
            Err(error) => {
                if force {
                    self.restore_service_after_failed_update(
                        error,
                        target_version.unwrap_or_else(|| "latest".into()),
                    );
                } else {
                    self.fail_unless_quitting(error);
                }
                return;
            }
        }
        self.mutate(|snapshot| {
            snapshot.phase = LauncherPhase::Starting;
            snapshot.step = LauncherStep::Start;
            snapshot.activity = Some(ActivityState {
                code: ActivityCode::StartingService,
                values: Default::default(),
                started_at_ms: now_ms(),
            });
            snapshot.progress = ProgressState::Indeterminate;
        });
        let started = self
            .server
            .lock()
            .expect("server poisoned")
            .start_cancellable(|| {
                controller.is_cancelled() || self.quitting.load(Ordering::SeqCst)
            });
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        match started {
            Ok(url) => {
                self.mutate(|snapshot| {
                    snapshot.phase = LauncherPhase::Ready;
                    snapshot.web_url = Some(url);
                    snapshot.service_started_at_ms = Some(now_ms());
                    snapshot.activity = None;
                    snapshot.error = None;
                });
                let state = Arc::clone(self);
                tauri::async_runtime::spawn(async move {
                    let _ = state.check_harness_update().await;
                });
            }
            Err(error) => self.fail_unless_quitting(error),
        }
    }

    fn fail_unless_quitting(&self, error: AppError) {
        if !self.quitting.load(Ordering::SeqCst) {
            self.fail(error);
        }
    }

    fn fail(&self, error: AppError) {
        log::error!("launcher operation failed: {error:?}");
        self.mutate(|snapshot| {
            snapshot.phase = LauncherPhase::Failed;
            snapshot.error = Some(error);
            snapshot.activity = None;
            snapshot.harness_update = HarnessUpdateState::None;
        });
    }

    fn restore_service_after_failed_update(&self, update_error: AppError, version: String) {
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        log::error!("Harness update failed and was rolled back: {update_error}");
        let restarted = self.server.lock().expect("server poisoned").start();
        match restarted {
            Ok(url) => self.mutate(|snapshot| {
                snapshot.phase = LauncherPhase::Ready;
                snapshot.step = LauncherStep::Start;
                snapshot.activity = None;
                snapshot.error = Some(update_error);
                snapshot.web_url = Some(url);
                snapshot.service_started_at_ms = Some(now_ms());
                snapshot.harness_version = installed_version(&self.paths);
                snapshot.harness_update = HarnessUpdateState::Failed { version };
            }),
            Err(restart_error) => {
                log::error!("the previous Harness runtime could not be restarted: {restart_error}");
                self.fail(restart_error);
            }
        }
    }

    pub(crate) async fn check_harness_update(self: &Arc<Self>) -> AppResult<Option<String>> {
        if self.quitting.load(Ordering::SeqCst) {
            return Err(AppError::new("harnessUpdateCheckCancelled"));
        }
        let snapshot = self.snapshot();
        if !matches!(
            snapshot.phase,
            LauncherPhase::Ready | LauncherPhase::Stopped
        ) {
            return Err(AppError::new("serviceNotReady"));
        }
        let _operation = self.begin_harness_update_check()?;
        let previous = snapshot.harness_update;
        if matches!(&previous, HarnessUpdateState::Installing { .. }) {
            return Err(AppError::new("harnessUpdateBusy"));
        }
        let current_value = snapshot
            .harness_version
            .ok_or_else(|| AppError::new("serviceNotReady"))?;
        let current = Version::parse(&current_value)
            .map_err(|_| AppError::new("runtimeVersionInvalid").value("version", current_value))?;
        if self.quitting.load(Ordering::SeqCst) {
            return Err(AppError::new("harnessUpdateCheckCancelled"));
        }
        if !self.mutate_if(|snapshot| mark_harness_update_checking(snapshot, &previous)) {
            return Err(AppError::new("harnessUpdateBusy"));
        }

        let controller = DeploymentController::default();
        let query_controller = controller.clone();
        let query =
            tauri::async_runtime::spawn_blocking(move || latest_harness_version(&query_controller));
        let result = match tokio::time::timeout(HARNESS_UPDATE_CHECK_TIMEOUT, query).await {
            Ok(joined) => joined
                .map_err(|error| AppError::new("versionQueryFailed").detail(error.to_string()))
                .and_then(|result| result),
            Err(_) => {
                controller.cancel();
                Err(AppError::new("harnessUpdateCheckTimedOut"))
            }
        };

        if self.quitting.load(Ordering::SeqCst) {
            controller.cancel();
            let _ =
                self.mutate_if(|snapshot| replace_harness_update_if_checking(snapshot, previous));
            return Err(AppError::new("harnessUpdateCheckCancelled"));
        }

        match result {
            Ok(latest) => {
                let available = Version::parse(&latest)
                    .ok()
                    .filter(|latest| latest > &current)
                    .map(|_| latest);
                let _ = self.mutate_if(|snapshot| {
                    replace_harness_update_if_checking(
                        snapshot,
                        available
                            .clone()
                            .map_or(HarnessUpdateState::None, |version| {
                                HarnessUpdateState::Available { version }
                            }),
                    )
                });
                Ok(available)
            }
            Err(error) => {
                log::warn!("Harness update check failed: {error}");
                let _ = self
                    .mutate_if(|snapshot| replace_harness_update_if_checking(snapshot, previous));
                Err(error)
            }
        }
    }

    pub(crate) fn update_harness(self: &Arc<Self>) {
        let target = match self.snapshot().harness_update {
            HarnessUpdateState::Available { version } | HarnessUpdateState::Failed { version } => {
                Some(version)
            }
            _ => None,
        };
        if target.is_some() {
            self.start(true, target);
        }
    }

    pub(crate) fn approve_migration(self: &Arc<Self>) -> AppResult<()> {
        let plan = match self.snapshot().migration {
            MigrationState::Pending { plan } => plan,
            _ => return Err(AppError::new("migrationNotAvailable")),
        };
        self.join_startup();
        self.mutate(|snapshot| {
            snapshot.phase = LauncherPhase::Preparing;
            snapshot.migration = MigrationState::Applying { plan };
            snapshot.error = None;
        });
        self.start_worker(false, None, true);
        Ok(())
    }

    pub(crate) fn skip_migration(self: &Arc<Self>) -> AppResult<()> {
        if !matches!(self.snapshot().migration, MigrationState::Pending { .. }) {
            return Err(AppError::new("migrationNotAvailable"));
        }
        self.join_startup();
        self.migration.skip()?;
        self.mutate(|snapshot| {
            snapshot.phase = LauncherPhase::Preparing;
            snapshot.migration = MigrationState::Skipped;
            snapshot.error = None;
        });
        self.start_worker(false, None, false);
        Ok(())
    }

    pub(crate) fn set_language(&self, language: Language) -> AppResult<()> {
        {
            let mut preferences = self.preferences.lock().expect("preferences poisoned");
            let mut candidate = preferences.clone();
            candidate.language = language;
            candidate.save(&self.paths.preferences_file)?;
            *preferences = candidate;
        }
        self.mutate(|snapshot| snapshot.language = language);
        if let Err(error) = self.refresh_tray_menu(language) {
            log::warn!("tray language refresh failed: {error}");
        }
        Ok(())
    }
    pub(crate) fn set_theme(&self, theme: ThemePreference) -> AppResult<()> {
        {
            let mut preferences = self.preferences.lock().expect("preferences poisoned");
            let mut candidate = preferences.clone();
            candidate.theme = theme;
            candidate.save(&self.paths.preferences_file)?;
            *preferences = candidate;
        }
        self.mutate(|snapshot| snapshot.theme = theme);
        Ok(())
    }
    pub(crate) fn select_browser(&self, id: String) -> AppResult<()> {
        if !self.browsers.contains(&id) {
            return Err(AppError::new("browserUnavailable"));
        }
        {
            let mut preferences = self.preferences.lock().expect("preferences poisoned");
            let mut candidate = preferences.clone();
            candidate.browser_id = id.clone();
            candidate.save(&self.paths.preferences_file)?;
            *preferences = candidate;
        }
        self.mutate(|snapshot| snapshot.selected_browser_id = id);
        Ok(())
    }
    pub(crate) fn open_web_ui(&self) -> AppResult<()> {
        let snapshot = self.snapshot();
        let url = snapshot
            .web_url
            .ok_or_else(|| AppError::new("serviceNotReady"))?;
        self.browsers.open(&snapshot.selected_browser_id, &url)
    }
    pub(crate) fn open_website(&self) -> AppResult<()> {
        self.browsers.open("system", WEBSITE)
    }
    pub(crate) fn open_external_link(&self, target: &str) -> AppResult<()> {
        let url = external_link_url(target).ok_or_else(|| AppError::new("externalLinkInvalid"))?;
        self.browsers.open("system", url)
    }
    pub(crate) fn web_url(&self) -> AppResult<String> {
        self.snapshot()
            .web_url
            .ok_or_else(|| AppError::new("serviceNotReady"))
    }
    fn begin_desktop_update(self: &Arc<Self>) -> AppResult<DesktopUpdateOperation> {
        self.desktop_update_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AppError::new("desktopUpdateBusy"))?;
        Ok(DesktopUpdateOperation {
            state: Arc::clone(self),
        })
    }

    fn begin_harness_update_check(self: &Arc<Self>) -> AppResult<HarnessUpdateCheckOperation> {
        self.harness_update_check_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AppError::new("harnessUpdateCheckBusy"))?;
        Ok(HarnessUpdateCheckOperation {
            state: Arc::clone(self),
        })
    }

    fn desktop_updater(self: &Arc<Self>) -> AppResult<tauri_plugin_updater::Updater> {
        let state = Arc::clone(self);
        self.app
            .updater_builder()
            .on_before_exit(move || {
                // Tauri's Windows updater starts the installer and then calls
                // process::exit(0). Cleanup after Update::install is therefore
                // unreachable on Windows and must live in this hook.
                if let Err(error) = state.prepare_restart() {
                    log::error!("service cleanup before updater exit failed: {error:?}");
                }
            })
            .build()
            .map_err(|error| AppError::new("desktopUpdateFailed").detail(error.to_string()))
    }

    pub(crate) async fn check_desktop_update(
        self: &Arc<Self>,
        report_failure: bool,
    ) -> AppResult<Option<String>> {
        let _operation = self.begin_desktop_update()?;
        self.mutate(|snapshot| snapshot.desktop_update = DesktopUpdateState::Checking);
        let result = async {
            let updater = self.desktop_updater()?;
            tokio::time::timeout(DESKTOP_UPDATE_TIMEOUT, updater.check())
                .await
                .map_err(|_| AppError::new("desktopUpdateCheckTimedOut"))?
                .map_err(|error| {
                    AppError::new("desktopUpdateCheckFailed").detail(error.to_string())
                })
        }
        .await;

        match result {
            Ok(Some(update)) => {
                let version = update.version;
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Available {
                        version: version.clone(),
                    }
                });
                Ok(Some(version))
            }
            Ok(None) => {
                self.mutate(|snapshot| snapshot.desktop_update = DesktopUpdateState::Idle);
                Ok(None)
            }
            Err(error) => {
                log::warn!("desktop update check failed: {error}");
                self.mutate(|snapshot| {
                    snapshot.desktop_update = if report_failure {
                        DesktopUpdateState::Failed { version: None }
                    } else {
                        DesktopUpdateState::Idle
                    }
                });
                Err(error)
            }
        }
    }

    pub(crate) async fn install_desktop_update(self: &Arc<Self>) -> AppResult<()> {
        let _operation = self.begin_desktop_update()?;
        let previous_version = match self.snapshot().desktop_update {
            DesktopUpdateState::Available { version }
            | DesktopUpdateState::Failed {
                version: Some(version),
            } => Some(version),
            _ => None,
        };
        self.mutate(|snapshot| snapshot.desktop_update = DesktopUpdateState::Checking);
        let updater = match self.desktop_updater() {
            Ok(updater) => updater,
            Err(error) => {
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Failed {
                        version: previous_version,
                    }
                });
                return Err(error);
            }
        };
        let checked = tokio::time::timeout(DESKTOP_UPDATE_TIMEOUT, updater.check()).await;
        let update = match checked {
            Err(_) => {
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Failed {
                        version: previous_version,
                    }
                });
                return Err(AppError::new("desktopUpdateCheckTimedOut"));
            }
            Ok(Ok(Some(update))) => update,
            Ok(Ok(None)) => {
                self.mutate(|snapshot| snapshot.desktop_update = DesktopUpdateState::Idle);
                return Err(AppError::new("desktopUpdateNotAvailable"));
            }
            Ok(Err(error)) => {
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Failed {
                        version: previous_version,
                    }
                });
                return Err(AppError::new("desktopUpdateCheckFailed").detail(error.to_string()));
            }
        };

        // Always use the version returned by this fresh check. If a newer
        // release appeared after the prompt, it replaces the stale version
        // instead of trapping the user in a permanent mismatch loop.
        let version = update.version.clone();
        self.mutate(|snapshot| {
            snapshot.desktop_update = DesktopUpdateState::Downloading {
                version: version.clone(),
                done: 0,
                total: None,
            }
        });
        let progress_version = version.clone();
        let weak = Arc::downgrade(self);
        let downloaded = tokio::time::timeout(
            DESKTOP_UPDATE_DOWNLOAD_TIMEOUT,
            update.download(
                move |chunk, total| {
                    if let Some(state) = weak.upgrade() {
                        state.mutate(|snapshot| {
                            let done = match &snapshot.desktop_update {
                                DesktopUpdateState::Downloading { done, .. } => *done,
                                _ => 0,
                            }
                            .saturating_add(chunk as u64);
                            snapshot.desktop_update = DesktopUpdateState::Downloading {
                                version: progress_version.clone(),
                                done,
                                total,
                            };
                        });
                    }
                },
                || {},
            ),
        )
        .await;
        let bytes = match downloaded {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(error)) => {
                log::warn!("desktop update download failed: {error}");
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Failed {
                        version: Some(version.clone()),
                    }
                });
                return Err(AppError::new("desktopUpdateDownloadFailed").detail(error.to_string()));
            }
            Err(_) => {
                log::warn!("desktop update download timed out");
                self.mutate(|snapshot| {
                    snapshot.desktop_update = DesktopUpdateState::Failed {
                        version: Some(version.clone()),
                    }
                });
                return Err(AppError::new("desktopUpdateDownloadTimedOut"));
            }
        };

        self.mutate(|snapshot| {
            snapshot.desktop_update = DesktopUpdateState::Installing {
                version: version.clone(),
            }
        });
        if let Err(error) = update.install(bytes) {
            log::warn!("desktop update install failed: {error}");
            self.mutate(|snapshot| {
                snapshot.desktop_update = DesktopUpdateState::Failed {
                    version: Some(version),
                }
            });
            return Err(AppError::new("desktopUpdateFailed").detail(error.to_string()));
        }

        // On Windows Update::install exits the process after invoking the
        // on_before_exit hook, so this branch is reached only on macOS/Linux.
        self.prepare_restart()?;
        self.app.restart();
    }
    pub(crate) fn prepare_restart(self: &Arc<Self>) -> AppResult<()> {
        self.quitting.store(true, Ordering::SeqCst);
        let deployment = self.cancel_deployment();
        let stopped = self.complete_process_cleanup(deployment);
        match stopped {
            Ok(()) => {
                self.hide_tray_before_exit();
                self.exit_ready.store(true, Ordering::SeqCst);
                Ok(())
            }
            Err(error) => {
                self.quitting.store(false, Ordering::SeqCst);
                Err(error)
            }
        }
    }
    pub(crate) fn quit(self: &Arc<Self>) {
        if self.quitting.swap(true, Ordering::SeqCst) {
            return;
        }
        let deployment = self.cancel_deployment();
        self.mutate(|snapshot| snapshot.phase = LauncherPhase::Stopping);
        let state = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("launcher-shutdown".into())
            .spawn(move || match state.complete_process_cleanup(deployment) {
                Ok(()) => state.exit_after_cleanup(),
                Err(error) => state.shutdown_failed(error),
            })
        {
            log::error!("shutdown worker could not start: {error}");
            let deployment = self.cancel_deployment();
            match self.complete_process_cleanup(deployment) {
                Ok(()) => self.exit_after_cleanup(),
                Err(error) => self.shutdown_failed(error),
            }
        }
    }

    fn shutdown_failed(&self, error: AppError) {
        log::error!("service cleanup failed; application exit was cancelled: {error:?}");
        self.exit_ready.store(false, Ordering::SeqCst);
        self.quitting.store(false, Ordering::SeqCst);
        self.fail(error);
        show_main_window(&self.app);
    }

    fn exit_after_cleanup(&self) {
        self.hide_tray_before_exit();
        self.exit_ready.store(true, Ordering::SeqCst);
        self.app.exit(0);
    }

    fn hide_tray_before_exit(&self) {
        if let Some(tray) = self.tray.lock().expect("tray poisoned").as_ref()
            && let Err(error) = tray.set_visible(false)
        {
            log::warn!("system tray icon could not be removed before exit: {error}");
        }
    }

    fn cancel_deployment(&self) -> Option<DeploymentController> {
        let controller = self
            .deployment
            .lock()
            .expect("deployment poisoned")
            .as_ref()
            .cloned();
        if let Some(controller) = controller.as_ref() {
            controller.cancel();
        }
        controller
    }

    fn complete_process_cleanup(&self, deployment: Option<DeploymentController>) -> AppResult<()> {
        self.join_startup();
        let deployment_error = deployment.and_then(|controller| controller.cleanup_error());
        self.server.lock().expect("server poisoned").stop()?;
        if let Some(error) = deployment_error {
            return Err(error);
        }
        Ok(())
    }

    fn join_startup(&self) {
        let worker = self
            .startup_thread
            .lock()
            .expect("startup thread poisoned")
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    fn refresh_tray_menu(&self, language: Language) -> tauri::Result<()> {
        let menu = tray_menu(&self.app, language)?;
        if let Some(tray) = self.tray.lock().expect("tray poisoned").as_ref() {
            tray.set_menu(Some(menu))?;
        }
        Ok(())
    }
}

fn mark_harness_update_checking(
    snapshot: &mut LauncherSnapshot,
    expected: &HarnessUpdateState,
) -> bool {
    if !matches!(
        snapshot.phase,
        LauncherPhase::Ready | LauncherPhase::Stopped
    ) || &snapshot.harness_update != expected
    {
        return false;
    }
    snapshot.harness_update = HarnessUpdateState::Checking;
    true
}

fn replace_harness_update_if_checking(
    snapshot: &mut LauncherSnapshot,
    replacement: HarnessUpdateState,
) -> bool {
    if snapshot.harness_update == HarnessUpdateState::Checking {
        snapshot.harness_update = replacement;
        true
    } else {
        false
    }
}

fn acquire_instance_lock(paths: &ApplicationPaths) -> AppResult<File> {
    acquire_instance_lock_with_timeout(paths, INSTANCE_LOCK_TIMEOUT)
}

fn acquire_instance_lock_with_timeout(
    paths: &ApplicationPaths,
    timeout: Duration,
) -> AppResult<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.launcher_lock)
        .map_err(|error| AppError::io("launcherLockFailed", &error))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == lock_contended_error().kind() => {
                if std::time::Instant::now() >= deadline {
                    return Err(AppError::new("launcherAlreadyRunning"));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(AppError::io("launcherLockFailed", &error)),
        }
    }
}

struct DesktopUpdateOperation {
    state: Arc<AppState>,
}

struct HarnessUpdateCheckOperation {
    state: Arc<AppState>,
}

impl Drop for HarnessUpdateCheckOperation {
    fn drop(&mut self) {
        self.state
            .harness_update_check_busy
            .store(false, Ordering::SeqCst);
    }
}

impl Drop for DesktopUpdateOperation {
    fn drop(&mut self) {
        self.state
            .desktop_update_busy
            .store(false, Ordering::SeqCst);
    }
}

fn tray_menu(app: &AppHandle, language: Language) -> tauri::Result<Menu<tauri::Wry>> {
    let (show, open_web, quit) = match language {
        Language::Zh => ("打开启动主页面", "打开 DeepSeek Harness 工作台", "退出"),
        Language::En => ("Open launcher", "Open DeepSeek Harness Workspace", "Quit"),
    };
    let show = MenuItem::with_id(app, "open", show, true, None::<&str>)?;
    let open_web = MenuItem::with_id(app, "web", open_web, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", quit, true, None::<&str>)?;
    Menu::with_items(app, &[&show, &open_web, &quit])
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(error) = window.show() {
            log::warn!("main window could not be shown: {error}");
            return;
        }
        if let Err(error) = window.set_focus() {
            log::warn!("main window could not be focused: {error}");
        }
    }
}

fn install_tray(app: &tauri::App, state: &Arc<AppState>) -> tauri::Result<()> {
    let menu = tray_menu(app.handle(), state.snapshot().language)?;
    let weak_menu = Arc::downgrade(state);
    let tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().expect("bundle icon").clone())
        .tooltip("DSH Launcher")
        .menu(&menu)
        .on_tray_icon_event(move |tray, event| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(move |app, event| {
            if let Some(state) = weak_menu.upgrade() {
                match event.id().as_ref() {
                    "open" => show_main_window(app),
                    "web" => {
                        let _ = state.open_web_ui();
                    }
                    "quit" => state.quit(),
                    _ => {}
                }
            }
        })
        .build(app)?;
    *state.tray.lock().expect("tray poisoned") = Some(tray);
    state.mutate(|snapshot| snapshot.tray_available = true);
    Ok(())
}

async fn check_desktop_update_after_startup(state: Arc<AppState>) {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = state.check_desktop_update(false).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleDecision {
    KeepRunning,
    QuitAfterCleanup,
    AllowExit,
}

fn lifecycle_decision(exit_ready: bool, tray_available: bool) -> LifecycleDecision {
    if exit_ready {
        LifecycleDecision::AllowExit
    } else if tray_available {
        LifecycleDecision::KeepRunning
    } else {
        LifecycleDecision::QuitAfterCleanup
    }
}

pub fn run() {
    if dsh_core::service::handle_service_guard_cli() || handle_cli_probe() {
        return;
    }
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let paths = ApplicationPaths::from_environment().map_err(|error| error.to_string())?;
            paths.ensure_dirs().map_err(|error| error.to_string())?;
            let state =
                AppState::new(app.handle().clone(), paths).map_err(|error| error.to_string())?;
            if let Err(error) = install_tray(app, &state) {
                log::warn!("system tray unavailable; closing the window will exit: {error}");
            }
            app.manage(Arc::clone(&state));
            state.start(false, None);
            tauri::async_runtime::spawn(check_desktop_update_after_startup(state));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && let Some(state) = window.app_handle().try_state::<Arc<AppState>>()
            {
                match lifecycle_decision(
                    state.exit_ready.load(Ordering::SeqCst),
                    state.snapshot().tray_available,
                ) {
                    LifecycleDecision::KeepRunning => {
                        api.prevent_close();
                        if let Err(error) = window.hide() {
                            log::warn!("main window could not be hidden: {error}");
                        }
                    }
                    LifecycleDecision::QuitAfterCleanup => {
                        api.prevent_close();
                        state.quit();
                    }
                    LifecycleDecision::AllowExit => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::launcher_get_snapshot,
            commands::launcher_retry,
            commands::launcher_stop,
            commands::launcher_restart,
            commands::launcher_check_harness_update,
            commands::launcher_update_harness,
            commands::migration_approve,
            commands::migration_skip,
            commands::launcher_select_browser,
            commands::launcher_open_web_ui,
            commands::application_open_website,
            commands::application_open_external_link,
            commands::application_copy_web_url,
            commands::preferences_set_language,
            commands::preferences_set_theme,
            commands::application_check_update,
            commands::application_install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building DSH Launcher")
        .run(|app, event| match event {
            #[cfg(target_os = "macos")]
            RunEvent::Reopen {
                has_visible_windows,
                ..
            } => {
                if !has_visible_windows {
                    show_main_window(app);
                }
            }
            RunEvent::ExitRequested { api, .. } => {
                if let Some(state) = app.try_state::<Arc<AppState>>() {
                    match lifecycle_decision(
                        state.exit_ready.load(Ordering::SeqCst),
                        state.snapshot().tray_available,
                    ) {
                        LifecycleDecision::KeepRunning => api.prevent_exit(),
                        LifecycleDecision::QuitAfterCleanup => {
                            api.prevent_exit();
                            state.quit();
                        }
                        LifecycleDecision::AllowExit => {}
                    }
                }
            }
            _ => {}
        });
}

fn handle_cli_probe() -> bool {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.iter().any(|value| value == "--desktop-version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if arguments.iter().any(|value| value == "--check") {
        let result = ApplicationPaths::from_environment().and_then(|paths| {
            if std::env::var_os("DSH_DESKTOP_HOME").is_none() {
                return Err(AppError::new("checkRequiresIsolatedHome"));
            }
            paths.ensure_dirs()
        });
        match result {
            Ok(()) => println!("DSH Launcher check passed"),
            Err(error) => {
                eprintln!("DSH Launcher check failed: {error}");
                std::process::exit(1);
            }
        }
        return true;
    }
    false
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::{
        DEEPSEEK_PLATFORM, GITHUB_REPOSITORY, HARNESS_GITHUB_REPOSITORY, LifecycleDecision,
        WEBSITE, acquire_instance_lock, acquire_instance_lock_with_timeout, external_link_url,
        lifecycle_decision, mark_harness_update_checking, replace_harness_update_if_checking,
    };
    use dsh_core::{ApplicationPaths, HarnessUpdateState, LauncherSnapshot};

    #[test]
    fn product_website_uses_the_public_homepage() {
        assert_eq!(WEBSITE, "https://dsdesktop.com/");
    }

    #[test]
    fn external_links_are_limited_to_known_destinations() {
        assert_eq!(external_link_url("github"), Some(GITHUB_REPOSITORY));
        assert_eq!(
            external_link_url("harnessGithub"),
            Some(HARNESS_GITHUB_REPOSITORY)
        );
        assert_eq!(external_link_url("deepseek"), Some(DEEPSEEK_PLATFORM));
        assert_eq!(external_link_url("unknown"), None);
    }

    #[test]
    fn a_stale_harness_check_cannot_replace_an_installing_state() {
        let mut snapshot = LauncherSnapshot::initial("0.2.2");
        snapshot.harness_update = HarnessUpdateState::Installing {
            version: "0.1.0-rc.8".into(),
        };

        assert!(!replace_harness_update_if_checking(
            &mut snapshot,
            HarnessUpdateState::None
        ));

        assert_eq!(
            snapshot.harness_update,
            HarnessUpdateState::Installing {
                version: "0.1.0-rc.8".into()
            }
        );
    }

    #[test]
    fn a_current_harness_check_can_publish_its_result() {
        let mut snapshot = LauncherSnapshot::initial("0.2.2");
        snapshot.harness_update = HarnessUpdateState::Checking;

        assert!(replace_harness_update_if_checking(
            &mut snapshot,
            HarnessUpdateState::Available {
                version: "0.1.0-rc.8".into(),
            },
        ));

        assert_eq!(
            snapshot.harness_update,
            HarnessUpdateState::Available {
                version: "0.1.0-rc.8".into()
            }
        );
    }

    #[test]
    fn a_stale_harness_check_cannot_enter_checking() {
        let mut snapshot = LauncherSnapshot::initial("0.2.2");
        snapshot.phase = dsh_core::LauncherPhase::Ready;
        snapshot.harness_update = HarnessUpdateState::Installing {
            version: "0.1.0-rc.8".into(),
        };

        assert!(!mark_harness_update_checking(
            &mut snapshot,
            &HarnessUpdateState::Available {
                version: "0.1.0-rc.8".into(),
            }
        ));
        assert!(matches!(
            snapshot.harness_update,
            HarnessUpdateState::Installing { .. }
        ));
    }

    #[test]
    fn a_stopped_service_can_enter_harness_update_checking() {
        let mut snapshot = LauncherSnapshot::initial("0.2.2");
        snapshot.phase = dsh_core::LauncherPhase::Stopped;

        assert!(mark_harness_update_checking(
            &mut snapshot,
            &HarnessUpdateState::None
        ));
        assert_eq!(snapshot.harness_update, HarnessUpdateState::Checking);
    }

    #[test]
    fn instance_lock_allows_only_one_launcher_per_desktop_home() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ApplicationPaths::from_home(temp.path().join("desktop"));
        paths.ensure_dirs().unwrap();
        let first = acquire_instance_lock(&paths).unwrap();
        let error = acquire_instance_lock_with_timeout(&paths, Duration::ZERO).unwrap_err();
        assert_eq!(error.code, "launcherAlreadyRunning");
        drop(first);
        acquire_instance_lock(&paths).unwrap();
    }

    #[test]
    fn instance_lock_waits_for_a_restarting_launcher_to_exit() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ApplicationPaths::from_home(temp.path().join("desktop"));
        paths.ensure_dirs().unwrap();
        let first = acquire_instance_lock(&paths).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(first);
        });

        acquire_instance_lock_with_timeout(&paths, Duration::from_secs(1)).unwrap();
        release.join().unwrap();
    }

    #[test]
    fn closing_or_requesting_exit_keeps_the_tray_process_running() {
        assert_eq!(
            lifecycle_decision(false, true),
            LifecycleDecision::KeepRunning
        );
    }

    #[test]
    fn closing_without_a_tray_quits_after_cleanup() {
        assert_eq!(
            lifecycle_decision(false, false),
            LifecycleDecision::QuitAfterCleanup
        );
    }

    #[test]
    fn cleanup_in_progress_does_not_allow_the_process_to_exit() {
        assert_eq!(
            lifecycle_decision(false, true),
            LifecycleDecision::KeepRunning
        );
        assert_eq!(
            lifecycle_decision(false, false),
            LifecycleDecision::QuitAfterCleanup
        );
    }

    #[test]
    fn completed_cleanup_allows_the_process_to_exit() {
        assert_eq!(lifecycle_decision(true, true), LifecycleDecision::AllowExit);
        assert_eq!(
            lifecycle_decision(true, false),
            LifecycleDecision::AllowExit
        );
    }
}
