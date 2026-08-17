use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    AppError, AppResult, ApplicationPaths,
    paths::atomic_write,
    runtime::{configure_process_group, terminate_tree},
};

const READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct ServerManager {
    paths: ApplicationPaths,
    child: Option<Child>,
    output_thread: Option<JoinHandle<()>>,
    web_url: Option<String>,
}

impl ServerManager {
    pub fn new(paths: ApplicationPaths) -> Self {
        Self {
            paths,
            child: None,
            output_thread: None,
            web_url: None,
        }
    }
    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    pub fn start(&mut self) -> AppResult<String> {
        self.start_cancellable(|| false)
    }

    pub fn start_cancellable(&mut self, cancelled: impl Fn() -> bool) -> AppResult<String> {
        if cancelled() {
            return Err(AppError::new("deploymentCancelled"));
        }
        if self.is_running() {
            return self
                .web_url
                .clone()
                .ok_or_else(|| AppError::new("serviceStartingNoAddress"));
        }
        self.stop();
        let environment = self.service_environment()?;
        let mut default_address_in_use = false;
        for use_free_port in [false, true] {
            if cancelled() {
                self.stop();
                return Err(AppError::new("deploymentCancelled"));
            }
            if use_free_port && !default_address_in_use {
                break;
            }
            let (sender, receiver) = mpsc::channel();
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.paths.server_log)?;
            let mut command = Command::new(&self.paths.node_bin);
            command.arg(&self.paths.dsh_bin).arg("web");
            if use_free_port {
                command.args(["--port", "0"]);
            }
            command
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());
            configure_process_group(&mut command);
            let mut child = command
                .spawn()
                .map_err(|error| AppError::io("serviceStartFailed", &error))?;
            if let Err(error) =
                atomic_write(&self.paths.server_pid, child.id().to_string().as_bytes())
            {
                stop_child(&mut child);
                return Err(error);
            }
            let Some(stdout) = child.stdout.take() else {
                stop_child(&mut child);
                return Err(AppError::new("serviceOutputUnreadable"));
            };
            let Some(stderr) = child.stderr.take() else {
                stop_child(&mut child);
                return Err(AppError::new("serviceOutputUnreadable"));
            };
            let thread =
                match thread::Builder::new()
                    .name("dsh-web-output".into())
                    .spawn(move || {
                        let mut log_out = log;
                        let stderr_log = log_out.try_clone().ok();
                        let sender_out = sender.clone();
                        let stdout_thread =
                            thread::spawn(move || capture(stdout, &mut log_out, &sender_out));
                        if let Some(mut stderr_log) = stderr_log {
                            capture(stderr, &mut stderr_log, &sender);
                        }
                        let _ = stdout_thread.join();
                    }) {
                    Ok(thread) => thread,
                    Err(error) => {
                        stop_child(&mut child);
                        return Err(AppError::io("serviceOutputUnreadable", &error));
                    }
                };
            self.child = Some(child);
            self.output_thread = Some(thread);
            let deadline = Instant::now() + READY_TIMEOUT;
            let mut address_in_use = false;
            while Instant::now() < deadline {
                if cancelled() {
                    self.stop();
                    return Err(AppError::new("deploymentCancelled"));
                }
                match receiver.recv_timeout(Duration::from_millis(200)) {
                    Ok(line) => {
                        address_in_use |= line.contains("EADDRINUSE");
                        if let Some(url) = parse_web_url(&line) {
                            if self.is_running() {
                                self.web_url = Some(url.clone());
                                return Ok(url);
                            }
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) if self.is_running() => continue,
                    Err(_) => break,
                }
            }
            self.stop();
            if !use_free_port {
                default_address_in_use = address_in_use;
            }
        }
        Err(AppError::new(if default_address_in_use {
            "freePortFailed"
        } else {
            "serviceNoAddress"
        }))
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take()
            && child.try_wait().ok().flatten().is_none()
        {
            terminate_tree(child.id(), false);
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if child.try_wait().ok().flatten().is_none() {
                terminate_tree(child.id(), true);
            }
            let _ = child.wait();
        }
        if let Some(thread) = self.output_thread.take() {
            let _ = thread.join();
        }
        self.web_url = None;
        let _ = fs::remove_file(&self.paths.server_pid);
    }

    fn service_environment(&self) -> AppResult<Vec<(String, std::ffi::OsString)>> {
        let mut environment: Vec<(String, std::ffi::OsString)> = std::env::vars_os()
            .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
            .collect();
        if std::env::var_os("DSH_HOME").is_none() {
            environment.retain(|(key, _)| key != "DSH_HOME");
            environment.push((
                "DSH_HOME".into(),
                self.paths.dsh_home.clone().into_os_string(),
            ));
        }
        Ok(environment)
    }
}

impl Drop for ServerManager {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stop_child(child: &mut Child) {
    terminate_tree(child.id(), false);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
        thread::sleep(Duration::from_millis(50));
    }
    if child.try_wait().ok().flatten().is_none() {
        terminate_tree(child.id(), true);
    }
    let _ = child.wait();
}

fn capture(stream: impl std::io::Read, log: &mut fs::File, sender: &mpsc::Sender<String>) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let _ = writeln!(log, "{line}");
        let _ = log.flush();
        let _ = sender.send(line);
    }
}

fn parse_web_url(line: &str) -> Option<String> {
    let candidate = line
        .trim()
        .strip_prefix("dsh web: ")?
        .split_whitespace()
        .next()?;
    let url = url::Url::parse(candidate).ok()?;
    (["http", "https"].contains(&url.scheme())
        && url.host_str().is_some()
        && url.port_or_known_default().is_some())
    .then(|| candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_official_line_and_web_url() {
        assert_eq!(
            parse_web_url("dsh web: http://127.0.0.1:3000"),
            Some("http://127.0.0.1:3000".into())
        );
        assert_eq!(parse_web_url("http://127.0.0.1:3000"), None);
        assert_eq!(parse_web_url("dsh web: file:///tmp/a"), None);
    }
}
