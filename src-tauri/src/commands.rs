use std::sync::Arc;

use dsh_core::{AppError, Language, LauncherSnapshot, ThemePreference};
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::application::AppState;

#[tauri::command]
pub fn launcher_get_snapshot(state: State<'_, Arc<AppState>>) -> LauncherSnapshot {
    state.snapshot()
}

#[tauri::command]
pub fn launcher_retry(state: State<'_, Arc<AppState>>) {
    state.inner().start(false, None);
}

#[tauri::command]
pub fn launcher_update_harness(state: State<'_, Arc<AppState>>) {
    state.inner().update_harness();
}

#[tauri::command]
pub async fn launcher_check_harness_update(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, AppError> {
    state.inner().check_harness_update().await
}

#[tauri::command]
pub fn migration_approve(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    state.inner().approve_migration()
}

#[tauri::command]
pub fn migration_skip(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    state.inner().skip_migration()
}

#[tauri::command]
pub fn launcher_select_browser(
    browser_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), AppError> {
    state.select_browser(browser_id)
}

#[tauri::command]
pub fn launcher_open_web_ui(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    state.open_web_ui()
}

#[tauri::command]
pub fn application_open_website(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    state.open_website()
}

#[tauri::command]
pub fn application_open_external_link(
    target: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), AppError> {
    state.open_external_link(&target)
}

#[tauri::command]
pub fn application_copy_web_url(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), AppError> {
    app.clipboard()
        .write_text(state.web_url()?)
        .map_err(|error| AppError::new("clipboardFailed").detail(error.to_string()))
}

#[tauri::command]
pub fn preferences_set_language(
    language: Language,
    state: State<'_, Arc<AppState>>,
) -> Result<(), AppError> {
    state.set_language(language)
}

#[tauri::command]
pub fn preferences_set_theme(
    theme: ThemePreference,
    state: State<'_, Arc<AppState>>,
) -> Result<(), AppError> {
    state.set_theme(theme)
}

#[tauri::command]
pub async fn application_check_update(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, AppError> {
    state.inner().check_desktop_update(true).await
}

#[tauri::command]
pub async fn application_install_update(state: State<'_, Arc<AppState>>) -> Result<(), AppError> {
    state.inner().install_desktop_update().await
}
