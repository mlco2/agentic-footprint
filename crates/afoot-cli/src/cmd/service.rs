use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;

use super::DEFAULT_LOGS_ENDPOINT;

const LABEL: &str = "dev.agentic-footprint.afoot-watch";
const SYSTEMD_UNIT: &str = "agentic-footprint-watch.service";
/// The Windows scheduled task, in the root task folder (a standard user can
/// register a task of their own there; a subfolder may need more).
const TASK_NAME: &str = "agentic-footprint-afoot-watch";
const TASK_FILE: &str = "afoot-watch.task.xml";

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
    /// OTLP HTTP/JSON logs endpoint the resident receiver must serve
    /// [default: the installed service's, else http://127.0.0.1:4318/v1/logs]
    #[arg(long, global = true)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Action {
    /// Install, enable, start, and verify the per-user service
    Install,
    /// Start or restart the installed per-user service
    Start,
    /// Show manager status and verify the receiver is reachable
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Launchd,
    Systemd,
    /// Windows Task Scheduler: a task that starts `afoot watch` when this
    /// user logs on. It needs no administrator rights, because it is
    /// registered for the current user only and runs in their session.
    TaskScheduler,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdUpdate {
    Restart,
    Bootstrap,
    Reload,
}

pub fn run(state_dir: &Path, args: Args) -> Result<()> {
    let binary = env::current_exe().context("resolve the afoot executable")?;
    // The address the installed service was given, so a restart or status
    // check verifies the receiver where it really is.
    let endpoint = args
        .endpoint
        .or_else(installed_endpoint)
        .unwrap_or_else(|| DEFAULT_LOGS_ENDPOINT.to_string());
    match args.action {
        Action::Install => ensure_installed(state_dir, &binary, &endpoint),
        Action::Start => start(manager(), state_dir, &endpoint),
        Action::Status => status(manager(), state_dir, &endpoint),
    }
}

pub fn ensure_for_setup(state_dir: &Path, endpoint: &str) -> Result<()> {
    let binary = env::current_exe().context("resolve the afoot executable")?;
    ensure_installed(state_dir, &binary, endpoint)
}

pub fn check_for_setup(state_dir: &Path, endpoint: &str) -> Result<()> {
    status(manager(), state_dir, endpoint)
}

pub fn receiver_reachable(endpoint: &str) -> Result<()> {
    verify_reachable(endpoint)
}

/// Whether a per-user service definition is installed for this platform's
/// manager — `afoot self update` restarts the receiver only then.
pub fn installed() -> bool {
    definition_path().is_some_and(|path| path.is_file())
}

/// Where this platform's manager keeps the service definition.
fn definition_path() -> Option<PathBuf> {
    match manager() {
        Manager::Launchd => launchd_plist_path().ok(),
        Manager::Systemd => systemd_config_dir().ok().map(|dir| dir.join(SYSTEMD_UNIT)),
        Manager::TaskScheduler => {
            crate::paths::state_dir_checked().map(|dir| task_definition_path(&dir))
        }
        Manager::Unsupported => None,
    }
}

/// The task definition `afoot` registered, kept so that the endpoint it was
/// installed with can be read back (Task Scheduler holds its own copy).
fn task_definition_path(state_dir: &Path) -> PathBuf {
    state_dir.join("service").join(TASK_FILE)
}

/// A service definition as text. Task Scheduler's XML is UTF-16 with a
/// byte-order mark; the others are UTF-8.
fn read_definition(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(utf16) => {
            let units: Vec<u16> = utf16
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).ok()
        }
        None => String::from_utf8(bytes).ok(),
    }
}

/// The logs endpoint the installed service's receiver serves, read back
/// from its definition (`--otlp-addr`), so a restart or status check
/// verifies the address it was installed with rather than the default.
pub(crate) fn installed_endpoint() -> Option<String> {
    let definition = read_definition(&definition_path()?)?;
    otlp_addr_in(&definition).map(|addr| format!("http://{addr}/v1/logs"))
}

