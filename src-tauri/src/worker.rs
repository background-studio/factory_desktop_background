use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    controller::FactoryController,
    lock,
    managed_launch::{
        confirm_launch_after_encode, keys_of, process_key_list, BusyGuard, ConfirmedLaunch,
        WatcherAction, WatcherState, MSG_BUSY, MSG_EXISTING, MSG_UNCONFIGURED,
    },
    media::download_configured_media,
    models::RuntimeStatus,
    payload::{build_active_payload_from_bytes, ActivePayload},
    plugin::{hello_result, PLUGIN_ID, PLUGIN_PROTOCOL},
    protocol::{parse_configure, ConfigureSpec},
};

pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("FactoryBackgroundStudio")
}

#[derive(Clone)]
struct ConfiguredBackground {
    revision: String,
    payload: ActivePayload,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResult {
    plugin_protocol: u32,
    plugin_id: &'static str,
    version: &'static str,
    phase: String,
    message: String,
    active_targets: u32,
    paused: bool,
    configured: bool,
    revision: Option<String>,
}

pub struct WorkerState {
    controller: Arc<Mutex<FactoryController>>,
    runtime_status: Mutex<RuntimeStatus>,
    watcher: Mutex<WatcherState>,
    managed_busy: AtomicBool,
    configured: Mutex<Option<ConfiguredBackground>>,
    shutting_down: AtomicBool,
}

impl WorkerState {
    pub fn load() -> Result<Self, String> {
        Self::load_from(data_directory())
    }

