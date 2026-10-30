use std::{
    fs,
    net::TcpListener,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::{
    injector::{main_page_ready, read_browser_identity, InjectorEngine},
    managed_launch::{
        normalize_executable_path, running_image_paths, snapshot_executable_processes, Observation,
        ProcessRecord, ProcessTracker, WatcherAction, DEBUG_PORT_WAIT, MSG_AUTO_APPLIED,
        MSG_DEBUG_TIMEOUT, MSG_EXISTING, MSG_SUSPENDED, MSG_TAKING_OVER, MSG_WAITING,
    },
    models::RuntimeStatus,
    payload::ActivePayload,
    settings::write_json_transaction,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PREFERRED_DEBUG_PORT: u16 = 9247;
const RUNTIME_SCHEMA_VERSION: u8 = 1;
const EXECUTABLE_NAME: &str = "factory-desktop.exe";

/// Factory 桌面版（Squirrel.Windows 安装）：`%LOCALAPPDATA%\Factory` 下的
/// `factory-desktop.exe` 只是启动桩，真正的 Electron 主程序在 `app-<版本>\factory-desktop.exe`。
/// 更新后旧版本目录可能仍在，且旧版本进程可能仍在运行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct FactoryInstall {
    root: String,
    executable: String,
    version: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeState {
    schema_version: u8,
    port: u16,
    browser_id: String,
    executable: String,
    version: String,
    created_at: String,
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn encoded_powershell(script: &str) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    STANDARD.encode(bytes)
}

fn run_powershell(script: &str, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded_powershell(script),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| error.to_string())?;
    if child
        .wait_timeout(timeout)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err("PowerShell 操作超时。".to_string());
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            if stdout.is_empty() {
                "PowerShell 操作失败。".to_string()
            } else {
                stdout
            }
        } else {
            stderr
        })
    }
}

fn normalized_path(path: &str) -> String {
    normalize_executable_path(path)
}

fn factory_root() -> Option<PathBuf> {
    // 单元测试绝不能解析到真实安装目录：restore/apply 会结束并重启用户正在使用的 Factory。
    #[cfg(test)]
    {
        Some(std::env::temp_dir().join("factory-background-studio-test-missing-install"))
    }
    #[cfg(not(test))]
    {
        std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("Factory"))
    }
}

/// `app-1.0.0` → `[1, 0, 0]`；Squirrel 预发布目录（如 `app-1.1.0-beta.2`）按主版本号比较。
fn parse_app_version(directory_name: &str) -> Option<(Vec<u64>, String)> {
    let version = directory_name.strip_prefix("app-")?;
    if version.is_empty() || version.len() > 64 {
        return None;
    }
    let core = version.split(['-', '+']).next()?;
    let parts = core
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty() {
        return None;
    }
    Some((parts, version.to_string()))
}

struct Candidate {
    order: Vec<u64>,
    install: FactoryInstall,
}

fn installed_candidates(root: &Path) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((order, version)) = parse_app_version(&name) else {
            continue;
        };
        let directory = entry.path();
        let executable = directory.join(EXECUTABLE_NAME);
        let asar = directory.join("resources").join("app.asar");
        if !executable.is_file() || !asar.is_file() {
            continue;
        }
        candidates.push(Candidate {
            order,
            install: FactoryInstall {
                root: root.to_string_lossy().to_string(),
                executable: executable.to_string_lossy().to_string(),
                version,
            },
        });
    }
    candidates
}

/// 优先选择正在运行的版本（Factory 有单实例锁，若去启动另一个版本只会把参数转交给旧进程），
/// 没有运行中的版本时选最高版本。
fn choose_install(mut candidates: Vec<Candidate>, running: &[String]) -> Option<FactoryInstall> {
    candidates.sort_by(|left, right| right.order.cmp(&left.order));
    candidates
        .iter()
        .find(|candidate| {
            let executable = normalized_path(&candidate.install.executable);
            running.iter().any(|path| path == &executable)
        })
        .or_else(|| candidates.first())
        .map(|candidate| candidate.install.clone())
}

fn discover_factory() -> Result<FactoryInstall, String> {
    let root = factory_root().ok_or_else(|| "无法读取 LOCALAPPDATA。".to_string())?;
    discover_factory_in(&root)
}