/// The address after `--otlp-addr` in a launchd plist
/// (`<string>--otlp-addr</string><string>ADDR</string>`), a systemd
/// `ExecStart=` line or a scheduled task's `<Arguments>` (`--otlp-addr ADDR`).
fn otlp_addr_in(definition: &str) -> Option<String> {
    let rest = definition.split("--otlp-addr").nth(1)?;
    let rest = rest.trim_start_matches("</string>").trim_start();
    let rest = rest.strip_prefix("<string>").unwrap_or(rest);
    let addr: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '<')
        .collect();
    (!addr.is_empty()).then_some(addr)
}

pub fn automatic_manager_available() -> bool {
    manager() != Manager::Unsupported
}

pub fn foreground_setup_instructions(state_dir: &Path, endpoint: &str) -> String {
    format!(
        "Start the receiver manually with `{}` and keep it running, then rerun `afoot setup` to verify {endpoint}.",
        manual_watch_command(state_dir)
    )
}

fn ensure_installed(state_dir: &Path, binary: &Path, endpoint: &str) -> Result<()> {
    let manager = manager();
    let addr = endpoint_addr(endpoint)?;
    let install_result = match manager {
        Manager::Launchd => install_launchd(state_dir, binary, addr),
        Manager::Systemd => install_systemd(state_dir, binary, addr),
        Manager::TaskScheduler => install_task(state_dir, binary, addr),
        Manager::Unsupported => {
            if verify_reachable(endpoint).is_ok() {
                println!("Resident receiver is reachable at {endpoint} (managed manually)");
                return Ok(());
            }
            return Err(manual_instructions(state_dir, endpoint));
        }
    };
    install_result.map_err(|err| with_manual_recovery(err, state_dir, endpoint))?;
    service_active(manager).map_err(|err| with_manual_recovery(err, state_dir, endpoint))?;
    wait_until_reachable(endpoint, Duration::from_secs(10))
        .map_err(|err| with_manual_recovery(err, state_dir, endpoint))
        .with_context(|| {
            format!(
                "resident receiver did not become reachable; {}",
                logs_hint(manager, state_dir)
            )
        })?;
    println!(
        "Resident receiver is running at {endpoint} ({})",
        logs_hint(manager, state_dir)
    );
    Ok(())
}

fn start(manager: Manager, state_dir: &Path, endpoint: &str) -> Result<()> {
    match manager {
        Manager::Launchd => kickstart(&launchd_target()?, "restart launchd service")?,
        Manager::Systemd => {
            run_command(
                Command::new("systemctl").args(["--user", "restart", SYSTEMD_UNIT]),
                "restart systemd user service",
            )?;
        }
        Manager::TaskScheduler => restart_task()?,
        Manager::Unsupported => return Err(manual_instructions(state_dir, endpoint)),
    }
    wait_until_reachable(endpoint, Duration::from_secs(10))?;
    println!("Resident receiver is reachable at {endpoint}");
    Ok(())
}

fn status(manager: Manager, state_dir: &Path, endpoint: &str) -> Result<()> {
    match manager {
        Manager::Launchd => {
            run_command(
                Command::new("launchctl")
                    .arg("print")
                    .arg(launchd_target()?),
                "query launchd service",
            )?;
        }
        Manager::Systemd => {
            run_command(
                Command::new("systemctl").args(["--user", "status", SYSTEMD_UNIT, "--no-pager"]),
                "query systemd user service",
            )?;
        }
        Manager::TaskScheduler => {
            run_command(
                schtasks().args(["/Query", "/TN", TASK_NAME, "/V", "/FO", "LIST"]),
                "query the scheduled task",
            )?;
        }
        Manager::Unsupported => {
            verify_reachable(endpoint)
                .map_err(|err| with_manual_recovery(err, state_dir, endpoint))?;
            println!("Receiver check: reachable at {endpoint} (managed manually)");
            return Ok(());
        }
    }
    verify_reachable(endpoint)?;
    println!("Receiver check: reachable at {endpoint}");
    println!("Logs: {}", logs_hint(manager, state_dir));
    Ok(())
}

