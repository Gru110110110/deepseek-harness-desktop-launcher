use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    net::IpAddr,
    path::Path,
};

use crate::{AppError, AppResult, paths::atomic_write};
use regex::Regex;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const MAX_PROVIDERS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcSwitchImportResult {
    pub imported: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
struct Provider {
    route: String,
    display_name: String,
    credential_ref: String,
    credential: String,
    api: String,
    base_url: String,
    models: Vec<String>,
}

type ParsedDocument = (Option<Map<String, Value>>, Option<Vec<u8>>, HashSet<String>);

pub fn discover_cc_switch_providers(cc_switch_home: &Path) -> u32 {
    let database = cc_switch_home.join("cc-switch.db");
    if database.is_symlink() || !database.is_file() {
        return 0;
    }
    read_providers(&database)
        .map(|(providers, _)| providers.len().min(u32::MAX as usize) as u32)
        .unwrap_or(0)
}

pub fn import_cc_switch_configuration(
    cc_switch_home: &Path,
    dsh_home: &Path,
    marker: &Path,
) -> AppResult<CcSwitchImportResult> {
    if fs::read(marker).is_ok_and(|bytes| bytes == b"1\n") {
        return Ok(outcome(false, "ccSwitchAlreadyImported"));
    }
    if marker.exists() || marker.is_symlink() {
        return Ok(outcome(false, "ccSwitchUnknownMarkerPreserved"));
    }
    if dsh_home.is_symlink() {
        complete(marker)?;
        return Ok(outcome(false, "ccSwitchUnsupportedTargetsPreserved"));
    }
    let settings = dsh_home.join("settings.yaml");
    let credentials = dsh_home.join(".credentials.yaml");
    if unsupported_target(&settings) || unsupported_target(&credentials) {
        complete(marker)?;
        return Ok(outcome(false, "ccSwitchUnsupportedTargetsPreserved"));
    }
    let old_settings = fs::read(&settings).ok();
    let old_credentials = fs::read(&credentials).ok();
    let database = cc_switch_home.join("cc-switch.db");
    if database.is_symlink() || !database.is_file() {
        complete(marker)?;
        return Ok(outcome(false, "ccSwitchDatabaseMissing"));
    }
    let (providers, skipped) = match read_providers(&database) {
        Ok(value) => value,
        Err(error) => {
            complete(marker)?;
            return Err(error);
        }
    };
    let Some((settings_bytes, credential_bytes, additions)) = merge_documents(
        &providers,
        old_settings.as_deref(),
        old_credentials.as_deref(),
    ) else {
        complete(marker)?;
        return Ok(outcome(false, "ccSwitchNoSafeAdditions"));
    };
    fs::create_dir_all(dsh_home)?;
    if let Err(error) = publish_candidate(&settings, &settings_bytes, old_settings.as_deref()) {
        complete(marker)?;
        return Err(error);
    }
    if let Err(error) =
        publish_candidate(&credentials, &credential_bytes, old_credentials.as_deref())
    {
        restore(&settings, old_settings.as_deref())?;
        complete(marker)?;
        return Err(error);
    }
    if let Err(error) = complete(marker) {
        restore(&credentials, old_credentials.as_deref())?;
        restore(&settings, old_settings.as_deref())?;
        return Err(error);
    }
    Ok(outcome(
        true,
        &format!(
            "ccSwitchCompleted:imported={additions};skipped={}",
            skipped + providers.len() - additions
        ),
    ))
}

fn read_providers(database: &Path) -> AppResult<(Vec<Provider>, usize)> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| AppError::new("ccSwitchDatabaseUnreadable").detail(error.to_string()))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(db_error)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(1))
        .map_err(db_error)?;
    let columns: HashSet<String> = {
        let mut statement = connection
            .prepare("PRAGMA table_info(providers)")
            .map_err(db_error)?;
        statement
            .query_map([], |row| row.get(1))
            .map_err(db_error)?
            .filter_map(Result::ok)
            .collect()
    };
    if !["id", "app_type", "name", "settings_config"]
        .iter()
        .all(|key| columns.contains(*key))
    {
        return Ok((Vec::new(), 0));
    }
    let meta = if columns.contains("meta") {
        "meta"
    } else {
        "NULL AS meta"
    };
    let order = if columns.contains("is_current") {
        "is_current DESC, name, id"
    } else {
        "name, id"
    };
    let sql = format!(
        "SELECT id, name, settings_config, {meta} FROM providers WHERE app_type = ? ORDER BY {order} LIMIT ?"
    );
    let mut statement = connection.prepare(&sql).map_err(db_error)?;
    let rows = statement
        .query_map(("claude", MAX_PROVIDERS as i64), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(db_error)?;
    let mut providers = Vec::new();
    let mut total: usize = 0;
    let mut routes = HashSet::new();
    for row in rows {
        total += 1;
        let (id, name, settings, meta) = row.map_err(db_error)?;
        if let Some(provider) = translate(&id, &name, &settings, meta.as_deref())
            && routes.insert(provider.route.clone())
        {
            providers.push(provider);
        }
    }
    Ok((providers, total.saturating_sub(total.min(routes.len()))))
}

