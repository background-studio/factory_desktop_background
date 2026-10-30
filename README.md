# Factory Background Studio

给 [Factory](https://factory.ai) Windows 桌面应用（Droid，Electron 42 / Squirrel 安装）加背景图与视频的 Background Studio 插件。

纯 Rust 无界面 worker，只由 Background Studio 壳启动；通过本机回环 CDP 注入主渲染页，**不修改** `factory-desktop.exe`、`app.asar` 或任何安装文件。

- 插件 id：`factory`，协议 `pluginProtocol: 2`，Pipe `\\.\pipe\background-studio-factory`
- 目标：`%LOCALAPPDATA%\Factory\app-<版本>\factory-desktop.exe`（已在 Factory 1.0.0 上验证）
- 运行状态：`%LOCALAPPDATA%\FactoryBackgroundStudio\runtime.json`
- 协议与接管细节：[docs/plugin-protocol.md](docs/plugin-protocol.md)

## 工作方式

1. 壳 `configure` 后，worker 等待 Factory；已在运行的普通 Factory 不会被关闭，点「立即接管」（`apply`）才会重启它。
2. 接管时结束该版本目录下的 Factory 主进程（`droid.exe` 不受影响），以 `--remote-debugging-address=127.0.0.1 --remote-debugging-port=<9247 起>` 重新启动。
3. 注入 `src/main/payload.ts`：
   - Factory 配色全部来自 CSS 变量（core-v2 令牌 + 旧 `--surface-*` / `--sidebar-*`，用户「外观」自定义由 `STYLE#factory-appearance-overrides` 覆盖）。插件在声明令牌的同一批元素上以 `!important` 覆盖：页面底色透明，卡片/输入框/代码块按不透明度混合，悬停/选中额外 +0.25。
   - 原色由 body 下的探针元素 `#factory-background-probe` 实时读取，不写死任何颜色，跟随深浅主题和外观自定义。
   - 侧栏、标题栏、内容框、输入区各只着色一次，避免半透明层层叠加。
4. `pause` 完整移除注入；`restore` 移除注入，并在 Factory 以调试端口运行时以官方参数重启它（普通启动的 Factory 不会被结束）。

## 设置项

见 `plugin.json` 的 `settingsSchema`：填充方式、位置、背景不透明度、模糊、缩放、遮罩、主页/会话页强度（`#/sessions/<id>` 为会话页）、侧栏 / 画布 / 卡片 / 输入区 / 菜单 / 代码与终端不透明度、视频静音与倍速。

## 构建

```powershell
cd src-tauri
cargo fmt --all -- --check
cargo test
cargo build --release
```

产物 `src-tauri/target/release/factory-background-studio.exe`，安装到壳时改名为 `Factory Background Studio.exe`，与 `plugin.json` 放在
`<壳 dataRoot>\plugins\factory\<版本>\`。单元测试从不解析真实安装目录（`cfg(test)` 下指向一个不存在的目录），不会结束或重启正在使用的 Factory；需要真实 Factory 的测试标记为 `#[ignore]`。

## 已知限制

- 插件图标使用用户提供的半透明底图与 Factory 官方标识组合，素材与复现步骤见 [assets/icon-source](assets/icon-source/README.md)。
- 当前主页面白名单针对本机 Squirrel 安装和 `factory-app://desktop`；customer-prem 的 HTTPS 渲染入口尚未支持。
- 真实应用已验证图片应用、热更新、暂停、恢复、重载和 worker 重连。浅色主题仅做过不持久化 DOM 模拟，视频及终端专用视图尚未完成运行时验证（NOT RUNTIME-VERIFIED）。
- 下载：[GitHub Releases](https://github.com/background-studio/factory_desktop_background/releases)。壳 catalog 已登记 `background-studio/factory_desktop_background`；旧版壳若没有 Factory 条目，需要更新其插件目录。

## 发布包

`FactoryBackgroundStudio-<版本>-plugin.zip` 包含 worker、`plugin.json` 和许可证；旁边的 `.sha256` 文件用于校验下载完整性。首版变更与验证边界见 [CHANGELOG.md](CHANGELOG.md)。