fn launchd_plist_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn install_launchd(state_dir: &Path, binary: &Path, addr: SocketAddr) -> Result<()> {
    let path = launchd_plist_path()?;
    let logs = state_dir.join("logs");
    fs::create_dir_all(&logs)
        .with_context(|| format!("create service log directory {}", logs.display()))?;
    let definition_changed = write_if_changed(
        &path,
        launchd_plist(binary, addr, state_dir, &logs).as_bytes(),
    )?;

    let domain = launchd_domain()?;
    let target = format!("{domain}/{LABEL}");
    let loaded = command_succeeds(Command::new("launchctl").arg("print").arg(&target));

    match launchd_update(loaded, definition_changed) {
        LaunchdUpdate::Restart => {}
        LaunchdUpdate::Bootstrap => {
            bootstrap_launchd(&domain, &path, &target, Duration::from_secs(3))?
        }
        LaunchdUpdate::Reload => {
            run_command(
                Command::new("launchctl").arg("bootout").arg(&target),
                "unload changed launchd service",
            )?;
            wait_until_launchd_unloaded(&target, Duration::from_secs(3))?;
            bootstrap_launchd(&domain, &path, &target, Duration::from_secs(3))?;
        }
    }

    run_command(
        Command::new("launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(&target),
        "start launchd service",
    )
}

fn install_systemd(state_dir: &Path, binary: &Path, addr: SocketAddr) -> Result<()> {
    let path = systemd_config_dir()?.join(SYSTEMD_UNIT);
    write_if_changed(&path, systemd_unit(binary, addr, state_dir).as_bytes())?;
    run_command(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "reload systemd user units",
    )?;
    run_command(
        Command::new("systemctl").args(["--user", "enable", "--now", SYSTEMD_UNIT]),
        "enable systemd user service",
    )?;
    run_command(
        Command::new("systemctl").args(["--user", "restart", SYSTEMD_UNIT]),
        "restart systemd user service",
    )
}

// --- Windows Task Scheduler -------------------------------------------------

fn schtasks() -> Command {
    Command::new("schtasks")
}

/// Where the task's `afoot watch` writes what it would print (it has no
/// console to print to).
pub(crate) fn service_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("logs").join("watch.log")
}

fn install_task(state_dir: &Path, binary: &Path, addr: SocketAddr) -> Result<()> {
    let user = task_user(env::var("USERDOMAIN").ok(), env::var("USERNAME").ok())?;
    let path = task_definition_path(state_dir);
    write_if_changed(
        &path,
        &utf16_with_bom(&task_xml(binary, addr, state_dir, &user)?),
    )?;
    // /F replaces a task of the same name: installing again is an update.
    let registered = run_command(
        schtasks()
            .args(["/Create", "/TN", TASK_NAME, "/F", "/XML"])
            .arg(&path),
        "register the scheduled task",
    );
    if let Err(err) = registered {
        // Nothing is installed: do not leave a definition that says so.
        let _ = fs::remove_file(&path);
        return Err(err.context(
            "Task Scheduler refused the task (an organisation's policy can forbid users \
             from creating tasks)",
        ));
    }
    restart_task()
}

/// Stop the task's `afoot watch` if it runs, and start it. Task Scheduler ends
/// a task by terminating its process; the store is written so that this is
/// safe at any point.
fn restart_task() -> Result<()> {
    let _ = command_succeeds(schtasks().args(["/End", "/TN", TASK_NAME]));
    run_command(
        schtasks().args(["/Run", "/TN", TASK_NAME]),
        "start the scheduled task",
    )
}

/// `DOMAIN\user`, as Task Scheduler names the current user. Without a
/// name the task would be for "any user", which only an administrator may
/// register: that is refused here rather than attempted.
fn task_user(domain: Option<String>, name: Option<String>) -> Result<String> {
    let name = name.filter(|name| !name.trim().is_empty()).context(
        "cannot tell which user to register the scheduled task for (USERNAME is not set)",
    )?;
    Ok(match domain.filter(|domain| !domain.trim().is_empty()) {
        Some(domain) => format!("{domain}\\{name}"),
        None => name,
    })
}

/// `path` as one argument of a Windows command line.
fn windows_argument(path: &Path) -> Result<String> {
    let text = path.to_string_lossy();
    if text.contains('"') {
        bail!("the path {text} contains a quotation mark");
    }
    // A backslash before the closing quote would escape it.
    Ok(format!("\"{}\"", text.trim_end_matches('\\')))
}

