use std::{
    collections::{HashMap, HashSet},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};
use url::Url;

use crate::payload::{ActivePayload, PENDING_MEDIA_URL_KEY};

const MEDIA_CHUNK_BYTES: usize = 192 * 1024;
const PENDING_MEDIA_PARTS_KEY: &str = "__BACKGROUND_STUDIO_PENDING_MEDIA_PARTS__";

const REMOVE_RENDERER_PAYLOAD: &str = r#"(() => {
  const state = window.__FACTORY_BACKGROUND_STUDIO__;
  if (state?.cleanup) return state.cleanup();
  document.getElementById("factory-background-layer")?.remove();
  document.getElementById("factory-background-style")?.remove();
  document.getElementById("factory-background-early-transparency")?.remove();
  document.getElementById("factory-background-probe")?.remove();
  document.documentElement?.classList.remove(
    "factory-background-active", "factory-background-home", "factory-background-task",
    "factory-background-home-disabled", "factory-background-task-disabled", "factory-background-fit-tile"
  );
  delete window.__FACTORY_BACKGROUND_STUDIO__;
  return true;
})()"#;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpTarget {
    id: String,
    #[serde(rename = "type")]
    target_type: String,
    url: String,
    web_socket_debugger_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpVersion {
    web_socket_debugger_url: String,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn validate_websocket_url(
    value: &str,
    port: u16,
    kind: &str,
    id: Option<&str>,
) -> Result<String, String> {
    let url = Url::parse(value).map_err(|_| "CDP WebSocket 地址无效。".to_string())?;
    let hostname = url.host_str().unwrap_or_default();
    let loopback = matches!(hostname, "127.0.0.1" | "localhost" | "::1");
    let expected = id
        .map(|id| format!("/devtools/{kind}/{id}"))
        .unwrap_or_else(|| format!("/devtools/{kind}/"));
    let valid_path = id.map_or_else(
        || url.path().starts_with(&expected),
        |_| url.path() == expected,
    );
    if url.scheme() != "ws"
        || !loopback
        || url.port() != Some(port)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !valid_path
    {
        return Err("CDP WebSocket 地址未通过本机回环校验。".to_string());
    }
    Ok(url.to_string())
}

fn fetch_json<T: for<'de> Deserialize<'de>>(port: u16, resource: &str) -> Result<T, String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?
        .get(format!("http://127.0.0.1:{port}{resource}"))
        .header("Cache-Control", "no-store")
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("CDP 返回 HTTP {}", status.as_u16()));
    }
    let bytes = response.bytes().map_err(|error| error.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("CDP HTTP 响应超过大小上限。".to_string());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

pub fn read_browser_identity(port: u16) -> Result<String, String> {
    let version: CdpVersion = fetch_json(port, "/json/version")?;
    let websocket =
        validate_websocket_url(&version.web_socket_debugger_url, port, "browser", None)?;
    let url = Url::parse(&websocket).map_err(|error| error.to_string())?;
    let id = url
        .path()
        .strip_prefix("/devtools/browser/")
        .filter(|id| valid_id(id))
        .ok_or_else(|| "CDP 浏览器身份无效。".to_string())?;
    Ok(id.to_string())
}

/// Factory 1.0 打包版用 loadFile 打开 `%LOCALAPPDATA%\Factory\app-<版本>\resources\app.asar`
/// 内的 `.vite/renderer/main_window/index.html`；另外接受应用自己注册的特权协议
/// `factory-app://desktop/`。`<webview>` 访客页是 webview 类型，不在 page 目标里。
fn is_main_factory_target_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    match url.scheme() {
        "file" => {
            let path = url.path().to_ascii_lowercase().replace('\\', "/");
            let renderer =
                path.ends_with("/resources/app.asar/.vite/renderer/main_window/index.html");
            let under_factory = path
                .split('/')
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair[0] == "factory" && pair[1].starts_with("app-"));
            renderer && under_factory
        }
        "factory-app" => {
            url.host_str() == Some("desktop")
                && matches!(url.path(), "/" | "/index.html" | "/main_window/index.html")
        }
        _ => false,
    }
}