    pub fn load_from(data_directory: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&data_directory).map_err(|error| error.to_string())?;
        let controller = FactoryController::load(&data_directory);
        let runtime_status = controller.status();
        Ok(Self {
            controller: Arc::new(Mutex::new(controller)),
            runtime_status: Mutex::new(runtime_status),
            watcher: Mutex::new(WatcherState::new()),
            managed_busy: AtomicBool::new(false),
            configured: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    pub fn is_configured(&self) -> Result<bool, String> {
        Ok(lock(&self.configured)?.is_some())
    }

    pub fn rearm_managed_watcher(&self) -> Result<(), String> {
        lock(&self.watcher)?.rearm();
        lock(&self.controller)?.set_watcher_paused(false);
        Ok(())
    }

    pub fn suspend_managed_watcher(&self) -> Result<(), String> {
        lock(&self.watcher)?.suspend();
        lock(&self.controller)?.set_watcher_paused(true);
        Ok(())
    }

    pub fn sync_managed_active(&self) -> Result<(), String> {
        let probe = lock(&self.controller)?.probe_managed()?;
        lock(&self.watcher)?.mark_managed_active(&process_key_list(&probe.processes));
        Ok(())
    }

    pub fn sync_managed_failure(&self) -> Result<(), String> {
        let processes = lock(&self.controller)?
            .probe_managed()
            .map(|probe| process_key_list(&probe.processes))
            .unwrap_or_default();
        lock(&self.watcher)?.takeover_failed(&processes);
        Ok(())
    }

    pub fn runtime_status(&self) -> Result<RuntimeStatus, String> {
        if let Ok(controller) = self.controller.try_lock() {
            let status = controller.status();
            *lock(&self.runtime_status)? = status.clone();
            return Ok(status);
        }
        Ok(lock(&self.runtime_status)?.clone())
    }

    pub fn refresh_runtime_status(&self) -> Result<RuntimeStatus, String> {
        let status = lock(&self.controller)?.status();
        *lock(&self.runtime_status)? = status.clone();
        Ok(status)
    }

    fn configured_revision(&self) -> Result<Option<String>, String> {
        Ok(lock(&self.configured)?
            .as_ref()
            .map(|configured| configured.revision.clone()))
    }

    fn active_payload(&self) -> Result<ActivePayload, String> {
        lock(&self.configured)?
            .as_ref()
            .map(|configured| configured.payload.clone())
            .ok_or_else(|| MSG_UNCONFIGURED.to_string())
    }

    pub fn status_value(&self) -> Result<Value, String> {
        let status = self.runtime_status()?;
        let configured = self.is_configured()?;
        let revision = self.configured_revision()?;
        let payload = StatusResult {
            plugin_protocol: PLUGIN_PROTOCOL,
            plugin_id: PLUGIN_ID,
            version: env!("CARGO_PKG_VERSION"),
            phase: status.phase.clone(),
            message: status.message,
            active_targets: status.active_targets,
            paused: status.phase == "paused",
            configured,
            revision,
        };
        serde_json::to_value(payload).map_err(|error| error.to_string())
    }

    pub fn hello() -> Value {
        hello_result()
    }

    /// IPC 处理入口。下载在异步侧完成，其余会抢 controller/watcher 锁的部分
    /// 全部放进 blocking 线程池，避免长时间占用 tokio runtime 工作线程
    /// 把管道 accept 循环饿死。
    pub async fn configure(self: &Arc<Self>, params: Value) -> Result<Value, String> {
        let spec = parse_configure(&params)?;
        let bytes = download_configured_media(&spec).await?;
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let payload = build_active_payload_from_bytes(
                bytes,
                &spec.media.kind,
                &spec.media.mime_type,
                &spec.display,
            )?;
            state.store_and_maybe_hot_update_blocking(spec, payload)?;
            state.status_value()
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn store_and_maybe_hot_update_blocking(
        &self,
        spec: ConfigureSpec,
        payload: ActivePayload,
    ) -> Result<(), String> {
        {
            *lock(&self.configured)? = Some(ConfiguredBackground {
                revision: spec.revision.clone(),
                payload: payload.clone(),
            });
        }
        // 必须先取进程列表、再拿 watcher 锁：tick_managed_launch 的加锁顺序是
        // controller → watcher，这里若持着 watcher 再等 controller 会构成
        // AB-BA 死锁（正是导致管道 os error 231 卡死的根因）。
        let processes = lock(&self.controller)
            .ok()
            .and_then(|mut controller| controller.probe_managed().ok())
            .map(|probe| process_key_list(&probe.processes))
            .unwrap_or_default();
        if let Ok(mut watcher) = lock(&self.watcher) {
            watcher.arm_after_configure(&processes);
        }
        if let Ok(mut controller) = lock(&self.controller) {
            if !controller.watcher_paused()
                && !matches!(controller.status().phase.as_str(), "active" | "paused")
            {
                controller.set_managed_status("idle", crate::managed_launch::MSG_WAITING);
            }
        }
        let phase = self.runtime_status()?.phase;
        if should_hot_update(&phase) {
            let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
                return Err(MSG_BUSY.to_string());
            };
            let result = lock(&self.controller)?.apply(payload, false);
            let _ = self.refresh_runtime_status();
            match result {
                Ok(_) => {
                    let _ = self.sync_managed_active();
                }
                Err(error) => {
                    let _ = self.sync_managed_failure();
                    return Err(error);
                }
            }
        } else {
            let reconnected = match lock(&self.controller) {
                Ok(mut controller) if !controller.watcher_paused() => {
                    let _ = controller.reconnect_saved(payload);
                    true
                }
                _ => false,
            };
            if reconnected {
                let _ = self.refresh_runtime_status();
            }
        }
        let _ = self.refresh_runtime_status();
        Ok(())
    }

    pub fn apply_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.rearm_managed_watcher();
        let payload = match self.active_payload() {
            Ok(payload) => payload,
            Err(error) => {
                let _ = self.sync_managed_failure();
                if let Ok(mut controller) = lock(&self.controller) {
                    controller.set_managed_status("idle", MSG_UNCONFIGURED);
                }
                let _ = self.refresh_runtime_status();
                return Err(error);
            }
        };
        let first = lock(&self.controller)?.apply(payload.clone(), false);
        let _ = self.refresh_runtime_status();
        let result = match first {
            Ok(_) => Ok(()),
            Err(error) if error.contains("需要重启一次") => {
                let retry = lock(&self.controller)?.apply(payload, true);
                let _ = self.refresh_runtime_status();
                retry.map(|_| ())
            }
            Err(error) => Err(error),
        };
        if result.is_ok() {
            let _ = self.sync_managed_active();
        } else {
            let _ = self.sync_managed_failure();
        }
        result?;
        self.status_value()
    }

    pub fn pause_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.suspend_managed_watcher();
        lock(&self.controller)?.pause()?;
        let _ = self.refresh_runtime_status();
        self.status_value()
    }

    pub fn restore_blocking(&self) -> Result<Value, String> {
        let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
            return Err(MSG_BUSY.to_string());
        };
        let _ = self.suspend_managed_watcher();
        lock(&self.controller)?.restore()?;
        let _ = self.refresh_runtime_status();
        self.status_value()
    }

    pub fn shutdown(&self) -> Result<Value, String> {
        self.shutting_down.store(true, Ordering::SeqCst);
        Ok(json!({ "shutdown": true, "keptTarget": true }))
    }

