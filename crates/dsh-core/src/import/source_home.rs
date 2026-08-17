use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;
use uuid::Uuid;

use crate::{AppResult, paths::atomic_write};

const HISTORY: [&str; 2] = ["attachments", "sessions"];
const EXCLUDED: [&str; 2] = [".anonymous-user-id", "storages"];

pub fn discover_source_entries(source: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(source) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            entry
                .file_type()
                .is_ok_and(|kind| (kind.is_file() || kind.is_dir()) && !kind.is_symlink())
                && portable_name(&name)
        })
        .take(u32::MAX as usize)
        .count() as u32
}

pub fn discover_source_workspace(source: &Path) -> bool {
    read_workspace(&source.join("storages/workspace.json")).is_some_and(|(_, empty)| !empty)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub copied: bool,
    pub message: String,
}

pub fn import_source_home(
    source: &Path,
    destination: &Path,
    marker: &Path,
) -> AppResult<ImportResult> {
    let source = absolute(source)?;
    let destination = absolute(destination)?;
    if overlaps(&source, &destination) || destination.is_symlink() {
        return Ok(result(false, "sourceHomeOverlap"));
    }
    fs::create_dir_all(&destination)?;
    if canonical_overlaps(&source, &destination) {
        return Ok(result(false, "sourceHomeOverlap"));
    }
    let completed = marker_complete(marker);
    if completed && has_configuration(&destination)? {
        return Ok(result(false, "sourceHomeAlreadyImported"));
    }
    if !completed && (marker.exists() || marker.is_symlink()) {
        return Ok(result(false, "sourceHomeUnknownMarkerPreserved"));
    }
    if !source.is_dir() {
        return Ok(result(false, "sourceHomeMissing"));
    }

    let destination_has_history = HISTORY.iter().any(|name| destination.join(name).exists());
    let staging = destination
        .parent()
        .unwrap_or(&destination)
        .join(format!(".dsh-config-import-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let mut roots = Vec::<(PathBuf, PathBuf)>::new();
    let outcome = (|| -> AppResult<ImportResult> {
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_text = name.to_string_lossy();
            let kind = entry.file_type()?;
            if kind.is_symlink()
                || (!kind.is_file() && !kind.is_dir())
                || !portable_name(&name_text)
                || (destination_has_history && HISTORY.contains(&name_text.as_ref()))
            {
                continue;
            }
            copy_portable(&entry.path(), &staging.join(&name))?;
        }
        for entry in fs::read_dir(&staging)? {
            let entry = entry?;
            collect_missing_roots(
                &entry.path(),
                &destination.join(entry.file_name()),
                &mut roots,
            )?;
        }
        let mut published = Vec::<PathBuf>::new();
        for (source, target) in &roots {
            if let Err(error) = publish_new(source, target) {
                for path in published.iter().rev() {
                    let _ = remove_owned(path);
                }
                return Err(error);
            }
            published.push(target.clone());
        }
        if published.is_empty() && !destination_has_history {
            return Ok(result(false, "sourceHomeNoPortableData"));
        }
        if let Err(error) = write_marker(marker) {
            for path in published.iter().rev() {
                let _ = remove_owned(path);
            }
            return Err(error);
        }
        Ok(ImportResult {
            copied: !published.is_empty(),
            message: format!("sourceHomeCompleted:{}", published.len()),
        })
    })();
    let _ = fs::remove_dir_all(&staging);
    outcome
}

pub fn import_source_workspace(
    source_home: &Path,
    destination_home: &Path,
    marker: &Path,
) -> AppResult<ImportResult> {
    let source = absolute(source_home)?;
    let destination = absolute(destination_home)?;
    if overlaps(&source, &destination) || destination.is_symlink() {
        return Ok(result(false, "workspaceOverlap"));
    }
    fs::create_dir_all(&destination)?;
    if canonical_overlaps(&source, &destination) {
        return Ok(result(false, "workspaceOverlap"));
    }
    if marker_complete(marker) {
        return Ok(result(false, "workspaceAlreadyImported"));
    }
    if marker.exists() || marker.is_symlink() {
        return Ok(result(false, "workspaceUnknownMarkerPreserved"));
    }
    let source_file = source.join("storages/workspace.json");
    let Some(source_bytes) = read_workspace(&source_file)
        .filter(|(_, empty)| !empty)
        .map(|(bytes, _)| bytes)
    else {
        write_marker(marker)?;
        return Ok(result(false, "workspaceNoCompatibleSource"));
    };
    let storages = destination.join("storages");
    if storages.is_symlink() || (storages.exists() && !storages.is_dir()) {
        write_marker(marker)?;
        return Ok(result(false, "workspaceDestinationPreserved"));
    }
    let target = storages.join("workspace.json");
    if target.exists() {
        match read_workspace(&target) {
            Some((_, true)) => {}
            _ => {
                write_marker(marker)?;
                return Ok(result(false, "workspaceDestinationPreserved"));
            }
        }
    }
    fs::create_dir_all(target.parent().expect("workspace parent"))?;
    let original = fs::read(&target).ok();
    if target.is_symlink() {
        write_marker(marker)?;
        return Ok(result(false, "workspaceDestinationPreserved"));
    }
    if let Err(error) = atomic_write(&target, &source_bytes).and_then(|_| write_marker(marker)) {
        match original {
            Some(bytes) => {
                let _ = atomic_write(&target, &bytes);
            }
            None => {
                let _ = fs::remove_file(&target);
            }
        }
        return Err(error);
    }
    Ok(result(true, "workspaceCompleted"))
}

fn read_workspace(path: &Path) -> Option<(Vec<u8>, bool)> {
    if path.is_symlink() || !path.is_file() {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let document: Value = serde_json::from_slice(&bytes).ok()?;
    let object = document.as_object()?;
    let unit = object.get("unit")?.as_object()?;
    if unit.get("name")?.as_str()? != "workspace" || unit.get("version")?.as_u64()? != 2 {
        return None;
    }
    let global = object.get("global")?.as_object()?;
    if !global.get("initialized")?.as_bool()? || global.contains_key("pendingMutation") {
        return None;
    }
    let ids = string_array(global.get("workspaceIds")?)?;
    let archived = string_array(
        global
            .get("archivedSessionIds")
            .unwrap_or(&Value::Array(Vec::new())),
    )?;
    let records = object.get("tables")?.get("workspaces")?.as_object()?;
    let record_ids: HashSet<_> = records.keys().cloned().collect();
    let ids_set: HashSet<_> = ids.iter().cloned().collect();
    if ids.len() != ids_set.len() || ids_set != record_ids {
        return None;
    }
    for record in records.values() {
        let item = record.as_object()?;
        item.get("path")?.as_str()?;
        item.get("title")?.as_str()?;
        string_array(item.get("sessionIds")?)?;
        item.get("createdAt")?.as_str()?;
        item.get("updatedAt")?.as_str()?;
    }
    Some((bytes, ids.is_empty() && archived.is_empty()))
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(ToOwned::to_owned))
        .collect()
}

fn portable_name(name: &str) -> bool {
    !EXCLUDED.contains(&name)
        && name != "node_modules"
        && !name.ends_with(".lock")
        && !name.ends_with(".tmp")
}

fn copy_portable(source: &Path, target: &Path) -> AppResult<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        fs::copy(source, target)?;
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for child in fs::read_dir(source)? {
            let child = child?;
            if child.file_name() == "node_modules" {
                continue;
            }
            if child.file_type()?.is_symlink() {
                continue;
            }
            copy_portable(&child.path(), &target.join(child.file_name()))?;
        }
    }
    Ok(())
}