fn discover_factory_in(root: &Path) -> Result<FactoryInstall, String> {
    let running = running_image_paths(EXECUTABLE_NAME).unwrap_or_default();
    let install = choose_install(installed_candidates(root), &running).ok_or_else(|| {
        "未找到官方 Factory 桌面应用（预期路径：%LOCALAPPDATA%\\Factory\\app-<版本>\\factory-desktop.exe）。"
            .to_string()
    })?;
    let prefix = format!(
        "{}\\app-",
        normalized_path(&install.root).trim_end_matches('\\')
    );
    let executable = normalized_path(&install.executable);
    if !executable.starts_with(&prefix) || !executable.ends_with("\\factory-desktop.exe") {
        return Err("Factory 安装路径未通过校验。".to_string());
    }
    Ok(install)
}

fn process_records_for(install: &FactoryInstall) -> Result<Vec<ProcessRecord>, String> {
    snapshot_executable_processes(&install.executable)
}

fn process_ids_for(install: &FactoryInstall) -> Result<Vec<u32>, String> {
    Ok(process_records_for(install)?
        .into_iter()
        .map(|process| process.key.pid)
        .collect())
}

fn debug_launched(install: &FactoryInstall) -> Result<bool, String> {
    Ok(process_records_for(install)?
        .iter()
        .any(|process| process.has_debug_args))
}