fn list_targets(port: u16, browser_id: &str) -> Result<Vec<CdpTarget>, String> {
    if read_browser_identity(port)? != browser_id {
        return Err("CDP 浏览器身份已变化，拒绝继续注入。".to_string());
    }
    let targets: Vec<CdpTarget> = fetch_json(port, "/json/list")?;
    Ok(targets
        .into_iter()
        .filter(|target| {
            target.target_type == "page"
                && is_main_factory_target_url(&target.url)
                && valid_id(&target.id)
                && validate_websocket_url(
                    &target.web_socket_debugger_url,
                    port,
                    "page",
                    Some(&target.id),
                )
                .is_ok()
        })
        .collect())
}

enum WorkerRequest {
    Send {
        method: String,
        params: Value,
        response: mpsc::Sender<Result<Value, String>>,
    },
    Close,
}

type CdpSocket = WebSocket<MaybeTlsStream<TcpStream>>;

fn socket_command(
    socket: &mut CdpSocket,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = *next_id;
    *next_id += 1;
    socket
        .send(Message::Text(
            serde_json::to_string(&json!({ "id": id, "method": method, "params": params }))
                .map_err(|error| error.to_string())?
                .into(),
        ))
        .map_err(|error| error.to_string())?;
    loop {
        let message = socket.read().map_err(|error| error.to_string())?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("CDP 命令失败");
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            return Err(format!("{message} ({code})"));
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

struct CdpSession {
    sender: mpsc::Sender<WorkerRequest>,
    closed: Arc<AtomicBool>,
}

impl CdpSession {
    fn open(target: &CdpTarget, port: u16) -> Result<Self, String> {
        let websocket_url = validate_websocket_url(
            &target.web_socket_debugger_url,
            port,
            "page",
            Some(&target.id),
        )?;
        let (sender, receiver) = mpsc::channel();
        let (setup_sender, setup_receiver) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = Arc::clone(&closed);
        thread::Builder::new()
            .name(format!("cdp-{}", target.id))
            .spawn(move || {
                let setup = (|| {
                    let (mut socket, _) =
                        connect(websocket_url.as_str()).map_err(|error| error.to_string())?;
                    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(30)))
                            .map_err(|error| error.to_string())?;
                        // 大图 base64 写入可达数 MB，10s 写超时会把会话打坏并堵死后续命令。
                        stream
                            .set_write_timeout(Some(Duration::from_secs(120)))
                            .map_err(|error| error.to_string())?;
                    }
                    let mut next_id = 1;
                    socket_command(&mut socket, &mut next_id, "Runtime.enable", json!({}))?;
                    socket_command(&mut socket, &mut next_id, "Page.enable", json!({}))?;
                    Ok::<_, String>((socket, next_id))
                })();
                match setup {
                    Ok((mut socket, mut next_id)) => {
                        let _ = setup_sender.send(Ok(()));
                        while let Ok(request) = receiver.recv() {
                            match request {
                                WorkerRequest::Send {
                                    method,
                                    params,
                                    response,
                                } => {
                                    let result =
                                        socket_command(&mut socket, &mut next_id, &method, params);
                                    let _ = response.send(result);
                                }
                                WorkerRequest::Close => {
                                    let _ = socket.close(None);
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = setup_sender.send(Err(error));
                    }
                }
                worker_closed.store(true, Ordering::Relaxed);
            })
            .map_err(|error| error.to_string())?;
        setup_receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "CDP 连接超时。".to_string())??;
        Ok(Self { sender, closed })
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        if self.is_closed() {
            return Err("CDP 会话已关闭。".to_string());
        }
        // 大图 base64 注入可达数 MB；固定 11s 会误杀仍在写 socket 的命令，
        // 并让后续命令堵在 worker 队列里，表现为「正在处理」卡死、退出也无响应。
        let approx_bytes = params.to_string().len() as u64;
        let timeout_secs = (20 + approx_bytes / 80_000).clamp(20, 180);
        let (response, receiver) = mpsc::channel();
        self.sender
            .send(WorkerRequest::Send {
                method: method.to_string(),
                params,
                response,
            })
            .map_err(|_| "CDP 会话已关闭。".to_string())?;
        match receiver.recv_timeout(Duration::from_secs(timeout_secs)) {
            Ok(result) => result,
            Err(_) => {
                // 超时后的 socket 仍可能在处理旧命令，不能继续向同一 worker 排队。
                self.closed.store(true, Ordering::Release);
                Err(format!("CDP 命令超时：{method}（已等待 {timeout_secs}s）"))
            }
        }
    }

    fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let result = self.send(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "userGesture": false
            }),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let detail = details
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("未知异常");
            return Err(format!(
                "Factory 渲染页执行背景脚本失败：{}",
                detail.chars().take(300).collect::<String>()
            ));
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

fn clear_pending_media(session: &CdpSession, revoke_url: bool) {
    let parts_key = serde_json::to_string(PENDING_MEDIA_PARTS_KEY).expect("key is serializable");
    let url_key = serde_json::to_string(PENDING_MEDIA_URL_KEY).expect("key is serializable");
    let revoke = if revoke_url {
        format!(
            "const url=window[{url_key}];if(typeof url==='string'&&url.startsWith('blob:'))URL.revokeObjectURL(url);"
        )
    } else {
        String::new()
    };
    let _ = session.evaluate(&format!(
        "(()=>{{{revoke}delete window[{parts_key}];delete window[{url_key}];return true;}})()"
    ));
}

fn upload_media(session: &CdpSession, payload: &ActivePayload) -> Result<(), String> {
    let parts_key =
        serde_json::to_string(PENDING_MEDIA_PARTS_KEY).map_err(|error| error.to_string())?;
    let url_key =
        serde_json::to_string(PENDING_MEDIA_URL_KEY).map_err(|error| error.to_string())?;
    session.evaluate(&format!(
        "(()=>{{const url=window[{url_key}];if(typeof url==='string'&&url.startsWith('blob:'))URL.revokeObjectURL(url);window[{url_key}]=undefined;window[{parts_key}]=[];return true;}})()"
    ))?;
    for chunk in payload.media_bytes.chunks(MEDIA_CHUNK_BYTES) {
        let encoded =
            serde_json::to_string(&STANDARD.encode(chunk)).map_err(|error| error.to_string())?;
        session.evaluate(&format!(
            "(()=>{{const binary=atob({encoded});const bytes=new Uint8Array(binary.length);for(let i=0;i<binary.length;i+=1)bytes[i]=binary.charCodeAt(i);window[{parts_key}].push(bytes);return bytes.length;}})()"
        ))?;
    }
    let mime =
        serde_json::to_string(&payload.media_mime_type).map_err(|error| error.to_string())?;
    session.evaluate(&format!(
        "(()=>{{const parts=window[{parts_key}];if(!Array.isArray(parts))throw new Error('背景媒体分块状态丢失');window[{url_key}]=URL.createObjectURL(new Blob(parts,{{type:{mime}}}));delete window[{parts_key}];return window[{url_key}];}})()"
    ))?;
    Ok(())
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        let _ = self.sender.send(WorkerRequest::Close);
        self.closed.store(true, Ordering::Relaxed);
    }
}