/// The task: start `afoot watch` when `user` logs on, in their own session
/// and with their ordinary rights, and keep it running.
///
/// `--as-service` carries the state directory (a task has no environment
/// of its own to set `AFOOT_STATE_DIR` in) and makes `afoot watch` give up its
/// console window. The settings are in the order Task Scheduler itself
/// writes them.
fn task_xml(binary: &Path, addr: SocketAddr, state_dir: &Path, user: &str) -> Result<String> {
    let arguments = format!(
        "watch --require-receiver --otlp-addr {addr} --as-service {}",
        windows_argument(state_dir)?
    );
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>agentic-footprint: receives coding-agent telemetry (afoot watch)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>10</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(Path::new(user)),
        command = xml_escape(binary),
        arguments = xml_escape(Path::new(&arguments)),
    ))
}

/// Task Scheduler reads its XML as UTF-16, little-endian, with a
/// byte-order mark.
fn utf16_with_bom(text: &str) -> Vec<u8> {
    std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// What `afoot watch --as-service` does first: on Windows, give up the console
/// window the task was started with and write to the service log instead;
/// the Python helpers are then started without a window of their own.
/// Elsewhere the service manager already handles both.
pub fn enter_background(state_dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows_sys::Win32::System::Console::{
            FreeConsole, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };

        let log = service_log_path(state_dir);
        if let Some(parent) = log.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create service log directory {}", parent.display()))?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .with_context(|| format!("open the service log {}", log.display()))?;
        // The handle lives as long as the process: it is its output now.
        let handle = file.into_raw_handle();
        // SAFETY: `handle` is a valid, open file handle that is never
        // closed; the three calls take no other pointers.
        unsafe {
            FreeConsole();
            SetStdHandle(STD_OUTPUT_HANDLE, handle as _);
            SetStdHandle(STD_ERROR_HANDLE, handle as _);
        }
        afoot_sidecar::spawn_without_windows();
    }
    #[cfg(not(windows))]
    let _ = state_dir;
    Ok(())
}

fn manager() -> Manager {
    match env::var("AFOOT_SERVICE_MANAGER").ok().as_deref() {
        Some("launchd") => return Manager::Launchd,
        Some("systemd") => return Manager::Systemd,
        Some("task-scheduler") => return Manager::TaskScheduler,
        Some("unsupported") => return Manager::Unsupported,
        Some(_) => return Manager::Unsupported,
        None => {}
    }
    if cfg!(target_os = "macos") {
        Manager::Launchd
    } else if cfg!(target_os = "linux") && env::var_os("XDG_RUNTIME_DIR").is_some() {
        Manager::Systemd
    } else if cfg!(windows) {
        Manager::TaskScheduler
    } else {
        Manager::Unsupported
    }
}

fn endpoint_addr(endpoint: &str) -> Result<SocketAddr> {
    let authority = endpoint
        .strip_prefix("http://")
        .and_then(|value| value.strip_suffix("/v1/logs"))
        .context("resident service endpoint must be http://HOST:PORT/v1/logs")?;
    let mut addresses = authority
        .to_socket_addrs()
        .with_context(|| format!("resolve receiver address {authority}"))?;
    let addr = addresses
        .find(|addr| addr.ip().is_loopback())
        .context("resident receiver must bind to a loopback address")?;
    Ok(addr)
}

fn verify_reachable(endpoint: &str) -> Result<()> {
    let addr = endpoint_addr(endpoint)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .with_context(|| format!("connect to {endpoint}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let host = addr.to_string();
    let request = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        bail!("{endpoint} answered, but not as an afoot OTLP receiver");
    }
    let head = response.split("\r\n\r\n").next().unwrap_or_default();
    let header = afoot_otlp::RECEIVER_HEADER.to_ascii_lowercase();
    let version = head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name.trim().to_ascii_lowercase() == header).then(|| value.trim().to_string())
    });
    let Some(version) = version else {
        bail!(
            "{endpoint} answered, but not as an afoot OTLP receiver (another OTLP collector, \
             or an afoot older than this one, owns the port)"
        );
    };
    let ours = env!("CARGO_PKG_VERSION");
    if version != ours {
        bail!("{endpoint} is afoot {version}, but this is afoot {ours}; restart the receiver");
    }
    Ok(())
}