fn translate(
    id: &str,
    name: &str,
    settings_text: &str,
    meta_text: Option<&str>,
) -> Option<Provider> {
    let id = clean(id, 512)?;
    let name = clean(name, 120)?;
    let settings: Value = serde_json::from_str(settings_text).ok()?;
    let settings = settings.as_object()?;
    let meta_value = meta_text
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| json!({}));
    let meta = meta_value.as_object()?;
    let provider_type = meta
        .get("providerType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ["codex_oauth", "github_copilot", "xai_oauth"].contains(&provider_type.as_str())
        || meta.get("isFullUrl") == Some(&Value::Bool(true))
        || meta.get("localProxyRequestOverrides").is_some()
        || settings.get("localProxyRequestOverrides").is_some()
    {
        return None;
    }
    if meta
        .get("authBinding")
        .and_then(Value::as_object)
        .and_then(|item| item.get("source"))
        .and_then(Value::as_str)
        == Some("managed_account")
    {
        return None;
    }
    if settings
        .get("auth_mode")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("bearer_only"))
    {
        return None;
    }
    let env = settings.get("env")?.as_object()?;
    let base_url = usable_url(env.get("ANTHROPIC_BASE_URL")?.as_str()?)?;
    let credential = [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "GOOGLE_API_KEY",
    ]
    .iter()
    .filter_map(|key| env.get(*key)?.as_str())
    .find_map(|value| clean(value, 16_384).filter(|value| value != "PROXY_MANAGED"))?;
    let raw_api = meta
        .get("apiFormat")
        .or_else(|| settings.get("api_format"))
        .or_else(|| settings.get("apiFormat"))
        .and_then(Value::as_str)
        .unwrap_or("anthropic")
        .to_ascii_lowercase();
    let api = match raw_api.as_str() {
        "anthropic" | "anthropic_messages" => "anthropic-messages",
        "openai_chat" | "openai_completions" => "openai-completions",
        "openai_responses" => "openai-responses",
        _ => return None,
    };
    let mut models = Vec::new();
    for key in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_REASONING_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        if let Some(model) = env
            .get(key)
            .and_then(Value::as_str)
            .and_then(|item| clean(item, 256))
            && !models.contains(&model)
        {
            models.push(model);
        }
    }
    if let Some(values) = settings.get("models").and_then(Value::as_array) {
        for value in values {
            let raw = value
                .as_str()
                .or_else(|| value.get("id").and_then(Value::as_str));
            if let Some(model) = raw.and_then(|item| clean(item, 256))
                && !models.contains(&model)
                && models.len() < 64
            {
                models.push(model);
            }
        }
    }
    if let Some(values) = settings.get("models").and_then(Value::as_object) {
        for raw in values.keys() {
            if let Some(model) = clean(raw, 256)
                && !models.contains(&model)
                && models.len() < 64
            {
                models.push(model);
            }
        }
    }
    if models.is_empty() {
        return None;
    }
    let digest = Sha256::digest(id.as_bytes());
    let digest = hex::encode(digest);
    let slug_re = Regex::new("[^a-z0-9]+").expect("static regex");
    let slug = slug_re
        .replace_all(&name.to_ascii_lowercase(), "-")
        .trim_matches('-')
        .chars()
        .take(32)
        .collect::<String>();
    Some(Provider {
        route: format!(
            "ccswitch-{}-{}",
            if slug.is_empty() { "provider" } else { &slug },
            &digest[..8]
        ),
        display_name: format!("{name} (CC Switch)"),
        credential_ref: format!("CCSWITCH_{}_API_KEY", digest[..12].to_ascii_uppercase()),
        credential,
        api: api.into(),
        base_url,
        models,
    })
}

