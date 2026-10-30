---
name: factory-background-development
description: 开发、调试 Factory Background Studio（Factory/Droid 桌面应用背景插件）时使用：DOM/令牌结构、注入时机、接管与恢复的安全约束和已踩过的坑。
---

# Factory Background Studio 开发要点

## 目标应用

- Squirrel 安装：`%LOCALAPPDATA%\Factory\factory-desktop.exe` 是启动桩，真正主程序在 `app-<版本>\factory-desktop.exe`；更新后旧版本目录可能并存。
- Electron 42，`titleBarStyle: "hidden"` + 透明 `titleBarOverlay`（Factory 自带窗口控件覆盖层，**不要**套 Multica 的 inspector/WCO 补丁，也不需要改 fuse）。
- 主渲染页 `loadFile(.vite/renderer/main_window/index.html)`，路由在 hash：`#/sessions`、`#/sessions/<id>`、`#/missions`、`#/automations`、`#/settings`。
- `requestSingleInstanceLock`：调试重启前必须先结束旧实例；`webviewTag: true`，访客 webview 不注入。
- 强杀 Factory 会打断进行中的会话界面；`droid.exe` 是独立进程，不在结束范围内。

## 样式结构（不要按截图猜选择器）

- 稳定钩子只有 `data-*` / `data-testid`；styled-components 类名是哈希，不能用。
- 底色全部来自令牌：core-v2（`--core-v2-bg-page/surface/inset/raised/overlay/hover/selected`、按钮/输入/代码）与旧令牌（`--surface-1..5`、`--sidebar-*`、`--windows-content-bg`）。v2 容器把旧令牌重映射到 core-v2。
- 用户外观自定义在 `STYLE#factory-appearance-overrides`，选择器特异性 (0,2,0)；插件规则要以 `!important` 覆盖令牌，而不是逐元素清背景。
- 原色必须由探针（`#factory-background-probe`，被覆盖规则排除）实时读取；外观覆盖样式可能晚于首次注入插入，靠 head 观察器和 4 秒定时重读自愈。
- 令牌并非只用于背景：`--text-inverted` 在 v2 容器里等于 `--core-v2-bg-page`（Mission Control 选中筛选按钮的文字），页面色透明后要单独钉回实色。
- 圆角内容框 `[data-windows-content-frame]` 用 padding-box + border-box 双渐变画描边，上层半透明后整块边框渐变会透出，需改成单层背景 + `border-image`。
- body 背景有 0.2s `background-color` 过渡，暂停/恢复后立即读 computed 值会读到中间态。

## 注入时机（踩过的坑）

- `Page.addScriptToEvaluateOnNewDocument` 在 HTML 解析前执行，`document.documentElement` 为 null。**绝不能** `document.appendChild(style)`：`<style>` 会成为文档根元素，Factory 渲染失败；之后移除该节点整页变空白。必须等 `document.head` 出现再插入。
- 渲染页重载后 CDP target id 与会话不变；媒体超过内联上限时新文档脚本只含透明样式，同步线程要检测页面状态丢失并重新完整注入。

## 测试安全

- `cargo test` 曾因 `restore` 发现真实安装而结束并重启用户的 Factory（还因子进程继承管道句柄让测试命令挂住）。现在 `cfg(test)` 下安装根指向不存在的目录，`restore` 只在进程确实带调试端口时才重启。
- 端到端验证：壳 → pipe（`configure`/`apply`/`pause`/`restore`/`shutdown`）→ CDP 检查 `window.__FACTORY_BACKGROUND_STUDIO__`、`#factory-background-layer` 唯一、页面 `childNodes` 非空，并覆盖 hash 路由、`location.reload()`、worker 重启重连。
- 探查截图等临时文件用完即删。