fn wait_until_reachable(endpoint: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        match verify_reachable(endpoint) {
            Ok(()) => return Ok(()),
            Err(err) => last = Some(err),
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(last.unwrap_or_else(|| anyhow!("receiver verification timed out")))
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<bool> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .context("service file has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create service directory {}", parent.display()))?;
    fs::write(path, contents).with_context(|| format!("write service file {}", path.display()))?;
    Ok(true)
}

fn command_succeeds(command: &mut Command) -> bool {
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn launchd_update(loaded: bool, definition_changed: bool) -> LaunchdUpdate {
    match (loaded, definition_changed) {
        (true, false) => LaunchdUpdate::Restart,
        (false, _) => LaunchdUpdate::Bootstrap,
        (true, true) => LaunchdUpdate::Reload,
    }
}

fn wait_until_launchd_unloaded(target: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !command_succeeds(Command::new("launchctl").arg("print").arg(target)) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("launchd service {target} did not finish unloading")
}

fn bootstrap_launchd(domain: &str, path: &Path, target: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain)
            .arg(path)
            .output()
            .context("load launchd service: command unavailable")?;
        if output.status.success() {
            return Ok(());
        }
        if command_succeeds(Command::new("launchctl").arg("print").arg(target)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if error.is_empty() {
                bail!("load launchd service: launchctl bootstrap failed")
            }
            bail!("load launchd service: {error}")
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn run_command(command: &mut Command, action: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("{action}: command unavailable"))?;
    if !status.success() {
        bail!("{action}: command exited with {status}");
    }
    Ok(())
}

/// The launchd service specifier, `gui/<uid>/<LABEL>`.
fn launchd_target() -> Result<String> {
    Ok(format!("{}/{LABEL}", launchd_domain()?))
}

fn kickstart(target: &str, action: &str) -> Result<()> {
    run_command(
        Command::new("launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(target),
        action,
    )
}

fn service_active(manager: Manager) -> Result<()> {
    match manager {
        Manager::Launchd => run_command(
            Command::new("launchctl")
                .arg("print")
                .arg(launchd_target()?),
            "verify launchd service",
        ),
        Manager::Systemd => run_command(
            Command::new("systemctl").args(["--user", "is-active", "--quiet", SYSTEMD_UNIT]),
            "verify systemd user service",
        ),
        Manager::TaskScheduler => {
            if !command_succeeds(schtasks().args(["/Query", "/TN", TASK_NAME])) {
                bail!("the scheduled task {TASK_NAME} is not registered");
            }
            Ok(())
        }
        Manager::Unsupported => bail!("no supported service manager is available"),
    }
}

fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("find user id for launchd")?;
    if !output.status.success() {
        bail!(
            "find user id for launchd: id -u exited with {}",
            output.status
        );
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

fn home_dir() -> Result<PathBuf> {
    crate::paths::home_dir().context("HOME must be set to install a user service")
}

fn systemd_config_dir() -> Result<PathBuf> {
    Ok(env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".config"))
        .join("systemd/user"))
}

fn logs_hint(manager: Manager, state_dir: &Path) -> String {
    match manager {
        Manager::Launchd => format!("logs in {}", state_dir.join("logs").display()),
        Manager::Systemd => format!("run `journalctl --user -u {SYSTEMD_UNIT}`"),
        Manager::TaskScheduler => {
            format!("log in {}", service_log_path(state_dir).display())
        }
        Manager::Unsupported => "run `afoot watch` in a persistent terminal".to_string(),
    }
}

/// How to start the receiver by hand, in the local shell's own syntax.
#[cfg(unix)]
fn manual_watch_command(state_dir: &Path) -> String {
    format!("AFOOT_STATE_DIR={} afoot watch", state_dir.display())
}

#[cfg(windows)]
fn manual_watch_command(state_dir: &Path) -> String {
    format!(
        "$env:AFOOT_STATE_DIR = '{}'; afoot watch  (PowerShell)",
        state_dir.display()
    )
}

fn manual_instructions(state_dir: &Path, endpoint: &str) -> anyhow::Error {
    anyhow!(
        "no supported per-user service manager is available. {} Supported automatic managers \
         are macOS launchd, Linux systemd user services and Windows Task Scheduler",
        foreground_setup_instructions(state_dir, endpoint)
    )
}

fn with_manual_recovery(err: anyhow::Error, state_dir: &Path, endpoint: &str) -> anyhow::Error {
    anyhow!("{err:#}; {}", manual_instructions(state_dir, endpoint))
}

/// `Umask` 63 is 0o077: the logs launchd creates, and every file the
/// service writes, are the user's alone.
fn launchd_plist(binary: &Path, addr: SocketAddr, state_dir: &Path, logs: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>
    <string>{}</string><string>watch</string><string>--require-receiver</string><string>--otlp-addr</string><string>{addr}</string>
  </array>
  <key>EnvironmentVariables</key><dict><key>AFOOT_STATE_DIR</key><string>{}</string></dict>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
  <key>Umask</key><integer>63</integer>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict></plist>
"#,
        xml_escape(binary),
        xml_escape(state_dir),
        xml_escape(&logs.join("watch.stdout.log")),
        xml_escape(&logs.join("watch.stderr.log")),
    )
}