    pub fn tick_managed_launch(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::SeqCst) || self.managed_busy.load(Ordering::Acquire) {
            return Ok(());
        }
        if !self.is_configured()? {
            if let Ok(mut controller) = self.controller.try_lock() {
                if !controller.watcher_paused() && controller.status().phase != "paused" {
                    controller.set_managed_status("idle", MSG_UNCONFIGURED);
                }
            }
            let _ = self.refresh_runtime_status();
            return Ok(());
        }
        let mut controller = match self.controller.try_lock() {
            Ok(controller) => controller,
            Err(_) => return Ok(()),
        };
        if controller.watcher_paused() {
            drop(controller);
            lock(&self.watcher)?.suspend();
            return Ok(());
        }
        let probe = controller.probe_managed()?;
        let action = {
            let mut watcher = lock(&self.watcher)?;
            watcher.decide(&probe.observation())
        };
        match action {
            WatcherAction::KeepActive | WatcherAction::Suspend => Ok(()),
            WatcherAction::Wait
            | WatcherAction::ReportExistingUnmanaged
            | WatcherAction::WaitForDebugPort
            | WatcherAction::ReportDebugTimeout => {
                let before = controller.status();
                controller.apply_watcher_action_status(action);
                let changed = before.phase != controller.status().phase
                    || before.message != controller.status().message;
                drop(controller);
                if changed {
                    let _ = self.refresh_runtime_status();
                }
                Ok(())
            }
            WatcherAction::ReleaseAndWait => {
                controller.release_stale_session()?;
                drop(controller);
                let _ = self.refresh_runtime_status();
                Ok(())
            }
            WatcherAction::Attach | WatcherAction::Takeover => {
                let Some(_busy) = BusyGuard::acquire(&self.managed_busy) else {
                    return Ok(());
                };
                let original_keys = keys_of(&probe.processes);
                controller.apply_watcher_action_status(action);
                drop(controller);
                let _ = self.refresh_runtime_status();
                let restart = action == WatcherAction::Takeover;
                let result: Result<bool, String> = (|| {
                    let payload = self.active_payload()?;
                    let mut controller = lock(&self.controller)?;
                    let after = controller.probe_managed()?;
                    let confirmed =
                        confirm_launch_after_encode(action, &original_keys, &after.observation());
                    match confirmed {
                        ConfirmedLaunch::Attach => {
                            controller.attach_without_restart(payload)?;
                            Ok(true)
                        }
                        ConfirmedLaunch::Takeover => {
                            controller.auto_takeover(payload)?;
                            Ok(true)
                        }
                        ConfirmedLaunch::CancelExited | ConfirmedLaunch::CancelStale => {
                            if confirmed == ConfirmedLaunch::CancelExited {
                                let _ = controller.release_stale_session();
                            }
                            let observation = after.observation();
                            drop(controller);
                            let next = lock(&self.watcher)?.sync_to_current(&observation);
                            if let Ok(mut controller) = lock(&self.controller) {
                                controller.apply_watcher_action_status(next);
                            }
                            Ok(false)
                        }
                    }
                })();
                match result {
                    Ok(true) => {
                        let _ = self.sync_managed_active();
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Ok(false) => {
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) if error.contains(MSG_UNCONFIGURED) => {
                        let _ = self.sync_managed_failure();
                        if let Ok(mut controller) = lock(&self.controller) {
                            controller.set_managed_status("idle", MSG_UNCONFIGURED);
                        }
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) if !restart && error.contains("需要重启一次") => {
                        let _ = self.sync_managed_failure();
                        if let Ok(mut controller) = lock(&self.controller) {
                            controller.set_managed_status("idle", MSG_EXISTING);
                        }
                        let _ = self.refresh_runtime_status();
                        Ok(())
                    }
                    Err(error) => {
                        let _ = self.sync_managed_failure();
                        let _ = self.refresh_runtime_status();
                        Err(error)
                    }
                }
            }
        }
    }
}

pub fn should_hot_update(phase: &str) -> bool {
    phase == "active"
}