/// 调试端口属于同一个 Factory 浏览器实例，且主渲染页已经创建并有文档。
pub fn main_page_ready(port: u16, browser_id: &str) -> Result<bool, String> {
    let target = list_targets(port, browser_id)?
        .into_iter()
        .next()
        .ok_or_else(|| "Factory 主页面尚未创建。".to_string())?;
    let session = CdpSession::open(&target, port)?;
    Ok(session
        .evaluate("Boolean(document.documentElement)")?
        .as_bool()
        .unwrap_or(false))
}

struct ManagedSession {
    session: CdpSession,
    early_script_id: Option<String>,
    revision: Option<String>,
}

struct InjectorInner {
    port: u16,
    browser_id: String,
    sessions: HashMap<String, ManagedSession>,
    payload: Option<ActivePayload>,
    paused: bool,
}

fn early_payload_for(payload: &str, revision: &str) -> String {
    let safe_revision = serde_json::to_string(revision).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(() => {{
  const revision = {safe_revision};
  const run = () => {{
    // 新文档脚本执行时 HTML 解析尚未开始，documentElement/head 都还不存在；
    // 此时插入节点会变成文档根元素并破坏 Factory 页面解析，必须等 head 出现。
    if (!document.documentElement || !document.head) return false;
    try {{ {payload}; return true; }} catch {{ return false; }}
  }};
  if (!run()) {{
    const observer = new MutationObserver(() => {{
      if (run()) observer.disconnect();
    }});
    observer.observe(document, {{ childList: true, subtree: true }});
    setTimeout(() => observer.disconnect(), 30000);
  }}
  return revision;
}})()"#
    )
}

