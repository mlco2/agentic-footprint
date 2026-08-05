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

const LABEL: &str = "dev.agentic-footprint.af-watch";
const SYSTEMD_UNIT: &str = "agentic-footprint-watch.service";
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:4318/v1/logs";

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
    /// OTLP HTTP/JSON logs endpoint the resident receiver must serve.
    #[arg(long, default_value = DEFAULT_ENDPOINT, global = true)]
    pub endpoint: String,
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
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchdUpdate {
    Restart,
    Bootstrap,
    Reload,
}

pub fn run(state_dir: &Path, args: Args) -> Result<()> {
    let binary = env::current_exe().context("resolve the af executable")?;
    match args.action {
        Action::Install => ensure_installed(state_dir, &binary, &args.endpoint),
        Action::Start => start(manager(), state_dir, &args.endpoint),
        Action::Status => status(manager(), state_dir, &args.endpoint),
    }
}

pub fn ensure_for_setup(state_dir: &Path, endpoint: &str) -> Result<()> {
    let binary = env::current_exe().context("resolve the af executable")?;
    ensure_installed(state_dir, &binary, endpoint)
}

pub fn check_for_setup(state_dir: &Path, endpoint: &str) -> Result<()> {
    status(manager(), state_dir, endpoint)
}

fn ensure_installed(state_dir: &Path, binary: &Path, endpoint: &str) -> Result<()> {
    let manager = manager();
    let addr = endpoint_addr(endpoint)?;
    let install_result = match manager {
        Manager::Launchd => install_launchd(state_dir, binary, addr),
        Manager::Systemd => install_systemd(state_dir, binary, addr),
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
        Manager::Launchd => {
            let domain = launchd_domain()?;
            run_command(
                Command::new("launchctl")
                    .arg("kickstart")
                    .arg("-k")
                    .arg(format!("{domain}/{LABEL}")),
                "restart launchd service",
            )?;
        }
        Manager::Systemd => {
            run_command(
                Command::new("systemctl").args(["--user", "restart", SYSTEMD_UNIT]),
                "restart systemd user service",
            )?;
        }
        Manager::Unsupported => return Err(manual_instructions(state_dir, endpoint)),
    }
    wait_until_reachable(endpoint, Duration::from_secs(10))?;
    println!("Resident receiver is reachable at {endpoint}");
    Ok(())
}

fn status(manager: Manager, state_dir: &Path, endpoint: &str) -> Result<()> {
    match manager {
        Manager::Launchd => {
            let domain = launchd_domain()?;
            run_command(
                Command::new("launchctl")
                    .arg("print")
                    .arg(format!("{domain}/{LABEL}")),
                "query launchd service",
            )?;
        }
        Manager::Systemd => {
            run_command(
                Command::new("systemctl").args(["--user", "status", SYSTEMD_UNIT, "--no-pager"]),
                "query systemd user service",
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

fn install_launchd(state_dir: &Path, binary: &Path, addr: SocketAddr) -> Result<()> {
    let home = home_dir()?;
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"));
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

fn manager() -> Manager {
    match env::var("AF_SERVICE_MANAGER").ok().as_deref() {
        Some("launchd") => return Manager::Launchd,
        Some("systemd") => return Manager::Systemd,
        Some("unsupported") => return Manager::Unsupported,
        Some(_) => return Manager::Unsupported,
        None => {}
    }
    if cfg!(target_os = "macos") {
        Manager::Launchd
    } else if cfg!(target_os = "linux") && env::var_os("XDG_RUNTIME_DIR").is_some() {
        Manager::Systemd
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
        bail!("{endpoint} answered, but not as an af OTLP receiver");
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

fn service_active(manager: Manager) -> Result<()> {
    match manager {
        Manager::Launchd => {
            let domain = launchd_domain()?;
            run_command(
                Command::new("launchctl")
                    .arg("print")
                    .arg(format!("{domain}/{LABEL}")),
                "verify launchd service",
            )
        }
        Manager::Systemd => run_command(
            Command::new("systemctl").args(["--user", "is-active", "--quiet", SYSTEMD_UNIT]),
            "verify systemd user service",
        ),
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
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME must be set to install a user service")
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
        Manager::Unsupported => "run `af watch` in a persistent terminal".to_string(),
    }
}

fn manual_instructions(state_dir: &Path, endpoint: &str) -> anyhow::Error {
    anyhow!(
        "no supported per-user service manager is available. Start the receiver manually with \
         `AF_STATE_DIR={} af watch` and keep it running, then rerun `af setup` to verify \
         {endpoint}. Supported automatic managers are macOS \
         launchd and Linux systemd user services",
        state_dir.display()
    )
}

fn with_manual_recovery(err: anyhow::Error, state_dir: &Path, endpoint: &str) -> anyhow::Error {
    anyhow!("{err:#}; {}", manual_instructions(state_dir, endpoint))
}

fn launchd_plist(binary: &Path, addr: SocketAddr, state_dir: &Path, logs: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>
    <string>{}</string><string>watch</string><string>--otlp-addr</string><string>{addr}</string>
  </array>
  <key>EnvironmentVariables</key><dict><key>AF_STATE_DIR</key><string>{}</string></dict>
  <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
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
        "[Unit]\nDescription=Agentic Footprint resident receiver\n\n[Service]\nType=simple\nExecStart={} watch --otlp-addr {addr}\nEnvironment={}\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_escape(binary),
        systemd_environment("AF_STATE_DIR", state_dir),
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
    fn service_definitions_run_watch_without_debug() {
        let binary = Path::new("/tmp/af binary");
        let state = Path::new("/tmp/af state");
        let addr = "127.0.0.1:4318".parse().unwrap();
        let launchd = launchd_plist(binary, addr, state, Path::new("/tmp/logs"));
        let systemd = systemd_unit(binary, addr, state);
        assert!(launchd.contains("<string>watch</string>"));
        assert!(systemd.contains(" watch --otlp-addr 127.0.0.1:4318"));
        assert!(!launchd.contains("--debug"));
        assert!(!systemd.contains("--debug"));
        assert!(launchd.contains("watch.stderr.log"));
        assert!(systemd.contains("Restart=on-failure"));
    }

    #[test]
    fn endpoint_must_be_loopback_http_logs() {
        assert_eq!(
            endpoint_addr(DEFAULT_ENDPOINT).unwrap(),
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
