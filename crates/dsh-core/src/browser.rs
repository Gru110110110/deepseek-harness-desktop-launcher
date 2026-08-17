#[cfg(unix)]
use std::thread;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{AppError, AppResult, model::BrowserChoice};

#[derive(Debug, Clone)]
pub struct BrowserCatalog {
    entries: Vec<BrowserEntry>,
}

#[derive(Debug, Clone)]
struct BrowserEntry {
    choice: BrowserChoice,
    executable: Option<PathBuf>,
}

impl BrowserCatalog {
    pub fn discover() -> Self {
        let mut entries = vec![BrowserEntry {
            choice: BrowserChoice {
                id: "system".into(),
                label: "System default".into(),
            },
            executable: None,
        }];
        #[cfg(target_os = "macos")]
        discover_macos(&mut entries);
        #[cfg(windows)]
        discover_windows(&mut entries);
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.choice.id.clone()));
        Self { entries }
    }

    pub fn choices(&self) -> Vec<BrowserChoice> {
        self.entries
            .iter()
            .map(|entry| entry.choice.clone())
            .collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|entry| entry.choice.id == id)
    }

    pub fn open(&self, id: &str, url: &str) -> AppResult<()> {
        url::Url::parse(url).map_err(|_| AppError::new("invalidWebUrl"))?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.choice.id == id)
            .or_else(|| self.entries.first())
            .ok_or_else(|| AppError::new("browserUnavailable"))?;
        match &entry.executable {
            #[cfg(target_os = "macos")]
            Some(app) => spawn(Command::new("open").arg("-a").arg(app).arg(url)),
            #[cfg(windows)]
            Some(executable) => spawn(Command::new(executable).arg(url)),
            #[cfg(not(any(target_os = "macos", windows)))]
            Some(executable) => spawn(Command::new(executable).arg(url)),
            None => open_default(url),
        }
    }
}

fn spawn(command: &mut Command) -> AppResult<()> {
    let mut child = command
        .spawn()
        .map_err(|error| AppError::io("browserOpenFailed", &error))?;
    #[cfg(unix)]
    thread::spawn(move || {
        let _ = child.wait();
    });
    #[cfg(not(unix))]
    drop(child);
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_default(url: &str) -> AppResult<()> {
    spawn(Command::new("open").arg(url))
}

#[cfg(windows)]
fn open_default(url: &str) -> AppResult<()> {
    spawn(
        Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url),
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_default(url: &str) -> AppResult<()> {
    spawn(Command::new("xdg-open").arg(url))
}

#[cfg(target_os = "macos")]
fn discover_macos(entries: &mut Vec<BrowserEntry>) {
    let candidates = [
        ("safari", "Safari", "/Applications/Safari.app"),
        ("chrome", "Google Chrome", "/Applications/Google Chrome.app"),
        ("edge", "Microsoft Edge", "/Applications/Microsoft Edge.app"),
        ("firefox", "Firefox", "/Applications/Firefox.app"),
        ("arc", "Arc", "/Applications/Arc.app"),
    ];
    for (id, label, path) in candidates {
        if Path::new(path).is_dir() {
            entries.push(BrowserEntry {
                choice: BrowserChoice {
                    id: id.into(),
                    label: label.into(),
                },
                executable: Some(path.into()),
            });
        }
    }
}

#[cfg(windows)]
fn discover_windows(entries: &mut Vec<BrowserEntry>) {
    let roots = [
        std::env::var_os("PROGRAMFILES"),
        std::env::var_os("PROGRAMFILES(X86)"),
        std::env::var_os("LOCALAPPDATA"),
    ];
    let candidates = [
        (
            "edge",
            "Microsoft Edge",
            "Microsoft/Edge/Application/msedge.exe",
        ),
        (
            "chrome",
            "Google Chrome",
            "Google/Chrome/Application/chrome.exe",
        ),
        ("firefox", "Firefox", "Mozilla Firefox/firefox.exe"),
    ];
    for root in roots.into_iter().flatten() {
        for (id, label, relative) in candidates {
            let path = PathBuf::from(&root).join(relative);
            if path.is_file() {
                entries.push(BrowserEntry {
                    choice: BrowserChoice {
                        id: id.into(),
                        label: label.into(),
                    },
                    executable: Some(path),
                });
            }
        }
    }
}