fn remove_from_session(managed: &mut ManagedSession) {
    if let Some(identifier) = managed.early_script_id.take() {
        let _ = managed.session.send(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({ "identifier": identifier }),
        );
    }
    let _ = managed.session.evaluate(REMOVE_RENDERER_PAYLOAD);
    managed.revision = None;
}

const EARLY_TRANSPARENCY_SCRIPT: &str = r#"(() => {
  const insert = () => {
    // 新文档脚本早于 HTML 解析执行：此时 documentElement 为 null，直接 appendChild 到 document
    // 会让 <style> 成为文档根元素，Factory 页面随之无法渲染（移除后整页空白）。
    if (!document.documentElement || !document.head) return false;
    if (document.getElementById("factory-background-early-transparency")) return true;
    const style = document.createElement("style");
    style.id = "factory-background-early-transparency";
    style.textContent = "html,body,#root{background:transparent!important;background-color:transparent!important}";
    document.head.appendChild(style);
    return true;
  };
  try {
    if (!insert()) {
      const observer = new MutationObserver(() => {
        if (insert()) observer.disconnect();
      });
      observer.observe(document, { childList: true, subtree: true });
      setTimeout(() => observer.disconnect(), 30000);
    }
  } catch {}
})()"#;

fn apply_to_session(managed: &mut ManagedSession, payload: &ActivePayload) -> Result<(), String> {
    let revision = &payload.revision;
    // 渲染页重载（如错误页的 Reload Application、Ctrl+R）后 target id 与 CDP 会话都不变，
    // 但页面状态已清空；媒体较大时新文档脚本只含透明样式，必须在这里重新完整注入。
    if managed.revision.is_some() && !page_has_payload(&managed.session) {
        managed.revision = None;
    }
    if managed.revision.as_deref() == Some(revision) {
        return Ok(());
    }
    if let Some(identifier) = managed.early_script_id.take() {
        let _ = managed.session.send(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({ "identifier": identifier }),
        );
    }
    managed
        .session
        .send("Page.setBypassCSP", json!({ "enabled": true }))?;
    let early_source = payload
        .early_script
        .as_deref()
        .map(|script| early_payload_for(script, revision))
        .unwrap_or_else(|| EARLY_TRANSPARENCY_SCRIPT.to_string());
    let early = managed.session.send(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": early_source }),
    )?;
    managed.early_script_id = early
        .get("identifier")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Err(error) = upload_media(&managed.session, payload)
        .and_then(|_| managed.session.evaluate(&payload.script).map(|_| ()))
    {
        clear_pending_media(&managed.session, true);
        return Err(error);
    }
    clear_pending_media(&managed.session, false);
    managed.revision = Some(revision.to_string());
    Ok(())
}

fn page_has_payload(session: &CdpSession) -> bool {
    session
        .evaluate(PAGE_HAS_PAYLOAD)
        .ok()
        .and_then(|value| value.as_bool())
        // 求值失败（页面正在跳转）时不判定为丢失，下一轮同步再确认。
        .unwrap_or(true)
}