fn collect_missing_roots(
    source: &Path,
    target: &Path,
    roots: &mut Vec<(PathBuf, PathBuf)>,
) -> AppResult<()> {
    if target.exists() || target.is_symlink() {
        if source.is_dir() && target.is_dir() && !target.is_symlink() {
            for child in fs::read_dir(source)? {
                let child = child?;
                collect_missing_roots(&child.path(), &target.join(child.file_name()), roots)?;
            }
        }
    } else {
        roots.push((source.to_owned(), target.to_owned()));
    }
    Ok(())
}

fn publish_new(source: &Path, target: &Path) -> AppResult<()> {
    if source.is_dir() {
        fs::create_dir(target)?;
        let published = (|| -> AppResult<()> {
            for child in fs::read_dir(source)? {
                let child = child?;
                publish_new(&child.path(), &target.join(child.file_name()))?;
            }
            fs::set_permissions(target, fs::metadata(source)?.permissions())?;
            Ok(())
        })();
        if let Err(error) = published {
            let _ = remove_owned(target);
            return Err(error);
        }
    } else {
        use std::fs::OpenOptions;
        let mut input = fs::File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        let published = (|| -> AppResult<()> {
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            fs::set_permissions(target, fs::metadata(source)?.permissions())?;
            Ok(())
        })();
        if let Err(error) = published {
            drop(output);
            let _ = remove_owned(target);
            return Err(error);
        }
    }
    Ok(())
}