fn merge_documents(
    providers: &[Provider],
    old_settings: Option<&[u8]>,
    old_credentials: Option<&[u8]>,
) -> Option<(Vec<u8>, Vec<u8>, usize)> {
    let (mut settings_json, settings_yaml, existing_routes) = parse_settings(old_settings)?;
    let (mut credentials_json, credentials_yaml, existing_credentials) =
        parse_credentials(old_credentials)?;
    let additions: Vec<_> = providers
        .iter()
        .filter(|item| {
            !existing_routes.contains(&item.route)
                || !existing_credentials.contains(&item.credential_ref)
        })
        .collect();
    if additions.is_empty() {
        return None;
    }
    let mut provider_values = Map::new();
    let mut credential_values = BTreeMap::new();
    for provider in &additions {
        if !existing_routes.contains(&provider.route) {
            provider_values.insert(
                provider.route.clone(),
                json!({
                    "displayName": provider.display_name, "apiKeyEnv": provider.credential_ref,
                    "api": provider.api, "baseURL": provider.base_url,
                    "models": provider.models.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()
                }),
            );
        }
        if !existing_credentials.contains(&provider.credential_ref) {
            credential_values.insert(provider.credential_ref.clone(), provider.credential.clone());
        }
    }
    let settings_bytes = if provider_values.is_empty() {
        old_settings?.to_vec()
    } else if let Some(existing) = settings_yaml {
        append_yaml(
            existing,
            format!(
                "llm-pi-ai: {}",
                serde_json::to_string(&json!({"providers": provider_values})).ok()?
            ),
        )
    } else {
        let root = settings_json.as_mut()?;
        let namespace = root
            .entry("llm-pi-ai")
            .or_insert_with(|| json!({}))
            .as_object_mut()?;
        let routes = namespace
            .entry("providers")
            .or_insert_with(|| json!({}))
            .as_object_mut()?;
        routes.extend(provider_values);
        pretty_json(root)?
    };
    let credentials_bytes = if credential_values.is_empty() {
        old_credentials?.to_vec()
    } else if let Some(mut existing) = credentials_yaml {
        let lines = credential_values
            .into_iter()
            .map(|(key, value)| {
                format!(
                    "{key}: {}",
                    serde_json::to_string(&value).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        existing = append_yaml(existing, lines);
        existing
    } else {
        let root = credentials_json.as_mut()?;
        for (key, value) in credential_values {
            root.insert(key, Value::String(value));
        }
        pretty_json(root)?
    };
    Some((settings_bytes, credentials_bytes, additions.len()))
}

fn parse_settings(content: Option<&[u8]>) -> Option<ParsedDocument> {
    let Some(content) = content else {
        return Some((Some(Map::new()), None, HashSet::new()));
    };
    if let Ok(Value::Object(root)) = serde_json::from_slice(content) {
        let routes = match root.get("llm-pi-ai") {
            None => HashSet::new(),
            Some(Value::Object(namespace)) => match namespace.get("providers") {
                None => HashSet::new(),
                Some(Value::Object(routes)) => routes.keys().cloned().collect(),
                _ => return None,
            },
            _ => return None,
        };
        return Some((Some(root), None, routes));
    }
    let keys = yaml_top_keys(content)?;
    if keys.contains("llm-pi-ai") {
        return None;
    }
    Some((None, Some(content.to_vec()), HashSet::new()))
}

fn parse_credentials(content: Option<&[u8]>) -> Option<ParsedDocument> {
    let Some(content) = content else {
        return Some((Some(Map::new()), None, HashSet::new()));
    };
    if let Ok(Value::Object(root)) = serde_json::from_slice(content) {
        if root.values().any(|value| !value.is_string()) {
            return None;
        }
        let keys = root.keys().cloned().collect();
        return Some((Some(root), None, keys));
    }
    let keys = yaml_top_keys(content)?;
    Some((None, Some(content.to_vec()), keys))
}

fn yaml_top_keys(content: &[u8]) -> Option<HashSet<String>> {
    let text = std::str::from_utf8(content).ok()?;
    let key = Regex::new(r"^([A-Za-z_][A-Za-z0-9_.-]*)\s*:").ok()?;
    let mut keys = HashSet::new();
    for line in text.lines() {
        if line.trim().is_empty()
            || line.trim_start().starts_with('#')
            || line.starts_with(char::is_whitespace)
        {
            continue;
        }
        if line == "---" {
            continue;
        }
        if line == "..." || line.starts_with(['%', '-', '{', '[', '?']) {
            return None;
        }
        let found = key.captures(line)?.get(1)?.as_str().to_owned();
        if !keys.insert(found) {
            return None;
        }
    }
    Some(keys)
}

fn append_yaml(mut bytes: Vec<u8>, text: String) -> Vec<u8> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(b'\n');
    bytes
}
fn pretty_json(root: &Map<String, Value>) -> Option<Vec<u8>> {
    let mut result = serde_json::to_vec_pretty(root).ok()?;
    result.push(b'\n');
    Some(result)
}

fn publish_candidate(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> AppResult<()> {
    if let Some(expected) = expected {
        if path.is_symlink() || !fs::read(path).is_ok_and(|value| value == expected) {
            return Err(AppError::new("configurationChanged"));
        }
        atomic_write(path, bytes)
    } else {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        owner_only(path)?;
        Ok(())
    }
}

fn restore(path: &Path, original: Option<&[u8]>) -> AppResult<()> {
    match original {
        Some(bytes) => atomic_write(path, bytes),
        None => {
            if path.exists() {
                fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}
fn complete(marker: &Path) -> AppResult<()> {
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?;
    file.write_all(b"1\n")?;
    file.sync_all()?;
    owner_only(marker)?;
    Ok(())
}
#[cfg(unix)]
fn owner_only(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
fn owner_only(_path: &Path) -> AppResult<()> {
    Ok(())
}
fn unsupported_target(path: &Path) -> bool {
    path.is_symlink() || (path.exists() && !path.is_file())
}
fn clean(value: &str, maximum: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}
fn usable_url(raw: &str) -> Option<String> {
    let value = url::Url::parse(raw).ok()?;
    if !["http", "https"].contains(&value.scheme())
        || !value.username().is_empty()
        || value.password().is_some()
        || value.query().is_some()
        || value.fragment().is_some()
    {
        return None;
    }
    let host = value.host_str()?.trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    {
        return None;
    }
    Some(raw.trim_end_matches('/').to_owned())
}
fn db_error(error: rusqlite::Error) -> AppError {
    AppError::new("ccSwitchDatabaseUnreadable").detail(error.to_string())
}
fn outcome(imported: bool, message: &str) -> CcSwitchImportResult {
    CcSwitchImportResult {
        imported,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_provider_separates_settings_from_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let cc_home = temp.path().join("cc-switch");
        let dsh_home = temp.path().join("dsh-home");
        fs::create_dir_all(&cc_home).unwrap();
        let connection = Connection::open(cc_home.join("cc-switch.db")).unwrap();
        connection.execute_batch("CREATE TABLE providers (id TEXT, app_type TEXT, name TEXT, settings_config TEXT, meta TEXT, is_current INTEGER);").unwrap();
        let settings = json!({"env": {
            "ANTHROPIC_BASE_URL": "https://api.example.test",
            "ANTHROPIC_API_KEY": "test-secret-not-real",
            "ANTHROPIC_MODEL": "test-model"
        }})
        .to_string();
        connection
            .execute(
                "INSERT INTO providers VALUES (?1, 'claude', 'Example', ?2, '{}', 1)",
                ("provider-1", settings),
            )
            .unwrap();
        drop(connection);

        let marker = temp.path().join("marker");
        let result = import_cc_switch_configuration(&cc_home, &dsh_home, &marker).unwrap();
        assert!(result.imported);
        let public = fs::read_to_string(dsh_home.join("settings.yaml")).unwrap();
        let credentials = fs::read_to_string(dsh_home.join(".credentials.yaml")).unwrap();
        assert!(public.contains("api.example.test"));
        assert!(!public.contains("test-secret-not-real"));
        assert!(credentials.contains("test-secret-not-real"));
        assert_eq!(fs::read(marker).unwrap(), b"1\n");
    }

    #[test]
    fn managed_provider_is_not_imported() {
        let settings = json!({"env": {
            "ANTHROPIC_BASE_URL": "https://api.example.test",
            "ANTHROPIC_API_KEY": "test-secret-not-real",
            "ANTHROPIC_MODEL": "test-model"
        }})
        .to_string();
        assert!(
            translate(
                "provider",
                "Managed",
                &settings,
                Some(r#"{"providerType":"codex_oauth"}"#)
            )
            .is_none()
        );
    }

    #[test]
    fn interrupted_import_repairs_a_missing_credential() {
        let provider = Provider {
            route: "route".into(),
            display_name: "Example".into(),
            credential_ref: "EXAMPLE_KEY".into(),
            credential: "test-secret-not-real".into(),
            api: "anthropic-messages".into(),
            base_url: "https://api.example.test".into(),
            models: vec!["test-model".into()],
        };
        let settings = br#"{"llm-pi-ai":{"providers":{"route":{"apiKeyEnv":"EXAMPLE_KEY"}}}}"#;
        let (new_settings, credentials, additions) =
            merge_documents(&[provider], Some(settings), None).unwrap();
        assert_eq!(new_settings, settings);
        assert_eq!(additions, 1);
        assert!(
            String::from_utf8(credentials)
                .unwrap()
                .contains("test-secret-not-real")
        );
    }
}