const PAGE_HAS_PAYLOAD: &str = "Boolean(window.__FACTORY_BACKGROUND_STUDIO__ && document.getElementById('factory-background-layer'))";

fn sync_inner(inner: &Arc<Mutex<InjectorInner>>, target_count: &AtomicUsize, force: bool) {
    let Ok(mut inner) = inner.lock() else {
        return;
    };
    let Ok(targets) = list_targets(inner.port, &inner.browser_id) else {
        target_count.store(inner.sessions.len(), Ordering::Relaxed);
        return;
    };
    let target_ids = targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<HashSet<_>>();
    inner
        .sessions
        .retain(|id, managed| target_ids.contains(id.as_str()) && !managed.session.is_closed());
    for target in targets {
        if inner.sessions.contains_key(&target.id) {
            continue;
        }
        let Ok(session) = CdpSession::open(&target, inner.port) else {
            continue;
        };
        let probe = session
            .evaluate("Boolean(document.documentElement)")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if probe {
            inner.sessions.insert(
                target.id,
                ManagedSession {
                    session,
                    early_script_id: None,
                    revision: None,
                },
            );
        }
    }
    if !inner.paused {
        let payload = inner.payload.clone();
        if let Some(payload) = payload {
            for managed in inner.sessions.values_mut() {
                if force {
                    managed.revision = None;
                }
                if let Err(error) = apply_to_session(managed, &payload) {
                    eprintln!("CDP 注入失败: {error}");
                }
            }
        }
    }
    target_count.store(inner.sessions.len(), Ordering::Relaxed);
}

pub struct InjectorEngine {
    inner: Arc<Mutex<InjectorInner>>,
    target_count: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InjectorEngine {
    pub fn new(port: u16, browser_id: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InjectorInner {
                port,
                browser_id,
                sessions: HashMap::new(),
                payload: None,
                paused: false,
            })),
            target_count: Arc::new(AtomicUsize::new(0)),
            stopping: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }

    pub fn start(&mut self, payload: ActivePayload) -> Result<(), String> {
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "CDP 注入状态锁已损坏。".to_string())?;
            inner.payload = Some(payload);
            inner.paused = false;
        }
        sync_inner(&self.inner, &self.target_count, false);
        if self.thread.is_none() {
            let inner = Arc::clone(&self.inner);
            let count = Arc::clone(&self.target_count);
            let stopping = Arc::clone(&self.stopping);
            self.thread = Some(
                thread::Builder::new()
                    .name("factory-cdp-sync".to_string())
                    .spawn(move || {
                        while !stopping.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(1200));
                            if !stopping.load(Ordering::Relaxed) {
                                sync_inner(&inner, &count, false);
                            }
                        }
                    })
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(())
    }

    pub fn update(&self, payload: ActivePayload) -> Result<(), String> {
        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| "CDP 注入状态锁已损坏。".to_string())?;
            inner.payload = Some(payload);
            inner.paused = false;
        }
        sync_inner(&self.inner, &self.target_count, true);
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "CDP 注入状态锁已损坏。".to_string())?;
        inner.paused = true;
        for managed in inner.sessions.values_mut() {
            remove_from_session(managed);
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "CDP 注入状态锁已损坏。".to_string())?;
        inner.paused = true;
        for managed in inner.sessions.values_mut() {
            remove_from_session(managed);
            let _ = managed
                .session
                .send("Page.setBypassCSP", json!({ "enabled": false }));
        }
        inner.sessions.clear();
        self.target_count.store(0, Ordering::Relaxed);
        Ok(())
    }

    pub fn active_targets(&self) -> u32 {
        self.target_count.load(Ordering::Relaxed) as u32
    }

    pub fn is_connected(&self) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        match read_browser_identity(inner.port) {
            Ok(id) => id == inner.browser_id,
            Err(_) => false,
        }
    }
}