fn remove_owned(path: &Path) -> std::io::Result<()> {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
fn marker_complete(marker: &Path) -> bool {
    fs::read(marker).is_ok_and(|value| value == b"1\n")
}
fn write_marker(marker: &Path) -> AppResult<()> {
    atomic_write(marker, b"1\n")
}
fn absolute(path: &Path) -> AppResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
fn overlaps(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}
fn canonical_overlaps(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left)
        .ok()
        .zip(fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| overlaps(&left, &right))
}
fn result(copied: bool, message: &str) -> ImportResult {
    ImportResult {
        copied,
        message: message.into(),
    }
}

fn has_configuration(home: &Path) -> AppResult<bool> {
    for entry in fs::read_dir(home)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if ![
            ".anonymous-user-id",
            "attachments",
            "sessions",
            "storages",
            "node_modules",
        ]
        .contains(&name.as_ref())
            && !name.ends_with(".lock")
            && !name.ends_with(".tmp")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_import_only_fills_missing_values() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(destination.join("nested")).unwrap();
        fs::write(source.join("nested/keep"), "source").unwrap();
        fs::write(source.join("nested/new"), "new").unwrap();
        fs::write(destination.join("nested/keep"), "destination").unwrap();
        import_source_home(&source, &destination, &temp.path().join("marker")).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/keep")).unwrap(),
            "destination"
        );
        assert_eq!(
            fs::read_to_string(destination.join("nested/new")).unwrap(),
            "new"
        );
    }

    #[test]
    fn populated_workspace_is_never_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("storages")).unwrap();
        fs::create_dir_all(destination.join("storages")).unwrap();
        let populated = |title: &str| {
            serde_json::to_vec(&serde_json::json!({
            "unit": {"name": "workspace", "version": 2},
            "global": {"initialized": true, "workspaceIds": ["one"], "archivedSessionIds": []},
            "tables": {"workspaces": {"one": {"path": "/tmp", "title": title, "sessionIds": [], "createdAt": "now", "updatedAt": "now"}}}
        })).unwrap()
        };
        fs::write(source.join("storages/workspace.json"), populated("source")).unwrap();
        fs::write(
            destination.join("storages/workspace.json"),
            populated("destination"),
        )
        .unwrap();
        import_source_workspace(&source, &destination, &temp.path().join("marker")).unwrap();
        let actual = fs::read_to_string(destination.join("storages/workspace.json")).unwrap();
        assert!(actual.contains("destination"));
        assert!(!actual.contains("source"));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_import_does_not_follow_destination_directory_links() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        let external = temp.path().join("external");
        fs::create_dir_all(source.join("storages")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(
            source.join("storages/workspace.json"),
            serde_json::to_vec(&serde_json::json!({
                "unit": {"name": "workspace", "version": 2},
                "global": {"initialized": true, "workspaceIds": ["one"], "archivedSessionIds": []},
                "tables": {"workspaces": {"one": {"path": "/tmp", "title": "source", "sessionIds": [], "createdAt": "now", "updatedAt": "now"}}}
            }))
            .unwrap(),
        )
        .unwrap();
        symlink(&external, destination.join("storages")).unwrap();

        import_source_workspace(&source, &destination, &temp.path().join("marker")).unwrap();
        assert!(!external.join("workspace.json").exists());
    }
}
