mod controller;
mod injector;
mod managed_launch;
mod media;
mod models;
mod payload;
mod plugin;
mod plugin_ipc;
mod protocol;
mod settings;
mod worker;

use std::sync::{Arc, Mutex, MutexGuard};

use worker::WorkerState;

pub(crate) fn lock<T>(value: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    value.lock().map_err(|_| "应用状态锁已损坏。".to_string())
}

pub async fn run() -> Result<(), String> {
    let state = Arc::new(WorkerState::load()?);
    worker::start_managed_launch_worker(Arc::clone(&state));
    plugin_ipc::start(Arc::clone(&state));
    loop {
        if state.is_shutting_down() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(())
}
