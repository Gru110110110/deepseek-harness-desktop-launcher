use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(windows)]
use crate::runtime::WindowsProcessGuard;
#[cfg(unix)]
use crate::runtime::process_tree_alive;
use crate::{
    AppError, AppResult, ApplicationPaths,
    paths::atomic_write,
    runtime::{configure_process_group, terminate_tree},
};

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(8);
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct ServerManager {
    paths: ApplicationPaths,
    child: Option<Child>,
    output_thread: Option<JoinHandle<()>>,
    web_url: Option<String>,
    #[cfg(unix)]
    shutdown_pid: Option<u32>,
    #[cfg(windows)]
    job: Option<WindowsProcessGuard>,
}

impl ServerManager {
    pub fn new(paths: ApplicationPaths) -> Self {
        Self {
            paths,
            child: None,
            output_thread: None,
            web_url: None,
            #[cfg(unix)]
            shutdown_pid: None,
            #[cfg(windows)]
            job: None,
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
        self.stop()?;
        let environment = self.service_environment()?;
        let mut default_address_in_use = false;
        for use_free_port in [false, true] {
            if cancelled() {
                self.stop()?;
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
            #[cfg(windows)]
            let job = WindowsProcessGuard::attach(&child)?;
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
            #[cfg(windows)]
            {
                self.job = Some(job);
            }
            let deadline = Instant::now() + READY_TIMEOUT;
            let mut address_in_use = false;
            while Instant::now() < deadline {
                #[cfg(windows)]
                if let Some(job) = self.job.as_ref() {
                    job.observe()?;
                }
                if cancelled() {
                    self.stop()?;
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
            self.stop()?;
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

    pub fn stop(&mut self) -> AppResult<()> {
        let web_url = self.web_url.clone();
        #[cfg(unix)]
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            self.shutdown_pid = Some(pid);
            terminate_tree(pid, false);
            let deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < deadline && process_tree_alive(pid) {
                let _ = child.try_wait();
                thread::sleep(Duration::from_millis(100));
            }
            if process_tree_alive(pid) {
                terminate_tree(pid, true);
            }
            child.wait()?;
        }
        #[cfg(unix)]
        if let Some(pid) = self.shutdown_pid {
            let deadline = Instant::now() + PORT_RELEASE_TIMEOUT;
            while Instant::now() < deadline && process_tree_alive(pid) {
                terminate_tree(pid, true);
                thread::sleep(Duration::from_millis(100));
            }
            if process_tree_alive(pid) {
                return Err(AppError::new("serviceProcessTreeStillRunning").value("processId", pid));
            }
            self.shutdown_pid = None;
        }
        #[cfg(windows)]
        if let Some(mut child) = self.child.take() {
            let pid = child.id();
            if let Some(job) = self.job.as_ref() {
                job.terminate()?;
            } else {
                terminate_tree(pid, true);
            }
            let deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < deadline && child.try_wait()?.is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            if child.try_wait()?.is_none() {
                terminate_tree(pid, true);
            }
            child.wait()?;
        }
        #[cfg(windows)]
        if let Some(job) = self.job.as_ref()
            && !job.wait_until_empty(PORT_RELEASE_TIMEOUT)?
        {
            return Err(AppError::new("serviceProcessTreeStillRunning"));
        }
        #[cfg(windows)]
        drop(self.job.take());
        if let Some(thread) = self.output_thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.paths.server_pid);
        if let Some(url) = web_url
            && !wait_for_port_release(&url, PORT_RELEASE_TIMEOUT)
        {
            return Err(
                AppError::new("serviceShutdownIncomplete").value("address", display_address(&url))
            );
        }
        self.web_url = None;
        Ok(())
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
        let _ = self.stop();
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
        && local_addresses(&url).is_some()
        && url.port_or_known_default().is_some())
    .then(|| candidate.to_owned())
}

fn local_addresses(url: &url::Url) -> Option<Vec<SocketAddr>> {
    let port = url.port_or_known_default()?;
    match url.host()? {
        url::Host::Ipv4(address) if address.is_loopback() => {
            Some(vec![SocketAddr::new(IpAddr::V4(address), port)])
        }
        url::Host::Ipv6(address) if address.is_loopback() => {
            Some(vec![SocketAddr::new(IpAddr::V6(address), port)])
        }
        url::Host::Domain(domain) if domain.eq_ignore_ascii_case("localhost") => Some(vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ]),
        _ => None,
    }
}

fn wait_for_port_release(url: &str, timeout: Duration) -> bool {
    let Some(addresses) = url::Url::parse(url).ok().as_ref().and_then(local_addresses) else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        let open = addresses
            .iter()
            .any(|address| TcpStream::connect_timeout(address, Duration::from_millis(100)).is_ok());
        if !open {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn display_address(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| {
            Some(format!(
                "{}:{}",
                url.host_str()?,
                url.port_or_known_default()?
            ))
        })
        .unwrap_or_else(|| "local-service".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::path::PathBuf;

    #[test]
    fn readiness_requires_official_line_and_web_url() {
        assert_eq!(
            parse_web_url("dsh web: http://127.0.0.1:3000"),
            Some("http://127.0.0.1:3000".into())
        );
        assert_eq!(parse_web_url("http://127.0.0.1:3000"), None);
        assert_eq!(parse_web_url("dsh web: file:///tmp/a"), None);
        assert_eq!(parse_web_url("dsh web: http://example.com:3000"), None);
    }

    #[test]
    fn shutdown_verification_waits_for_the_local_port_to_close() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");

        assert!(!wait_for_port_release(&url, Duration::from_millis(20)));
        drop(listener);
        assert!(wait_for_port_release(&url, Duration::from_secs(1)));
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_group_closes_descendants_and_releases_their_port() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::unix_process_group_parent_waits",
            ])
            .env("DSH_UNIX_PROCESS_GROUP_TEST_ADDRESS", address.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let url = format!("http://{address}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while wait_for_port_release(&url, Duration::from_millis(20)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!wait_for_port_release(&url, Duration::from_millis(20)));

        terminate_tree(pid, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_tree_alive(pid) && Instant::now() < deadline {
            let _ = child.try_wait();
            thread::sleep(Duration::from_millis(20));
        }
        if process_tree_alive(pid) {
            terminate_tree(pid, true);
        }
        let _ = child.wait();
        assert!(!process_tree_alive(pid));
        assert!(wait_for_port_release(&url, Duration::from_secs(2)));
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_process_group_parent_waits() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::unix_process_group_grandchild_holds_port",
            ])
            .env(
                "DSH_UNIX_PROCESS_GROUP_TEST_ADDRESS",
                std::env::var_os("DSH_UNIX_PROCESS_GROUP_TEST_ADDRESS").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        thread::sleep(Duration::from_secs(30));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_process_group_grandchild_holds_port() {
        let address = std::env::var("DSH_UNIX_PROCESS_GROUP_TEST_ADDRESS")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let _listener = std::net::TcpListener::bind(address).unwrap();
        thread::sleep(Duration::from_secs(30));
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_guard_closes_descendants_and_releases_their_port() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = tempfile::tempdir().unwrap();
        let signal = temp.path().join("attached");
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_job_parent_waits",
            ])
            .env("DSH_WINDOWS_JOB_TEST_ADDRESS", address.to_string())
            .env("DSH_WINDOWS_JOB_TEST_SIGNAL", &signal)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let job = WindowsProcessGuard::attach_snapshot(&child).unwrap();
        fs::write(&signal, b"attached").unwrap();
        let url = format!("http://{address}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while wait_for_port_release(&url, Duration::from_millis(20)) && Instant::now() < deadline {
            job.observe().unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        job.observe().unwrap();
        assert!(!wait_for_port_release(&url, Duration::from_millis(20)));

        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(child.try_wait().unwrap().is_some());
        assert!(wait_for_port_release(&url, Duration::from_secs(2)));
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_job_parent_waits() {
        let signal = PathBuf::from(std::env::var_os("DSH_WINDOWS_JOB_TEST_SIGNAL").unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !signal.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(signal.exists());
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_job_grandchild_holds_port",
            ])
            .env(
                "DSH_WINDOWS_JOB_TEST_ADDRESS",
                std::env::var_os("DSH_WINDOWS_JOB_TEST_ADDRESS").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        thread::sleep(Duration::from_secs(30));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_job_grandchild_holds_port() {
        let address = std::env::var("DSH_WINDOWS_JOB_TEST_ADDRESS")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let _listener = std::net::TcpListener::bind(address).unwrap();
        thread::sleep(Duration::from_secs(30));
    }
}
