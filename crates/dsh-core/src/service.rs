use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use std::{io::Read, process::ChildStdin};

#[cfg(windows)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::process_recovery::{
    SERVICE_GUARD_ARGUMENT, SERVICE_GUARD_FREE_PORT_ARGUMENT, SERVICE_GUARD_HOME_ARGUMENT,
};
#[cfg(windows)]
use crate::runtime::WindowsProcessGuard;
#[cfg(unix)]
use crate::runtime::process_tree_alive;
use crate::{
    AppError, AppResult, ApplicationPaths,
    paths::atomic_write,
    process_recovery::recover_owned_services,
    runtime::{configure_process_group, terminate_tree},
};

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(8);
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
const OUTPUT_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(any(unix, windows))]
const SERVICE_GUARD_KILL_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct ServerManager {
    paths: ApplicationPaths,
    child: Option<Child>,
    output_thread: Option<JoinHandle<()>>,
    web_url: Option<String>,
    guard_stdin: Option<ChildStdin>,
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
            guard_stdin: None,
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
        let recovered = recover_owned_services(&self.paths)?;
        if recovered > 0 {
            log::warn!("recovered {recovered} stale DSH service process tree(s)");
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
            let mut command = {
                let executable = std::env::current_exe()
                    .map_err(|error| AppError::io("serviceGuardFailed", &error))?;
                let mut command = Command::new(executable);
                command
                    .arg(SERVICE_GUARD_ARGUMENT)
                    .arg(SERVICE_GUARD_HOME_ARGUMENT)
                    .arg(&self.paths.app_home);
                if use_free_port {
                    command.arg(SERVICE_GUARD_FREE_PORT_ARGUMENT);
                }
                command.stdin(Stdio::piped());
                command
            };
            command
                .envs(environment.iter().map(|(key, value)| (key, value)))
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            configure_process_group(&mut command);
            let mut child = command
                .spawn()
                .map_err(|error| AppError::io("serviceStartFailed", &error))?;
            #[cfg(windows)]
            let job = match WindowsProcessGuard::attach(&child) {
                Ok(job) => job,
                Err(error) => {
                    stop_child(&mut child);
                    return Err(error);
                }
            };
            if let Err(error) =
                atomic_write(&self.paths.server_pid, child.id().to_string().as_bytes())
            {
                stop_child(&mut child);
                return Err(error);
            }
            {
                self.guard_stdin = child.stdin.take();
                if self.guard_stdin.is_none() {
                    stop_child(&mut child);
                    return Err(AppError::new("serviceGuardFailed"));
                }
            }
            let Some(stdout) = child.stdout.take() else {
                self.guard_stdin = None;
                stop_child(&mut child);
                return Err(AppError::new("serviceOutputUnreadable"));
            };
            let Some(stderr) = child.stderr.take() else {
                self.guard_stdin = None;
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
                        self.guard_stdin = None;
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
            self.guard_stdin = None;
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
            self.guard_stdin = None;
            // Give the guard a chance to observe stdin EOF and clean up its
            // own Node job/tree before the outer ownership boundary is
            // terminated. This matters most when either guard has fallen
            // back from Job Objects to process snapshots.
            let graceful_deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < graceful_deadline && child.try_wait()?.is_none() {
                thread::sleep(Duration::from_millis(50));
            }
            if let Some(job) = self.job.as_ref() {
                if let Err(error) = job.terminate() {
                    log::warn!("outer Windows process guard termination failed: {error}");
                    terminate_tree(pid, true);
                }
            } else {
                terminate_tree(pid, true);
            }
            let deadline = Instant::now() + STOP_TIMEOUT;
            while Instant::now() < deadline && child.try_wait()?.is_none() {
                thread::sleep(Duration::from_millis(100));
            }
            if child.try_wait()?.is_none() {
                terminate_tree(pid, true);
                let _ = child.kill();
                let deadline = Instant::now() + PORT_RELEASE_TIMEOUT;
                while Instant::now() < deadline && child.try_wait()?.is_none() {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            if child.try_wait()?.is_none() {
                return Err(AppError::new("serviceProcessTreeStillRunning").value("processId", pid));
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
            let deadline = Instant::now() + OUTPUT_JOIN_TIMEOUT;
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                log::warn!("DSH output reader did not stop before its cleanup deadline");
            }
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

#[cfg(any(unix, windows))]
pub fn handle_service_guard_cli() -> bool {
    let options = match parse_service_guard_arguments(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(()) => {
            eprintln!("DSH service guard received invalid arguments");
            std::process::exit(2);
        }
    };
    let Some((owner_home, use_free_port)) = options else {
        return false;
    };
    let result = ApplicationPaths::from_environment().and_then(|paths| {
        let actual_home = fs::canonicalize(&paths.app_home)
            .map_err(|error| AppError::io("serviceGuardFailed", &error))?;
        let owner_home = fs::canonicalize(owner_home)
            .map_err(|error| AppError::io("serviceGuardFailed", &error))?;
        if owner_home != actual_home {
            return Err(AppError::new("serviceGuardOwnershipMismatch"));
        }
        run_service_guard(&paths, use_free_port)
    });
    if let Err(error) = result {
        eprintln!("DSH service guard failed: {error}");
        std::process::exit(1);
    }
    true
}

#[cfg(any(unix, windows))]
fn parse_service_guard_arguments(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Option<(PathBuf, bool)>, ()> {
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(SERVICE_GUARD_ARGUMENT)) {
        return Ok(None);
    }
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(SERVICE_GUARD_HOME_ARGUMENT)) {
        return Err(());
    }
    let owner_home = arguments.next().map(PathBuf::from).ok_or(())?;
    let use_free_port = match arguments.next() {
        None => false,
        Some(value) if value == SERVICE_GUARD_FREE_PORT_ARGUMENT => true,
        Some(_) => return Err(()),
    };
    if arguments.next().is_some() {
        return Err(());
    }
    Ok(Some((owner_home, use_free_port)))
}

#[cfg(not(any(unix, windows)))]
pub fn handle_service_guard_cli() -> bool {
    false
}

#[cfg(unix)]
fn run_service_guard(paths: &ApplicationPaths, use_free_port: bool) -> AppResult<()> {
    let group = std::process::id();
    if unsafe { libc::getpgrp() } != i32::try_from(group).unwrap_or_default() {
        return Err(AppError::new("serviceGuardInvalidGroup"));
    }
    let mut command = Command::new(&paths.node_bin);
    command
        .arg(&paths.dsh_bin)
        .arg("web")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if use_free_port {
        command.args(["--port", "0"]);
    }
    let mut service = command
        .spawn()
        .map_err(|error| AppError::io("serviceStartFailed", &error))?;

    // The service inherited the default SIGTERM disposition. The guard ignores
    // SIGTERM only after spawning it, so a normal group shutdown stops the
    // service while leaving the guard alive long enough to reap the child.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let _monitor = match thread::Builder::new()
        .name("dsh-parent-monitor".into())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut byte = [0_u8; 1];
            loop {
                match input.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = signal_guard_group(group, libc::SIGTERM);
            thread::sleep(SERVICE_GUARD_KILL_DELAY);
            let _ = signal_guard_group(group, libc::SIGKILL);
        }) {
        Ok(monitor) => monitor,
        Err(error) => {
            stop_guard_service(&mut service, group);
            return Err(AppError::io("serviceGuardFailed", &error));
        }
    };

    match service.wait() {
        Ok(_) => {}
        Err(error) => {
            stop_guard_service(&mut service, group);
            return Err(error.into());
        }
    }
    // The service root may exit while leaving descendants in the group. Kill
    // the complete group, including this guard, so no descendant can outlive
    // the ownership boundary.
    signal_guard_group(group, libc::SIGKILL)
        .map_err(|error| AppError::io("serviceGuardFailed", &error))?;
    unsafe { libc::_exit(0) }
}

#[cfg(windows)]
fn run_service_guard(paths: &ApplicationPaths, use_free_port: bool) -> AppResult<()> {
    let mut command = Command::new(&paths.node_bin);
    command
        .arg(&paths.dsh_bin)
        .arg("web")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if use_free_port {
        command.args(["--port", "0"]);
    }
    configure_process_group(&mut command);
    run_windows_guarded_service(command)
}

#[cfg(windows)]
fn run_windows_guarded_service(mut command: Command) -> AppResult<()> {
    let mut service = command
        .spawn()
        .map_err(|error| AppError::io("serviceStartFailed", &error))?;
    let process_guard = match WindowsProcessGuard::attach(&service) {
        Ok(guard) => Arc::new(guard),
        Err(error) => {
            stop_child(&mut service);
            return Err(error);
        }
    };

    let service_pid = service.id();
    let parent_gone = Arc::new(AtomicBool::new(false));
    let monitor_state = Arc::clone(&parent_gone);
    let monitor_guard = Arc::clone(&process_guard);
    let monitor = match thread::Builder::new()
        .name("dsh-parent-monitor".into())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut byte = [0_u8; 1];
            loop {
                match input.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            monitor_state.store(true, Ordering::SeqCst);
            let terminated = monitor_guard.terminate().is_ok()
                && monitor_guard
                    .wait_until_empty(SERVICE_GUARD_KILL_DELAY)
                    .unwrap_or(false);
            if !terminated {
                terminate_tree(service_pid, true);
            }
        }) {
        Ok(monitor) => monitor,
        Err(error) => {
            let _ = process_guard.terminate();
            stop_child(&mut service);
            return Err(AppError::io("serviceGuardFailed", &error));
        }
    };

    let status = match service.wait() {
        Ok(status) => status,
        Err(error) => {
            let _ = process_guard.terminate();
            terminate_tree(service_pid, true);
            return Err(error.into());
        }
    };
    if parent_gone.load(Ordering::SeqCst) {
        let _ = monitor.join();
        return Ok(());
    }
    process_guard.terminate()?;
    if !process_guard.wait_until_empty(SERVICE_GUARD_KILL_DELAY)? {
        terminate_tree(service_pid, true);
        if !process_guard.wait_until_empty(SERVICE_GUARD_KILL_DELAY)? {
            return Err(
                AppError::new("serviceProcessTreeStillRunning").value("processId", service_pid)
            );
        }
    }
    if status.success() {
        Ok(())
    } else {
        Err(AppError::new("processFailed").value("status", status.to_string()))
    }
}

#[cfg(unix)]
fn stop_guard_service(service: &mut Child, group: u32) {
    let _ = signal_guard_group(group, libc::SIGTERM);
    let deadline = Instant::now() + SERVICE_GUARD_KILL_DELAY;
    while Instant::now() < deadline && service.try_wait().ok().flatten().is_none() {
        thread::sleep(Duration::from_millis(50));
    }
    // This also terminates the guard itself and any descendant that survived
    // the graceful signal, even if the service root has already exited.
    let _ = signal_guard_group(group, libc::SIGKILL);
}

#[cfg(unix)]
fn signal_guard_group(group: u32, signal: libc::c_int) -> std::io::Result<()> {
    let group = i32::try_from(group)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid group"))?;
    let result = unsafe { libc::kill(-group, signal) };
    if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
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
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
            thread::sleep(Duration::from_millis(50));
        }
    }
    if child.try_wait().ok().flatten().is_some() {
        let _ = child.wait();
    }
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
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[cfg(any(unix, windows))]
    struct GuardTestProcess(Child);

    #[cfg(any(unix, windows))]
    impl Drop for GuardTestProcess {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                terminate_tree(self.0.id(), true);
            }
            let _ = self.0.wait();
        }
    }

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

    #[cfg(any(unix, windows))]
    #[test]
    fn service_guard_arguments_require_an_owner_home() {
        let home = PathBuf::from("owner-home");
        let options = parse_service_guard_arguments(
            [
                SERVICE_GUARD_ARGUMENT.into(),
                SERVICE_GUARD_HOME_ARGUMENT.into(),
                home.clone().into_os_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(options, Some((home.clone(), false)));

        let options = parse_service_guard_arguments(
            [
                SERVICE_GUARD_ARGUMENT.into(),
                SERVICE_GUARD_HOME_ARGUMENT.into(),
                home.into_os_string(),
                SERVICE_GUARD_FREE_PORT_ARGUMENT.into(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(options, Some((PathBuf::from("owner-home"), true)));
        assert!(
            parse_service_guard_arguments([SERVICE_GUARD_ARGUMENT.into()].into_iter()).is_err()
        );
        assert_eq!(
            parse_service_guard_arguments(["--version".into()].into_iter()).unwrap(),
            None
        );
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
    fn unix_service_guard_stops_the_service_when_its_parent_pipe_closes() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ApplicationPaths::from_home(temp.path().join("desktop"));
        paths.ensure_dirs().unwrap();
        fs::create_dir_all(paths.node_bin.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.dsh_bin.parent().unwrap()).unwrap();
        symlink("/bin/sh", &paths.node_bin).unwrap();
        let service_pid = temp.path().join("service.pid");
        fs::write(
            &paths.dsh_bin,
            "echo $$ > \"$DSH_GUARD_TEST_PID\"\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        )
        .unwrap();

        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::unix_service_guard_helper",
            ])
            .env("DSH_DESKTOP_HOME", &paths.app_home)
            .env("DSH_GUARD_TEST_PID", &service_pid)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut guard = GuardTestProcess(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !service_pid.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(service_pid.exists());
        let pid = fs::read_to_string(&service_pid)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();

        drop(guard.0.stdin.take());
        let deadline = Instant::now() + SERVICE_GUARD_KILL_DELAY + Duration::from_secs(3);
        while guard.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(guard.0.try_wait().unwrap().is_some());
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_service_guard_stops_descendants_after_the_service_root_exits() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = tempfile::tempdir().unwrap();
        let paths = ApplicationPaths::from_home(temp.path().join("desktop"));
        paths.ensure_dirs().unwrap();
        fs::create_dir_all(paths.node_bin.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.dsh_bin.parent().unwrap()).unwrap();
        symlink("/bin/sh", &paths.node_bin).unwrap();
        let signal = temp.path().join("descendant-ready");
        fs::write(
            &paths.dsh_bin,
            "\"$DSH_GUARD_TEST_EXECUTABLE\" --ignored --exact service::tests::unix_guard_descendant_holds_port &\nwhile [ ! -f \"$DSH_GUARD_TEST_SIGNAL\" ]; do sleep 0.01; done\nexit 0\n",
        )
        .unwrap();

        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(&executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::unix_service_guard_helper",
            ])
            .env("DSH_DESKTOP_HOME", &paths.app_home)
            .env("DSH_GUARD_TEST_EXECUTABLE", &executable)
            .env("DSH_GUARD_TEST_ADDRESS", address.to_string())
            .env("DSH_GUARD_TEST_SIGNAL", &signal)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut guard = GuardTestProcess(command.spawn().unwrap());
        let url = format!("http://{address}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !signal.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(signal.exists());
        let deadline = Instant::now() + Duration::from_secs(3);
        while guard.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(guard.0.try_wait().unwrap().is_some());
        assert!(wait_for_port_release(&url, Duration::from_secs(2)));
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_service_guard_helper() {
        let paths = ApplicationPaths::from_environment().unwrap();
        run_service_guard(&paths, false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[ignore]
    fn unix_guard_descendant_holds_port() {
        let address = std::env::var("DSH_GUARD_TEST_ADDRESS")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let _listener = std::net::TcpListener::bind(address).unwrap();
        fs::write(std::env::var_os("DSH_GUARD_TEST_SIGNAL").unwrap(), b"ready").unwrap();
        thread::sleep(Duration::from_secs(30));
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
    fn windows_snapshot_outer_guard_allows_pipe_cleanup_before_escalation() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = tempfile::tempdir().unwrap();
        let signal = temp.path().join("guard-child-ready");
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_service_guard_helper",
            ])
            .env("DSH_WINDOWS_GUARD_TEST_ADDRESS", address.to_string())
            .env("DSH_WINDOWS_GUARD_TEST_SIGNAL", &signal)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut guard = GuardTestProcess(command.spawn().unwrap());
        let outer_guard = WindowsProcessGuard::attach_snapshot(&guard.0).unwrap();
        let url = format!("http://{address}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while (!signal.exists() || wait_for_port_release(&url, Duration::from_millis(20)))
            && Instant::now() < deadline
        {
            outer_guard.observe().unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        outer_guard.observe().unwrap();
        assert!(signal.exists());
        assert!(!wait_for_port_release(&url, Duration::from_millis(20)));

        drop(guard.0.stdin.take());
        let deadline = Instant::now() + SERVICE_GUARD_KILL_DELAY + Duration::from_secs(3);
        while guard.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(guard.0.try_wait().unwrap().is_some());
        outer_guard.terminate().unwrap();
        assert!(
            outer_guard
                .wait_until_empty(Duration::from_secs(2))
                .unwrap()
        );
        assert!(wait_for_port_release(&url, Duration::from_secs(2)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_service_guard_stops_descendants_after_the_service_root_exits() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = tempfile::tempdir().unwrap();
        let signal = temp.path().join("orphan-descendant-ready");
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_service_guard_root_exit_helper",
            ])
            .env("DSH_WINDOWS_GUARD_TEST_ADDRESS", address.to_string())
            .env("DSH_WINDOWS_GUARD_TEST_SIGNAL", &signal)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut guard = GuardTestProcess(command.spawn().unwrap());
        let url = format!("http://{address}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !signal.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(signal.exists());
        let deadline = Instant::now() + Duration::from_secs(5);
        while guard.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(guard.0.try_wait().unwrap().is_some());
        assert!(wait_for_port_release(&url, Duration::from_secs(2)));
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_service_guard_helper() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_service_guard_child_holds_port",
            ])
            .env(
                "DSH_WINDOWS_GUARD_TEST_ADDRESS",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_ADDRESS").unwrap(),
            )
            .env(
                "DSH_WINDOWS_GUARD_TEST_SIGNAL",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_SIGNAL").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        run_windows_guarded_service(command).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_service_guard_child_holds_port() {
        let address = std::env::var("DSH_WINDOWS_GUARD_TEST_ADDRESS")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let _listener = std::net::TcpListener::bind(address).unwrap();
        fs::write(
            std::env::var_os("DSH_WINDOWS_GUARD_TEST_SIGNAL").unwrap(),
            b"ready",
        )
        .unwrap();
        thread::sleep(Duration::from_secs(30));
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_service_guard_root_exit_helper() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_service_root_exits_with_descendant",
            ])
            .env(
                "DSH_WINDOWS_GUARD_TEST_ADDRESS",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_ADDRESS").unwrap(),
            )
            .env(
                "DSH_WINDOWS_GUARD_TEST_SIGNAL",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_SIGNAL").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        run_windows_guarded_service(command).unwrap();
    }

    #[cfg(windows)]
    #[test]
    #[ignore]
    fn windows_service_root_exits_with_descendant() {
        let executable = std::env::current_exe().unwrap();
        let mut command = Command::new(executable);
        command
            .args([
                "--ignored",
                "--exact",
                "service::tests::windows_service_guard_child_holds_port",
            ])
            .env(
                "DSH_WINDOWS_GUARD_TEST_ADDRESS",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_ADDRESS").unwrap(),
            )
            .env(
                "DSH_WINDOWS_GUARD_TEST_SIGNAL",
                std::env::var_os("DSH_WINDOWS_GUARD_TEST_SIGNAL").unwrap(),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let _descendant = command.spawn().unwrap();
        let signal = PathBuf::from(std::env::var_os("DSH_WINDOWS_GUARD_TEST_SIGNAL").unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !signal.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(signal.exists());
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
