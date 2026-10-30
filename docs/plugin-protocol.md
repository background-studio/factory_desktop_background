# Background Studio 插件协议

本插件是纯 Rust 无界面 worker，只由 Background Studio 壳启动。

- `pluginProtocol`: `2`
- `pluginId`: `factory`
- 可执行文件：`Factory Background Studio.exe`
- Pipe：`\\.\pipe\background-studio-factory`
- 清单：根级 `plugin.json`
- Release 产物：`FactoryBackgroundStudio-<version>-plugin.zip`

## 传输

Named Pipe，每行一个 JSON。

请求：

```json
{"id":"...","cmd":"...","params":{}}
```

成功：`{"id":"...","ok":true,"result":{...}}`

失败：`{"id":"...","ok":false,"error":"..."}`

## 命令

| 命令 | 作用 |
|------|------|
| `hello` | 返回 `pluginProtocol:2`、`pluginId`、`version`、`capabilities` |
| `configure` | 校验并下载回环媒体，保存最近一次有效配置；若当前已 active 则热更新 |
| `status` | `phase` / `message` / `activeTargets` / `paused` / `configured` / `revision` |
| `apply` | 使用最近一次有效 configure，手动接管并重新武装 watcher |
| `pause` | 完整移除注入（样式、媒体层、探针、根类名与变量）并暂停 watcher，不重启 Factory |
| `restore` | 移除注入并暂停 watcher；只有 Factory 正以调试端口运行时才以官方参数重启它 |
| `shutdown` | 结束 worker，不改当前 Factory；返回 `shutdown:true`、`keptTarget:true` |

`configure.params`：

```json
{
  "schemaVersion": 1,
  "revision": "...",
  "media": {
    "url": "http://127.0.0.1:<port>/...",
    "kind": "image",
    "mimeType": "image/png",
    "sha256": "<64 hex>",
    "byteSize": 123
  },
  "display": {}
}
```

`hello.capabilities.maxMediaBytes` 与 Manifest 都声明 64 MiB。媒体 URL 仅允许 `http://127.0.0.1` 或 `http://localhost`，拒绝 userinfo、fragment、非回环、端口 0 和超长 JSON。下载使用 `no_proxy`，并校验 `Content-Type`、`Content-Length`、`byteSize`、`sha256`、`mimeType`/`kind`。

未配置时 watcher 只等待并报告「尚未配置背景」，不会杀进程。

## 自动接管

启用后不会自动打开官方 Factory。

- 安装发现：每次探测扫描 `%LOCALAPPDATA%\Factory\app-<版本>\factory-desktop.exe`（Squirrel 布局），优先正在运行的版本，否则取最高版本；30 秒重新发现一次，正在托管的版本仍在运行时不切换。根目录的同名 `factory-desktop.exe` 只是启动桩，不会被识别为 Factory 主进程。
- 进程识别：按完整映像路径匹配 `app-<版本>\factory-desktop.exe`；`droid.exe`、`Update.exe`、启动桩都不会被结束。
- 调试启动：直接启动版本目录里的主程序，参数只有 `--remote-debugging-address=127.0.0.1 --remote-debugging-port=<9247 起>`。Factory 打包版未禁用调试开关，不改 EXE、不改 `app.asar`、不需要 inspector/WCO 补丁（Factory 自带透明的窗口控件覆盖层）。
- Factory 有单实例锁：接管时必须先结束正在运行的实例再以调试参数启动；调试启动失败会结束并以官方参数重新打开 Factory。
- 就绪判定：同一浏览器 id 的主渲染页存在且有 `document.documentElement`。
- 只注入主渲染页：`file:///…/Factory/app-<版本>/resources/app.asar/.vite/renderer/main_window/index.html` 或 `factory-app://desktop/`；`<webview>` 访客页不注入。
- 已有有效调试会话（`%LOCALAPPDATA%\FactoryBackgroundStudio\runtime.json`，版本、路径、浏览器 id 都匹配）：worker 重启后直接重连，不重启 Factory。
- 配置前已经在跑的普通进程：不自动关闭，状态为「Factory 已在运行，点立即接管可重启」；`apply` 才会重启接管。
- 渲染页重载（错误页的 Reload Application、Ctrl+R）：新文档脚本先在 `<head>` 出现后注入透明样式，同步线程发现页面状态丢失后重新完整注入（约 1.2 秒内）。
- `pause` / `restore` 暂停本插件进程内的 watcher；手动 `apply` 重新武装。
- `shutdown` 或壳停用插件：移除注入、结束 worker，不改当前 Factory。