impl Drop for InjectorEngine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_websocket_urls() {
        assert!(validate_websocket_url(
            "ws://127.0.0.1:9227/devtools/page/page-1",
            9227,
            "page",
            Some("page-1")
        )
        .is_ok());
        for value in [
            "ws://192.168.1.2:9227/devtools/page/page-1",
            "wss://127.0.0.1:9227/devtools/page/page-1",
            "ws://127.0.0.1:9228/devtools/page/page-1",
            "ws://user@127.0.0.1:9227/devtools/page/page-1",
        ] {
            assert!(validate_websocket_url(value, 9227, "page", Some("page-1")).is_err());
        }
    }

    #[test]
    fn injects_only_factory_app_pages() {
        assert!(is_main_factory_target_url(
            "file:///C:/Users/example/AppData/Local/Factory/app-1.0.0/resources/app.asar/.vite/renderer/main_window/index.html#/sessions"
        ));
        assert!(is_main_factory_target_url(
            "file:///C:/Users/example/AppData/Local/Factory/app-1.2.3/resources/app.asar/.vite/renderer/main_window/index.html#/sessions/243ee8dc-06b1-4543-8a6d-525089223cd4"
        ));
        assert!(is_main_factory_target_url(
            "factory-app://desktop/index.html"
        ));
        assert!(!is_main_factory_target_url(
            "file:///C:/Users/example/AppData/Local/Factory/app-1.0.0/resources/app.asar/.vite/renderer/other/index.html"
        ));
        assert!(!is_main_factory_target_url(
            "file:///C:/Users/example/AppData/Local/Other/app-1.0.0/resources/app.asar/.vite/renderer/main_window/index.html"
        ));
        assert!(!is_main_factory_target_url("factory-app://evil/index.html"));
        assert!(!is_main_factory_target_url("https://app.factory.ai/"));
        assert!(!is_main_factory_target_url(
            "http://localhost:5173/index.html"
        ));
        assert!(!is_main_factory_target_url(
            "devtools://devtools/bundled/inspector.html"
        ));
    }

    #[test]
    fn early_payload_contains_revision_and_cleanup_is_reversible() {
        let payload = early_payload_for("window.test = true", "revision-1");
        assert!(payload.contains("revision-1"));
        assert!(payload.contains("MutationObserver"));
        assert!(payload.contains("!document.head"));
        assert!(!payload.contains("document.documentElement || document"));
        assert!(EARLY_TRANSPARENCY_SCRIPT.contains("document.head.appendChild"));
        assert!(!EARLY_TRANSPARENCY_SCRIPT.contains("|| document)"));
        assert!(REMOVE_RENDERER_PAYLOAD.contains("cleanup"));
    }

    #[test]
    #[ignore = "requires a live Factory CDP endpoint on port 9247"]
    fn reads_live_factory_browser_identity() {
        let identity = read_browser_identity(9247).expect("read live browser identity");
        assert!(valid_id(&identity));
    }

    #[test]
    #[ignore = "requires a live Factory CDP endpoint on port 9247"]
    fn reads_live_factory_with_reqwest() {
        let body = reqwest::blocking::get("http://127.0.0.1:9247/json/version")
            .expect("connect with reqwest")
            .text()
            .expect("read response");
        assert!(body.contains("webSocketDebuggerUrl"));
    }

    #[test]
    #[ignore = "requires a live Factory CDP endpoint on port 9247"]
    fn connects_to_live_factory_page_session() {
        let identity = read_browser_identity(9247).expect("read live browser identity");
        let target = list_targets(9247, &identity)
            .expect("list live targets")
            .into_iter()
            .next()
            .expect("at least one Factory page target");
        let session = CdpSession::open(&target, 9247).expect("open CDP page session");
        assert_eq!(
            session
                .evaluate("Boolean(document.documentElement)")
                .expect("evaluate page probe"),
            Value::Bool(true)
        );
        assert_eq!(
            session
                .evaluate("Boolean(window.__FACTORY_BACKGROUND_STUDIO__)")
                .expect("verify injected background state"),
            Value::Bool(true)
        );
    }
}