fn systemd_unit(binary: &Path, addr: SocketAddr, state_dir: &Path) -> String {
    format!(
        "[Unit]\nDescription=Agentic Footprint resident receiver\n\n[Service]\nType=simple\nExecStart={} watch --require-receiver --otlp-addr {addr}\nEnvironment={}\nUMask=0077\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_escape(binary),
        systemd_environment("AFOOT_STATE_DIR", state_dir),
    )
}

fn xml_escape(path: &Path) -> String {
    path.to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_escape(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value
        .bytes()
        .any(|byte| byte.is_ascii_whitespace() || byte == b'"' || byte == b'\\')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.into_owned()
    }
}

fn systemd_environment(name: &str, path: &Path) -> String {
    format!(
        "\"{name}={}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheduled_task_is_for_this_user_only_and_needs_no_elevation() {
        let binary = Path::new(r"C:\Users\Ann Lee\.local\bin\afoot.exe");
        let state = Path::new(r"C:\Users\Ann Lee\AppData\Local\afoot\");
        let addr = "127.0.0.1:4318".parse().unwrap();
        let user = task_user(Some("DESK".into()), Some("ann".into())).unwrap();
        assert_eq!(user, r"DESK\ann");
        let xml = task_xml(binary, addr, state, &user).unwrap();
        // Both places name the user: a logon trigger without one means
        // "any user", which needs an administrator.
        assert_eq!(xml.matches(r"<UserId>DESK\ann</UserId>").count(), 2);
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains(r"<Command>C:\Users\Ann Lee\.local\bin\afoot.exe</Command>"));
        // The state directory is one quoted argument, with no backslash
        // left to escape the closing quote.
        assert!(xml.contains(
            r"watch --require-receiver --otlp-addr 127.0.0.1:4318 --as-service &quot;C:\Users\Ann Lee\AppData\Local\afoot&quot;</Arguments>"
        ));
        assert!(!xml.contains("--debug"));
        // No time limit: the default would stop it after three days.
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }

    #[test]
    fn a_task_is_never_registered_for_an_unnamed_user() {
        assert!(task_user(Some("DESK".into()), None).is_err());
        assert!(task_user(None, Some("  ".into())).is_err());
        assert_eq!(task_user(None, Some("ann".into())).unwrap(), "ann");
        assert!(windows_argument(Path::new("C:\\a\"b")).is_err());
    }

    #[test]
    fn the_endpoint_is_read_back_from_the_utf16_task_definition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TASK_FILE);
        let xml = task_xml(
            Path::new("afoot.exe"),
            "127.0.0.1:4399".parse().unwrap(),
            Path::new(r"C:\state"),
            "ann",
        )
        .unwrap();
        let bytes = utf16_with_bom(&xml);
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        fs::write(&path, bytes).unwrap();
        let read = read_definition(&path).unwrap();
        assert_eq!(read, xml);
        assert_eq!(otlp_addr_in(&read).as_deref(), Some("127.0.0.1:4399"));
    }

    #[test]
    fn service_definitions_run_watch_without_debug() {
        let binary = Path::new("/tmp/afoot binary");
        let state = Path::new("/tmp/afoot state");
        let addr = "127.0.0.1:4318".parse().unwrap();
        let launchd = launchd_plist(binary, addr, state, Path::new("/tmp/logs"));
        let systemd = systemd_unit(binary, addr, state);
        assert!(launchd.contains("<string>watch</string>"));
        assert!(systemd.contains(" --otlp-addr 127.0.0.1:4318"));
        assert!(!launchd.contains("--debug"));
        assert!(!systemd.contains("--debug"));
        assert!(launchd.contains("watch.stderr.log"));
        assert!(systemd.contains("Restart=on-failure"));
        // A service that cannot bind its port must exit so the manager
        // retries, rather than run on without a receiver.
        assert!(launchd.contains("<string>--require-receiver</string>"));
        assert!(systemd.contains(" --require-receiver"));
    }

    /// The manager creates the log files, and anything the service writes
    /// before `afoot watch` restricts the state dir: both owner-only.
    #[test]
    fn service_definitions_create_files_owner_only() {
        let binary = Path::new("/tmp/afoot");
        let state = Path::new("/tmp/state");
        let addr = "127.0.0.1:4318".parse().unwrap();
        let launchd = launchd_plist(binary, addr, state, Path::new("/tmp/logs"));
        assert!(
            launchd.contains("<key>Umask</key><integer>63</integer>"),
            "{launchd}"
        );
        assert!(systemd_unit(binary, addr, state).contains("\nUMask=0077\n"));
    }

    /// One-shot HTTP server answering with `head` (status line + headers).
    fn answer_once(head: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(head.as_bytes());
        });
        endpoint
    }

    #[test]
    fn a_plain_200_is_not_an_afoot_receiver() {
        let endpoint =
            answer_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let error = verify_reachable(&endpoint).unwrap_err();
        assert!(
            format!("{error:#}").contains("not as an afoot OTLP receiver"),
            "{error:#}"
        );
    }

    #[test]
    fn an_identified_afoot_receiver_is_reachable() {
        let endpoint = answer_once(concat!(
            "HTTP/1.1 200 OK\r\nX-Agentic-Footprint: ",
            env!("CARGO_PKG_VERSION"),
            "\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        verify_reachable(&endpoint).unwrap();
    }

    #[test]
    fn an_afoot_receiver_of_another_version_needs_a_restart() {
        let endpoint = answer_once(
            "HTTP/1.1 200 OK\r\nX-Agentic-Footprint: 0.0.0-old\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let error = verify_reachable(&endpoint).unwrap_err();
        assert!(format!("{error:#}").contains("0.0.0-old"), "{error:#}");
    }

    #[test]
    fn the_installed_receiver_address_is_read_back_from_either_definition() {
        let binary = Path::new("/tmp/afoot");
        let state = Path::new("/tmp/state");
        let addr = "127.0.0.1:5000".parse().unwrap();
        let plist = launchd_plist(binary, addr, state, Path::new("/tmp/logs"));
        let unit = systemd_unit(binary, addr, state);
        assert_eq!(otlp_addr_in(&plist).as_deref(), Some("127.0.0.1:5000"));
        assert_eq!(otlp_addr_in(&unit).as_deref(), Some("127.0.0.1:5000"));
        assert_eq!(otlp_addr_in("ExecStart=/tmp/afoot watch"), None);
    }

    #[test]
    fn endpoint_must_be_loopback_http_logs() {
        assert_eq!(
            endpoint_addr(DEFAULT_LOGS_ENDPOINT).unwrap(),
            "127.0.0.1:4318".parse().unwrap()
        );
        assert!(endpoint_addr("https://127.0.0.1:4318/v1/logs").is_err());
        assert!(endpoint_addr("http://0.0.0.0:4318/v1/logs").is_err());
    }

    #[test]
    fn service_definition_write_reports_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service-definition");

        assert!(write_if_changed(&path, b"first").unwrap());
        assert!(!write_if_changed(&path, b"first").unwrap());
        assert!(write_if_changed(&path, b"second").unwrap());
    }

    #[test]
    fn launchd_update_avoids_reloading_unchanged_service() {
        assert_eq!(launchd_update(true, false), LaunchdUpdate::Restart);
        assert_eq!(launchd_update(false, false), LaunchdUpdate::Bootstrap);
        assert_eq!(launchd_update(false, true), LaunchdUpdate::Bootstrap);
        assert_eq!(launchd_update(true, true), LaunchdUpdate::Reload);
    }
}