pub fn start_managed_launch_worker(state: Arc<WorkerState>) {
    tokio::spawn(async move {
        loop {
            if state.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            let tick_state = Arc::clone(&state);
            let outcome =
                tokio::task::spawn_blocking(move || tick_state.tick_managed_launch()).await;
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if let Ok(mut controller) = lock(&state.controller) {
                        let status = controller.status();
                        if status.phase != "error" || status.message != error {
                            controller.set_managed_error(error);
                        }
                    }
                    let _ = state.refresh_runtime_status();
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
                Err(error) => {
                    eprintln!("托管探测任务失败：{error}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_launch::{Observation, ProcessRecord, WatcherAction};
    use crate::payload::build_active_payload_from_bytes;
    use crate::protocol::hex_sha256;
    use crate::{models::DisplaySettings, models::MediaKind};
    use serde_json::json;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use uuid::Uuid;

    fn temp_state() -> WorkerState {
        let dir = std::env::temp_dir().join(format!("factory-worker-{}", Uuid::new_v4()));
        WorkerState::load_from(dir).unwrap()
    }

    fn serve_body(body: &[u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_vec();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        port
    }

    #[test]
    fn hot_update_only_when_active() {
        assert!(should_hot_update("active"));
        assert!(!should_hot_update("idle"));
        assert!(!should_hot_update("paused"));
    }

    #[test]
    fn unconfigured_tick_does_not_takeover() {
        let state = temp_state();
        state.tick_managed_launch().unwrap();
        let status = state.runtime_status().unwrap();
        assert_eq!(status.message, MSG_UNCONFIGURED);
        assert!(!state.is_configured().unwrap());

        let mut watcher = WatcherState::new();
        let observation = Observation {
            processes: vec![ProcessRecord::new(11, 100), ProcessRecord::new(12, 101)],
            ..Observation::default()
        };
        assert_eq!(
            watcher.decide(&observation),
            WatcherAction::ReportExistingUnmanaged
        );
        assert_eq!(state.runtime_status().unwrap().phase, "idle");
    }

    #[tokio::test]
    async fn configure_stores_revision_without_takeover() {
        let state = Arc::new(temp_state());
        let body = b"worker-png";
        let port = serve_body(body);
        let status = state
            .configure(json!({
                "schemaVersion": 1,
                "revision": "rev-hot",
                "media": {
                    "url": format!("http://127.0.0.1:{port}/a.png"),
                    "kind": "image",
                    "mimeType": "image/png",
                    "sha256": hex_sha256(body),
                    "byteSize": body.len()
                },
                "display": { "opacity": 0.5 }
            }))
            .await
            .unwrap();
        assert_eq!(status["configured"], true);
        assert_eq!(status["revision"], "rev-hot");
        assert_ne!(status["phase"], "active");
    }

    #[test]
    fn apply_without_configure_fails() {
        let state = temp_state();
        let error = state.apply_blocking().unwrap_err();
        assert!(error.contains("尚未配置"));
        assert_eq!(state.runtime_status().unwrap().message, MSG_UNCONFIGURED);
    }

    #[test]
    fn pause_restore_and_shutdown() {
        let state = temp_state();
        let paused = state.pause_blocking().unwrap();
        assert_eq!(paused["paused"], true);
        assert_eq!(paused["phase"], "paused");

        let restored = state.restore_blocking().unwrap();
        assert_eq!(restored["phase"], "idle");
        assert_eq!(restored["paused"], false);

        let shutdown = state.shutdown().unwrap();
        assert_eq!(shutdown["shutdown"], true);
        assert_eq!(shutdown["keptTarget"], true);
        assert!(state.is_shutting_down());
        assert_ne!(state.runtime_status().unwrap().phase, "restoring");
    }

    #[test]
    fn busy_guard_blocks_concurrent_apply() {
        let state = Arc::new(temp_state());
        let payload = build_active_payload_from_bytes(
            b"x".to_vec(),
            &MediaKind::Image,
            "image/png",
            &DisplaySettings::default(),
        )
        .unwrap();
        *lock(&state.configured).unwrap() = Some(ConfiguredBackground {
            revision: "busy".to_string(),
            payload,
        });
        let flag = &state.managed_busy;
        let first = BusyGuard::acquire(flag).unwrap();
        let error = state.apply_blocking().unwrap_err();
        assert_eq!(error, MSG_BUSY);
        drop(first);
        assert!(BusyGuard::acquire(&AtomicBool::new(false)).is_some());
    }

    #[test]
    fn configure_then_second_display_changes_payload_revision() {
        let first = build_active_payload_from_bytes(
            b"same".to_vec(),
            &MediaKind::Image,
            "image/png",
            &DisplaySettings::default(),
        )
        .unwrap();
        let mut display = DisplaySettings::default();
        display.opacity = 0.2;
        let second = build_active_payload_from_bytes(
            b"same".to_vec(),
            &MediaKind::Image,
            "image/png",
            &display,
        )
        .unwrap();
        assert_ne!(first.revision, second.revision);
    }
}