fn debug_ports_for(install: &FactoryInstall) -> Result<Vec<u16>, String> {
    let mut ports = process_records_for(install)?
        .into_iter()
        .filter_map(|process| process.debug_port)
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn is_disconnected_error(error: &str) -> bool {
    error.contains("CDP 会话已关闭")
        || error.contains("CDP 浏览器身份已变化")
        || error.contains("CDP 命令超时")
        || error.contains("CDP 连接超时")
        || error.contains("CDP 返回 HTTP")
}

fn session_ready(port: u16, browser_id: &str) -> bool {
    main_page_ready(port, browser_id).unwrap_or(false)
}

/// 只结束映像路径与本次发现的 `app-<版本>\factory-desktop.exe` 完全一致的进程；
/// Squirrel 启动桩、Update.exe 和 droid.exe 都不会被选中。
fn stop_verified_factory(install: &FactoryInstall) -> Result<(), String> {
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$target = {}
$processes = @(Get-CimInstance Win32_Process -Filter "Name='factory-desktop.exe'" | Where-Object {{
  $_.ExecutablePath -and [IO.Path]::GetFullPath($_.ExecutablePath).Equals($target, [StringComparison]::OrdinalIgnoreCase)
}})
foreach ($item in $processes) {{ Stop-Process -Id ([int]$item.ProcessId) -Force -ErrorAction SilentlyContinue }}
"#,
        powershell_quote(&normalized_path(&install.executable))
    );
    run_powershell(&script, Duration::from_secs(30))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while !process_ids_for(install)?.is_empty() {
        if Instant::now() >= deadline {
            return Err("Factory 未能在 15 秒内完全退出。".to_string());
        }
        thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

fn launch_factory(install: &FactoryInstall, arguments: &[String]) -> Result<(), String> {
    let executable = PathBuf::from(&install.executable);
    if !executable.is_file() {
        return Err("Factory 可执行文件不存在。".to_string());
    }
    Command::new(&executable)
        .args(arguments)
        .current_dir(executable.parent().unwrap_or(Path::new(&install.root)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动 Factory 失败：{error}"))?;
    Ok(())
}

fn debug_arguments(port: u16) -> Vec<String> {
    vec![
        "--remote-debugging-address=127.0.0.1".to_string(),
        format!("--remote-debugging-port={port}"),
    ]
}

/// 以本机回环调试端口启动 Factory，等到同一浏览器实例的主渲染页就绪。
/// Factory 打包版没有关闭 Chromium 调试开关，也不需要任何主进程补丁。
fn launch_with_debug_port(install: &FactoryInstall, port: u16) -> Result<String, String> {
    launch_factory(install, &debug_arguments(port))?;
    let started = Instant::now();
    let deadline = started + DEBUG_PORT_WAIT;
    loop {
        if let Ok(browser_id) = read_browser_identity(port) {
            if session_ready(port, &browser_id) {
                return Ok(browser_id);
            }
        }
        if started.elapsed() > Duration::from_secs(8) && process_ids_for(install)?.is_empty() {
            return Err(
                "Factory 启动后已退出，可能有其他 Factory 实例占用了单实例锁。".to_string(),
            );
        }
        if Instant::now() >= deadline {
            return Err("Factory 已启动，但调试端口上的主界面未在 45 秒内就绪。".to_string());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn select_port(preferred: u16) -> Result<u16, String> {
    for port in preferred..=preferred.saturating_add(100) {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err("无法为 Factory 分配本机调试端口。".to_string())
}

pub struct ManagedProbe {
    pub processes: Vec<ProcessRecord>,
    pub has_ready_debug_session: bool,
    pub engine_alive: bool,
}

impl ManagedProbe {
    pub fn observation(&self) -> Observation {
        Observation {
            processes: self.processes.clone(),
            has_ready_debug_session: self.has_ready_debug_session,
            engine_alive: self.engine_alive,
            now: std::time::Instant::now(),
        }
    }
}

pub struct FactoryController {
    state_path: PathBuf,
    engine: Option<InjectorEngine>,
    state: Option<RuntimeState>,
    status: RuntimeStatus,
    install_cache: Option<FactoryInstall>,
    install_checked_at: Option<Instant>,
    tracker: ProcessTracker,
    watcher_paused: bool,
}

/// Squirrel 更新会新增 `app-<版本>` 目录；缓存的安装信息定期重新发现。
const INSTALL_RECHECK_INTERVAL: Duration = Duration::from_secs(30);

impl FactoryController {
    pub fn load(data_directory: &Path) -> Self {
        let state_path = data_directory.join("runtime.json");
        let state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|content| serde_json::from_str::<RuntimeState>(&content).ok())
            .filter(|state| {
                state.schema_version == RUNTIME_SCHEMA_VERSION && !state.browser_id.is_empty()
            });
        Self {
            state_path,
            engine: None,
            state,
            status: RuntimeStatus::default(),
            install_cache: None,
            install_checked_at: None,
            tracker: ProcessTracker::new(),
            watcher_paused: false,
        }
    }

    pub fn status(&self) -> RuntimeStatus {
        let mut status = self.status.clone();
        if let Some(engine) = &self.engine {
            status.active_targets = engine.active_targets();
        }
        status
    }

    pub fn watcher_paused(&self) -> bool {
        self.watcher_paused
    }

    pub fn set_watcher_paused(&mut self, paused: bool) {
        self.watcher_paused = paused;
    }

    pub fn set_managed_status(&mut self, phase: &str, message: &str) {
        self.status.phase = phase.to_string();
        self.status.message = message.to_string();
        if phase != "error" {
            self.status.last_error = None;
        }
    }

    pub fn set_managed_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.status.phase = "error".to_string();
        self.status.message = error.clone();
        self.status.last_error = Some(error);
    }

    fn cached_install(&mut self) -> Result<FactoryInstall, String> {
        let fresh = self
            .install_checked_at
            .is_some_and(|checked| checked.elapsed() < INSTALL_RECHECK_INTERVAL);
        if let Some(install) = &self.install_cache {
            if fresh && Path::new(&install.executable).is_file() {
                return Ok(install.clone());
            }
        }
        // 正在托管的进程仍在运行时，不切换到新装的版本目录。
        if let Some(install) = self.install_cache.clone() {
            if Path::new(&install.executable).is_file()
                && !process_ids_for(&install).unwrap_or_default().is_empty()
            {
                self.install_checked_at = Some(Instant::now());
                return Ok(install);
            }
        }
        let install = discover_factory()?;
        self.install_cache = Some(install.clone());
        self.install_checked_at = Some(Instant::now());
        Ok(install)
    }

    fn state_matches(state: &RuntimeState, install: &FactoryInstall) -> bool {
        state.version == install.version
            && normalized_path(&state.executable) == normalized_path(&install.executable)
            && read_browser_identity(state.port).ok().as_deref() == Some(state.browser_id.as_str())
            && session_ready(state.port, &state.browser_id)
    }

    fn saved_session_is_live(&self, install: &FactoryInstall) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| Self::state_matches(state, install))
    }

    fn live_debug_session_ready(processes: &[ProcessRecord]) -> bool {
        for process in processes {
            let Some(port) = process.debug_port else {
                continue;
            };
            let Ok(browser_id) = read_browser_identity(port) else {
                continue;
            };
            if session_ready(port, &browser_id) {
                return true;
            }
        }
        false
    }

    pub fn probe_managed(&mut self) -> Result<ManagedProbe, String> {
        let install = self.cached_install()?;
        let processes = self.tracker.scan(&install.executable)?;
        let engine_alive = self
            .engine
            .as_ref()
            .is_some_and(InjectorEngine::is_connected);
        let has_ready_debug_session = engine_alive
            || self.saved_session_is_live(&install)
            || Self::live_debug_session_ready(&processes);
        self.status.factory_version = Some(install.version);
        Ok(ManagedProbe {
            processes,
            has_ready_debug_session,
            engine_alive,
        })
    }

    pub fn apply_watcher_action_status(&mut self, action: WatcherAction) {
        match action {
            WatcherAction::Wait => {
                if !matches!(self.status.phase.as_str(), "active" | "paused") {
                    self.set_managed_status("idle", MSG_WAITING);
                }
            }
            WatcherAction::ReleaseAndWait => {
                self.set_managed_status("idle", MSG_WAITING);
            }
            WatcherAction::ReportExistingUnmanaged => {
                self.set_managed_status("idle", MSG_EXISTING);
            }
            WatcherAction::WaitForDebugPort | WatcherAction::Takeover | WatcherAction::Attach => {
                self.set_managed_status("starting", MSG_TAKING_OVER);
            }
            WatcherAction::ReportDebugTimeout => {
                self.set_managed_error(MSG_DEBUG_TIMEOUT);
            }
            WatcherAction::KeepActive | WatcherAction::Suspend => {}
        }
    }

    pub fn auto_takeover(&mut self, payload: ActivePayload) -> Result<RuntimeStatus, String> {
        self.set_managed_status("starting", MSG_TAKING_OVER);
        self.apply(payload, true)?;
        self.set_managed_status("active", MSG_AUTO_APPLIED);
        Ok(self.status())
    }

    pub fn attach_without_restart(
        &mut self,
        payload: ActivePayload,
    ) -> Result<RuntimeStatus, String> {
        self.set_managed_status("starting", MSG_TAKING_OVER);
        self.apply(payload, false)?;
        if self.status.phase == "active" {
            self.set_managed_status("active", MSG_AUTO_APPLIED);
        }
        Ok(self.status())
    }

    pub fn release_stale_session(&mut self) -> Result<RuntimeStatus, String> {
        self.engine = None;
        self.write_state(None)?;
        self.status.active_targets = 0;
        self.set_managed_status("idle", MSG_WAITING);
        Ok(self.status())
    }

    fn drop_disconnected_engine(&mut self) {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| !engine.is_connected())
        {
            self.engine = None;
        }
    }

    fn write_state(&mut self, state: Option<RuntimeState>) -> Result<(), String> {
        self.state = state;
        match &self.state {
            Some(state) => write_json_transaction(&self.state_path, state),
            None => match fs::remove_file(&self.state_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            },
        }
    }

    fn remember_session(
        &mut self,
        install: &FactoryInstall,
        port: u16,
        browser_id: &str,
    ) -> Result<(), String> {
        self.write_state(Some(RuntimeState {
            schema_version: RUNTIME_SCHEMA_VERSION,
            port,
            browser_id: browser_id.to_string(),
            executable: install.executable.clone(),
            version: install.version.clone(),
            created_at: Utc::now().to_rfc3339(),
        }))
    }

    fn try_attach_saved(&mut self, install: &FactoryInstall) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        if !Self::state_matches(state, install) {
            return false;
        }
        self.engine = Some(InjectorEngine::new(state.port, state.browser_id.clone()));
        true
    }

    pub fn reconnect_saved(&mut self, payload: ActivePayload) -> Result<bool, String> {
        if self.state.is_none() {
            return Ok(false);
        }
        let install = self.cached_install()?;
        if !self.try_attach_saved(&install) {
            self.write_state(None)?;
            self.status = RuntimeStatus::default();
            return Ok(false);
        }
        let result = self
            .engine
            .as_mut()
            .expect("engine set after saved session validation")
            .start(payload);
        match result {
            Ok(()) => {
                self.status.phase = "active".to_string();
                self.status.message = "已自动恢复背景会话".to_string();
                self.status.factory_version = Some(install.version);
                self.status.last_error = None;
                Ok(true)
            }
            Err(error) => {
                self.engine = None;
                self.status.phase = "error".to_string();
                self.status.message = error.clone();
                self.status.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub fn apply(
        &mut self,
        payload: ActivePayload,
        restart_existing: bool,
    ) -> Result<RuntimeStatus, String> {
        self.watcher_paused = false;
        self.status.phase = "starting".to_string();
        self.status.message = "正在连接 Factory".to_string();
        self.status.last_error = None;
        let result: Result<RuntimeStatus, String> = (|| {
            let install = self.cached_install()?;
            self.drop_disconnected_engine();
            if let Some(engine) = &self.engine {
                match engine.update(payload.clone()) {
                    Ok(()) => {
                        self.status.phase = "active".to_string();
                        self.status.message = "背景已实时应用".to_string();
                        self.status.factory_version = Some(install.version);
                        return Ok(self.status());
                    }
                    Err(error) if is_disconnected_error(&error) => {
                        self.engine = None;
                    }
                    Err(error) => return Err(error),
                }
            }
            if self.try_attach_saved(&install) {
                self.engine
                    .as_mut()
                    .expect("engine set after attach")
                    .start(payload)?;
                self.status.phase = "active".to_string();
                self.status.message = "已重新连接背景会话".to_string();
                self.status.factory_version = Some(install.version);
                return Ok(self.status());
            }
            for port in debug_ports_for(&install)? {
                let Ok(browser_id) = read_browser_identity(port) else {
                    continue;
                };
                if !session_ready(port, &browser_id) {
                    continue;
                }
                self.remember_session(&install, port, &browser_id)?;
                let mut engine = InjectorEngine::new(port, browser_id);
                engine.start(payload.clone())?;
                self.engine = Some(engine);
                self.status.phase = "active".to_string();
                self.status.message = "已重新连接背景会话".to_string();
                self.status.factory_version = Some(install.version);
                return Ok(self.status());
            }
            let running = process_ids_for(&install)?;
            if !running.is_empty() && !restart_existing {
                return Err("Factory 需要重启一次以启用背景。".to_string());
            }
            if !running.is_empty() {
                stop_verified_factory(&install)?;
            }
            let port = select_port(PREFERRED_DEBUG_PORT)?;
            let browser_id = match launch_with_debug_port(&install, port) {
                Ok(browser_id) => browser_id,
                Err(error) => {
                    // 调试启动失败时不让用户停在没有 Factory 的状态。
                    let recovery =
                        stop_verified_factory(&install).and_then(|_| launch_factory(&install, &[]));
                    return Err(match recovery {
                        Ok(()) => format!("调试端口启动失败，已恢复官方 Factory：{error}"),
                        Err(recovery_error) => format!(
                            "调试端口启动失败：{error}；恢复官方 Factory 也失败：{recovery_error}"
                        ),
                    });
                }
            };
            self.remember_session(&install, port, &browser_id)?;
            let mut engine = InjectorEngine::new(port, browser_id);
            engine.start(payload)?;
            self.engine = Some(engine);
            self.status.phase = "active".to_string();
            self.status.message = "背景已应用".to_string();
            self.status.factory_version = Some(install.version);
            Ok(self.status())
        })();
        if let Err(error) = &result {
            self.status.phase = if error.contains("需要重启一次") {
                "idle".to_string()
            } else {
                "error".to_string()
            };
            self.status.message = error.clone();
            self.status.last_error = Some(error.clone());
        }
        result
    }

    pub fn pause(&mut self) -> Result<RuntimeStatus, String> {
        self.watcher_paused = true;
        if let Some(engine) = &self.engine {
            engine.pause()?;
        }
        self.status.phase = "paused".to_string();
        self.status.message = MSG_SUSPENDED.to_string();
        self.status.last_error = None;
        Ok(self.status())
    }

    /// 移除注入；只有当前 Factory 确实带着调试端口运行时，才以官方参数重启它，
    /// 普通启动的 Factory 本来就是官方状态，绝不结束它（避免打断正在进行的会话）。
    pub fn restore(&mut self) -> Result<RuntimeStatus, String> {
        self.watcher_paused = true;
        self.status.phase = "restoring".to_string();
        self.status.message = "正在恢复官方外观".to_string();
        self.status.last_error = None;
        let result: Result<RuntimeStatus, String> = (|| {
            if let Some(mut engine) = self.engine.take() {
                engine.stop()?;
            }
            if let Ok(install) = self.cached_install() {
                if debug_launched(&install)? {
                    stop_verified_factory(&install)?;
                    launch_factory(&install, &[])?;
                }
                self.status.factory_version = Some(install.version);
            }
            self.write_state(None)?;
            self.status.phase = "idle".to_string();
            self.status.message = "已恢复官方外观".to_string();
            self.status.active_targets = 0;
            Ok(self.status())
        })();
        if let Err(error) = &result {
            self.status.phase = "error".to_string();
            self.status.message = error.clone();
            self.status.last_error = Some(error.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(version: &str) -> Candidate {
        let (order, version) = parse_app_version(&format!("app-{version}")).unwrap();
        Candidate {
            order,
            install: FactoryInstall {
                root: r"C:\Users\Me\AppData\Local\Factory".to_string(),
                executable: format!(
                    r"C:\Users\Me\AppData\Local\Factory\app-{version}\factory-desktop.exe"
                ),
                version,
            },
        }
    }

    #[test]
    fn quotes_powershell_literals() {
        assert_eq!(powershell_quote("a'b"), "'a''b'");
    }

    #[test]
    fn parses_squirrel_app_directories() {
        assert_eq!(parse_app_version("app-1.0.0").unwrap().0, vec![1, 0, 0]);
        assert_eq!(parse_app_version("app-0.107.1").unwrap().0, vec![0, 107, 1]);
        assert_eq!(
            parse_app_version("app-1.1.0-beta.2").unwrap().1,
            "1.1.0-beta.2"
        );
        assert!(parse_app_version("packages").is_none());
        assert!(parse_app_version("app-").is_none());
        assert!(parse_app_version("app-x.y").is_none());
    }

    #[test]
    fn prefers_running_version_then_highest_version() {
        let pick = |running: &[&str]| {
            let running = running
                .iter()
                .map(|path| normalized_path(path))
                .collect::<Vec<_>>();
            choose_install(
                vec![
                    candidate("0.107.1"),
                    candidate("1.0.0"),
                    candidate("0.99.0"),
                ],
                &running,
            )
            .unwrap()
            .version
        };
        assert_eq!(pick(&[]), "1.0.0");
        assert_eq!(
            pick(&[r"C:\Users\Me\AppData\Local\Factory\app-0.107.1\factory-desktop.exe"]),
            "0.107.1"
        );
        // Squirrel 启动桩与真实主程序同名，不能把它当成“正在运行的版本”。
        assert_eq!(
            pick(&[r"C:\Users\Me\AppData\Local\Factory\factory-desktop.exe"]),
            "1.0.0"
        );
    }

    #[test]
    fn debug_arguments_bind_loopback_only() {
        assert_eq!(
            debug_arguments(9247),
            vec![
                "--remote-debugging-address=127.0.0.1".to_string(),
                "--remote-debugging-port=9247".to_string()
            ]
        );
    }

    #[test]
    fn selects_an_available_loopback_port() {
        let port = select_port(39_000).expect("available test port");
        assert!((39_000..=39_100).contains(&port));
    }

    #[test]
    fn treats_dead_cdp_errors_as_recoverable() {
        assert!(is_disconnected_error("CDP 会话已关闭。"));
        assert!(is_disconnected_error(
            "CDP 浏览器身份已变化，拒绝继续注入。"
        ));
        assert!(!is_disconnected_error("Factory 渲染页执行背景脚本失败：x"));
    }

    #[test]
    fn wait_and_existing_clear_stale_probe_error() {
        let dir =
            std::env::temp_dir().join(format!("factory-managed-status-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let mut controller = FactoryController::load(&dir);
        controller.set_managed_error("探测失败");
        controller.apply_watcher_action_status(WatcherAction::Wait);
        let status = controller.status();
        assert_eq!(status.phase, "idle");
        assert_eq!(status.message, MSG_WAITING);
        assert!(status.last_error.is_none());

        controller.set_managed_error("探测失败");
        controller.apply_watcher_action_status(WatcherAction::ReportExistingUnmanaged);
        let status = controller.status();
        assert_eq!(status.phase, "idle");
        assert_eq!(status.message, MSG_EXISTING);
        assert!(status.last_error.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unit_tests_never_resolve_the_real_install() {
        assert!(discover_factory().is_err());
    }

    #[test]
    #[ignore = "requires the official Factory desktop installation"]
    fn discovers_installed_factory_and_reads_processes() {
        let root =
            PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA")).join("Factory");
        let install = discover_factory_in(&root).expect("discover official Factory");
        assert!(normalized_path(&install.executable).ends_with("\\factory-desktop.exe"));
        process_ids_for(&install).expect("query verified Factory processes");
        for port in debug_ports_for(&install).expect("query verified Factory debug ports") {
            read_browser_identity(port).expect("verify Factory browser identity");
        }
    }
}
