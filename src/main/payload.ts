// 本文件是 Factory 渲染页注入脚本的唯一来源：src-tauri/build.rs 在编译期
// 抽取 BACKGROUND_CSS / REVIEW_SHADOW_CSS / 运行时模板（两空格缩进的 `return String.raw`）。
// payload.rs 会把 CSS 一起摘要进修订号，改 CSS 后热更新一定会重新注入。
import { createHash } from "node:crypto";

type MediaKind = "image" | "video";
interface DisplaySettings {
  fit: "cover" | "contain" | "fill" | "tile";
  positionX: number;
  positionY: number;
  opacity: number;
  blur: number;
  scale: number;
  overlayColor: string;
  overlayOpacity: number;
  homeIntensity: number;
  taskIntensity: number;
  sidebarOpacity: number;
  surfaceOpacity: number;
  cardOpacity: number;
  composerOpacity: number;
  menuOpacity: number;
  terminalOpacity: number;
  enabledOnHome: boolean;
  enabledOnTasks: boolean;
  videoMuted: boolean;
  videoPlaybackRate: number;
}

/*
 * Factory 1.0 的配色全部来自 CSS 变量：
 *   - core-v2 令牌声明在 [data-core-ui-v2-theme] / [data-core-ui-v2-portals] 上，
 *     用户“外观”自定义颜色再由 STYLE#factory-appearance-overrides 以双属性选择器覆盖；
 *   - 旧令牌（--surface-*、--sidebar-*）声明在 :root[data-theme] 上，
 *     并在 v2 容器（data-session-detail-v2 等）上被重映射到 core-v2 令牌；
 *   - #root、split-view、会话区等十几层嵌套都在刷同一个页面底色。
 * 逐元素清背景既脆弱又会漏，所以在“声明令牌的同一批元素”上以 !important 覆盖令牌：
 *   - 结构层（页面底色）一律透明，雾只在侧栏 / 标题栏 / 内容框 / 输入区各着色一次，
 *     避免多层半透明叠加越叠越黑；
 *   - 内容层（卡片、表格、输入框、代码块）= 原色 × 卡片不透明度；
 *   - 悬停 / 选中 = 原色 × min(1, 卡片不透明度 + 0.25)，保证高亮仍可辨认；
 *   - body 下的弹层（Radix portal）内容令牌改用菜单不透明度。
 * 原色由 body 下的探针元素实时读取：探针不在覆盖范围内、body 与 html 也不被覆盖，
 * 所以读到的永远是 Factory 当前主题（含外观自定义）的原始实色。
 */
const BACKGROUND_CSS = String.raw`
html.factory-background-active,
html.factory-background-active body,
html.factory-background-active #root {
  background: transparent !important;
  background-color: transparent !important;
}

html.factory-background-active #root,
html.factory-background-active :is(
  [data-theme],
  [data-core-ui-v2-theme],
  [data-core-ui-v2-portals],
  [data-new-session-composer-v2],
  [data-session-detail-v2],
  [data-mission-control-v2],
  [data-mission-sidebar-v2],
  [data-sw-factory-v2],
  [data-command-palette-v2],
  [data-onboarding-v2],
  [data-windows-content-surface]
):not(html):not(body):not([data-fbg-probe]) {
  --core-v2-bg-page: transparent !important;
  --surface-1: transparent !important;
  --sidebar-bg: transparent !important;
  --initial-loader-background: transparent !important;
  --core-v2-bg-surface: color-mix(in srgb, var(--fbg-solid-surface) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-bg-surface-inset: color-mix(in srgb, var(--fbg-solid-surface-inset) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-bg-surface-raised: color-mix(in srgb, var(--fbg-solid-surface-raised) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-button-secondary-bg-default: color-mix(in srgb, var(--fbg-solid-button) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-input-bg: color-mix(in srgb, var(--fbg-solid-input) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-code-bg: color-mix(in srgb, var(--fbg-solid-code) calc(var(--fbg-terminal-opacity) * 100%), transparent) !important;
  --surface-2: color-mix(in srgb, var(--fbg-solid-surface-2) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --core-v2-bg-hover: color-mix(in srgb, var(--fbg-solid-hover) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --core-v2-bg-selected: color-mix(in srgb, var(--fbg-solid-selected) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --core-v2-button-secondary-bg-hover: color-mix(in srgb, var(--fbg-solid-button-hover) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --core-v2-button-secondary-bg-pressed: color-mix(in srgb, var(--fbg-solid-button-pressed) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --surface-3: color-mix(in srgb, var(--fbg-solid-surface-3) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --sidebar-hover: color-mix(in srgb, var(--fbg-solid-hover) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --sidebar-active: color-mix(in srgb, var(--fbg-solid-selected) calc(var(--fbg-highlight-opacity) * 100%), transparent) !important;
  --core-v2-bg-overlay: color-mix(in srgb, var(--fbg-solid-overlay) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
  --core-v2-appearance-user-message-bubble: color-mix(in srgb, var(--fbg-solid-bubble) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
}

/* v2 容器把反色文字定义为 var(--core-v2-bg-page)（如 Mission Control 选中标签），页面色透明后文字会消失。 */
html.factory-background-active :is(
  [data-new-session-composer-v2],
  [data-session-detail-v2],
  [data-mission-control-v2],
  [data-mission-sidebar-v2],
  [data-sw-factory-v2],
  [data-command-palette-v2],
  [data-onboarding-v2],
  [data-windows-title-bar-v2]
) {
  --text-inverted: var(--fbg-solid-page) !important;
}
html.factory-background-active body > :not(#root):not(#factory-background-layer):not([data-fbg-probe]) {
  --text-inverted: var(--fbg-solid-page) !important;
}

/* 统计卡片的图表令牌取自 --surface-1：卡片按卡片不透明度、悬浮提示按菜单不透明度。 */
html.factory-background-active [data-card][data-sf-metric="card"] {
  --sf-chart-card-bg: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-card-opacity) * 100%), transparent) !important;
  --sf-chart-tooltip-bg: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
}

/*
 * body 下的弹层（对话框 / 菜单 / 命令面板）：页面与内容令牌都跟随菜单不透明度，
 * 弹层浮在内容之上，完全透明会与下方文字重叠。
 */
html.factory-background-active body > :not(#root):not(#factory-background-layer):not([data-fbg-probe]),
html.factory-background-active body > :not(#root):not(#factory-background-layer):not([data-fbg-probe]) :is(
  [data-core-ui-v2-theme],
  [data-command-palette-v2],
  [data-onboarding-v2]
) {
  --core-v2-bg-page: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
  --surface-1: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
  --core-v2-bg-surface: color-mix(in srgb, var(--fbg-solid-surface) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
  --core-v2-bg-surface-inset: color-mix(in srgb, var(--fbg-solid-surface-inset) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
  --core-v2-bg-surface-raised: color-mix(in srgb, var(--fbg-solid-surface-raised) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
}

/* 媒体层：固定铺满窗口，z-index:-1 落在根层叠上下文最底，不改动 #root 的定位。 */
#factory-background-layer {
  position: fixed;
  inset: 0;
  z-index: -1;
  overflow: hidden;
  pointer-events: none;
  opacity: calc(var(--fbg-opacity) * var(--fbg-route-intensity));
  background-color: transparent;
  transition: opacity 220ms ease;
}

#factory-background-media,
#factory-background-tile {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  transform: scale(var(--fbg-scale));
  filter: blur(var(--fbg-blur));
  transform-origin: center center;
}

#factory-background-media {
  display: block;
  object-fit: var(--fbg-fit);
  object-position: var(--fbg-position-x) var(--fbg-position-y);
}

#factory-background-tile {
  display: none;
  background-image: var(--fbg-media-url);
  background-repeat: repeat;
  background-position: var(--fbg-position-x) var(--fbg-position-y);
  background-size: auto;
}

html.factory-background-fit-tile #factory-background-media { display: none; }
html.factory-background-fit-tile #factory-background-tile { display: block; }

#factory-background-overlay {
  position: absolute;
  inset: 0;
  background: var(--fbg-overlay-color);
  opacity: var(--fbg-overlay-opacity);
}

html.factory-background-home { --fbg-route-intensity: var(--fbg-home-intensity); }
html.factory-background-task { --fbg-route-intensity: var(--fbg-task-intensity); }
html.factory-background-home.factory-background-home-disabled,
html.factory-background-task.factory-background-task-disabled { --fbg-route-intensity: 0; }

#factory-background-probe { display: none !important; }

/*
 * 左侧栏：内部所有底色令牌清空，只在 [data-windows-sidebar] 这一层着色一次。
 * 设置页会在侧栏里再叠一层 data-sidebar-overlay（其下原侧栏为 visibility:hidden），
 * 各层各自着色会叠成多层。
 */
html.factory-background-active [data-windows-sidebar] :is([data-core-ui-v2-theme], [data-sidebar-overlay], [data-testid="session-manager"]) {
  --core-v2-bg-surface: transparent !important;
  --core-v2-bg-surface-raised: transparent !important;
}
html.factory-background-active [data-windows-sidebar] {
  background: color-mix(in srgb, var(--fbg-solid-sidebar) calc(var(--fbg-sidebar-opacity) * 100%), transparent) !important;
}

/* 会话列表上下渐隐条（含 *-fade-color 子层）用侧栏实色渐变 + blur，透明后会变成色带。 */
html.factory-background-active [data-testid^="session-list-top-fade"],
html.factory-background-active [data-testid^="session-list-bottom-fade"] {
  background: none !important;
  backdrop-filter: none !important;
}

/* 置顶的用户消息浮在滚动内容之上，按菜单不透明度着色，避免与下方文字重叠难读。 */
html.factory-background-active [data-pinned-user-message-overlay] {
  --core-v2-appearance-user-message-bubble: color-mix(in srgb, var(--fbg-solid-bubble) calc(var(--fbg-menu-opacity) * 100%), transparent) !important;
}

/* 代码块：外壳已按卡片着色，内容区再刷一次会叠成双层。 */
html.factory-background-active [data-testid="code-block"] [data-testid="code-block-content"] {
  background-color: transparent !important;
}

/* 顶部标题栏（含导航占位）：内部清空，整条只着色一次。 */
html.factory-background-active [data-windows-title-bar-v2] [data-window-navigation-placeholder] {
  background: transparent !important;
}
html.factory-background-active [data-windows-title-bar-v2] {
  background: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-surface-opacity) * 100%), transparent) !important;
}

/*
 * 主画布：圆角内容框原本用 var(--windows-content-bg) 刷 padding-box、再用边框渐变刷整个 border-box，
 * 靠上层不透明把边框渐变“挤”成 1px 描边。上层变半透明后整块边框渐变会透出来变成灰幕，
 * 所以背景只保留 padding-box 一层雾，描边改用同一渐变的 border-image（仅上、左各 1px）。
 */
html.factory-background-active [data-windows-content-surface] {
  --windows-content-bg: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-surface-opacity) * 100%), transparent) !important;
  background: transparent !important;
}
html.factory-background-active [data-windows-content-frame="true"] {
  background: linear-gradient(var(--windows-content-bg) 0 0) padding-box !important;
  border-image: linear-gradient(
    to right,
    var(--sidebar-border, color-mix(in srgb, var(--border-1) 50%, var(--fbg-solid-sidebar))),
    var(--border-1) 40%
  ) 1 !important;
}

/* 输入区：外层壳着色一次，内部继承透明页面令牌。 */
html.factory-background-active [data-testid="chat-composer-wrapper"] {
  background-color: color-mix(in srgb, var(--fbg-solid-page) calc(var(--fbg-composer-opacity) * 100%), transparent) !important;
}

@media (prefers-reduced-motion: reduce) {
  #factory-background-layer { transition: none; }
}
`;

const REVIEW_SHADOW_STYLE_ID = "factory-background-review-shadow-style";
const REVIEW_SHADOW_CSS = String.raw`
/* Factory 1.0 主界面不使用 Shadow DOM；保留占位以参与修订哈希。 */
:host { background-color: transparent !important; }
`;

export interface PayloadInput {
  mediaUrl: string;
  mediaKind: MediaKind;
  display: DisplaySettings;
  revision: string;
}

export function buildRendererPayload(input: PayloadInput) {
  const revision = createHash("sha256")
    .update(input.revision)
    .update(BACKGROUND_CSS)
    .update(REVIEW_SHADOW_CSS)
    .digest("hex");
  const serialized = JSON.stringify({ ...input, revision }).replace(/</g, "\\u003c");
  const css = JSON.stringify(BACKGROUND_CSS);
  const reviewShadowCss = JSON.stringify(REVIEW_SHADOW_CSS);
  const reviewShadowStyleId = JSON.stringify(REVIEW_SHADOW_STYLE_ID);
  return String.raw`((config, cssText, reviewShadowCssText, reviewShadowStyleId) => {
    const STATE = "__FACTORY_BACKGROUND_STUDIO__";
    const STYLE_ID = "factory-background-style";
    const EARLY_STYLE_ID = "factory-background-early-transparency";
    const LAYER_ID = "factory-background-layer";
    const PROBE_ID = "factory-background-probe";
    const ROOT_CLASSES = [
      "factory-background-active", "factory-background-home", "factory-background-task",
      "factory-background-home-disabled", "factory-background-task-disabled",
      "factory-background-fit-tile"
    ];
    const SOLID_TOKENS = {
      "--fbg-solid-page": "--core-v2-bg-page",
      "--fbg-solid-surface": "--core-v2-bg-surface",
      "--fbg-solid-surface-inset": "--core-v2-bg-surface-inset",
      "--fbg-solid-surface-raised": "--core-v2-bg-surface-raised",
      "--fbg-solid-overlay": "--core-v2-bg-overlay",
      "--fbg-solid-hover": "--core-v2-bg-hover",
      "--fbg-solid-selected": "--core-v2-bg-selected",
      "--fbg-solid-button": "--core-v2-button-secondary-bg-default",
      "--fbg-solid-button-hover": "--core-v2-button-secondary-bg-hover",
      "--fbg-solid-button-pressed": "--core-v2-button-secondary-bg-pressed",
      "--fbg-solid-input": "--core-v2-input-bg",
      "--fbg-solid-code": "--core-v2-code-bg",
      "--fbg-solid-surface-2": "--surface-2",
      "--fbg-solid-surface-3": "--surface-3",
      "--fbg-solid-sidebar": "--sidebar-bg"
    };
    const ROOT_PROPERTIES = [
      "--fbg-opacity", "--fbg-blur", "--fbg-scale", "--fbg-fit",
      "--fbg-position-x", "--fbg-position-y", "--fbg-overlay-color",
      "--fbg-overlay-opacity", "--fbg-home-intensity", "--fbg-task-intensity",
      "--fbg-sidebar-opacity", "--fbg-surface-opacity", "--fbg-card-opacity",
      "--fbg-highlight-opacity", "--fbg-composer-opacity", "--fbg-menu-opacity",
      "--fbg-terminal-opacity", "--fbg-media-url",
      "--fbg-solid-bubble", ...Object.keys(SOLID_TOKENS)
    ];

    const previous = window[STATE];
    if (previous?.cleanup) previous.cleanup();
    void reviewShadowCssText;
    void reviewShadowStyleId;
    let scheduled = 0;

    const mediaUrl = (() => {
      const source = String(config.mediaUrl || "");
      const comma = source.indexOf(",");
      if (!source.startsWith("data:") || comma < 0) return source;
      const binary = atob(source.slice(comma + 1));
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
      const mime = /^data:([^;,]+)/.exec(source)?.[1] || "application/octet-stream";
      return URL.createObjectURL(new Blob([bytes], { type: mime }));
    })();
    if (!mediaUrl) throw new Error("背景媒体尚未就绪");

    const cleanup = () => {
      const state = window[STATE];
      state?.observer?.disconnect();
      if (state?.timer) clearInterval(state.timer);
      if (state?.onHash) window.removeEventListener("hashchange", state.onHash);
      if (state?.onScheme) state.scheme?.removeEventListener?.("change", state.onScheme);
      if (scheduled) cancelAnimationFrame(scheduled);
      scheduled = 0;
      for (const id of [LAYER_ID, STYLE_ID, EARLY_STYLE_ID, PROBE_ID]) document.getElementById(id)?.remove();
      document.documentElement?.classList.remove(...ROOT_CLASSES);
      for (const property of ROOT_PROPERTIES) document.documentElement?.style.removeProperty(property);
      if (state?.mediaUrl?.startsWith("blob:")) URL.revokeObjectURL(state.mediaUrl);
      delete window[STATE];
      return true;
    };

    const currentTheme = () => {
      const value = (document.documentElement?.getAttribute("data-theme") || "").toLowerCase();
      if (value.includes("dark")) return "dark";
      if (value.includes("light")) return "light";
      const portals = (document.body?.getAttribute("data-core-ui-v2-portals") || "").toLowerCase();
      if (portals.includes("dark")) return "dark";
      if (portals.includes("light")) return "light";
      try {
        return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
      } catch {}
      return "dark";
    };

    // 探针与 Radix portal 同级挂在 body 下、自带主题属性且被覆盖规则排除，
    // 读到的是当前主题（含外观自定义）的原始实色。
    const readSolidColors = () => {
      const body = document.body;
      if (!body) return null;
      let probe = document.getElementById(PROBE_ID);
      if (!probe) {
        probe = document.createElement("div");
        probe.id = PROBE_ID;
        probe.setAttribute("data-fbg-probe", "");
        probe.setAttribute("aria-hidden", "true");
        body.appendChild(probe);
      }
      const theme = currentTheme();
      if (probe.getAttribute("data-core-ui-v2-theme") !== theme) {
        probe.setAttribute("data-core-ui-v2-theme", theme);
      }
      const computed = getComputedStyle(probe);
      const fallback = computed.getPropertyValue("--core-v2-bg-page").trim() ||
        (theme === "dark" ? "#121212" : "#ffffff");
      const colors = {};
      for (const [target, source] of Object.entries(SOLID_TOKENS)) {
        colors[target] = computed.getPropertyValue(source).trim() || fallback;
      }
      // 用户消息气泡：优先外观自定义色，否则与 Factory 一致（深色 --tungsten-17、浅色 --surface-2）。
      colors["--fbg-solid-bubble"] =
        computed.getPropertyValue("--core-v2-appearance-user-message-bubble").trim() ||
        (theme === "dark" ? computed.getPropertyValue("--tungsten-17").trim() : "") ||
        colors["--fbg-solid-surface-2"];
      return colors;
    };

    const install = () => {
      const root = document.documentElement;
      if (!root) return false;
      const setClass = (name, on) => {
        if (root.classList.contains(name) !== on) root.classList.toggle(name, on);
      };
      const setProp = (name, value) => {
        if (root.style.getPropertyValue(name) !== value) root.style.setProperty(name, value);
      };

      const colors = readSolidColors();
      if (colors) for (const [name, value] of Object.entries(colors)) setProp(name, value);

      let style = document.getElementById(STYLE_ID);
      if (!style) {
        style = document.createElement("style");
        style.id = STYLE_ID;
        (document.head || root).appendChild(style);
      }
      if (style.dataset.fbgRevision !== config.revision) {
        style.textContent = cssText;
        style.dataset.fbgRevision = config.revision;
      }
      document.getElementById(EARLY_STYLE_ID)?.remove();

      let layer = document.getElementById(LAYER_ID);
      if (!layer && document.body) {
        layer = document.createElement("div");
        layer.id = LAYER_ID;
        layer.setAttribute("aria-hidden", "true");
        const media = document.createElement(config.mediaKind === "video" ? "video" : "img");
        media.id = "factory-background-media";
        media.setAttribute("aria-hidden", "true");
        if (config.mediaKind === "video") {
          media.autoplay = true;
          media.loop = true;
          media.muted = Boolean(config.display.videoMuted);
          media.defaultMuted = Boolean(config.display.videoMuted);
          media.playsInline = true;
        } else {
          media.alt = "";
          media.decoding = "async";
        }
        media.src = mediaUrl;
        const tile = document.createElement("div");
        tile.id = "factory-background-tile";
        const overlay = document.createElement("div");
        overlay.id = "factory-background-overlay";
        layer.append(media, tile, overlay);
        document.body.prepend(layer);
        if (config.mediaKind === "video") {
          media.playbackRate = Number(config.display.videoPlaybackRate) || 1;
          media.play().catch(() => undefined);
        }
      } else if (layer && document.body && layer.parentNode !== document.body) {
        document.body.prepend(layer);
      }

      const display = config.display;
      setClass("factory-background-active", true);
      setClass("factory-background-fit-tile", display.fit === "tile" && config.mediaKind === "image");
      setClass("factory-background-home-disabled", !display.enabledOnHome);
      setClass("factory-background-task-disabled", !display.enabledOnTasks);
      setProp("--fbg-opacity", String(display.opacity));
      setProp("--fbg-blur", display.blur + "px");
      setProp("--fbg-scale", String(display.scale));
      setProp("--fbg-fit", display.fit === "tile" ? "cover" : display.fit);
      setProp("--fbg-position-x", display.positionX + "%");
      setProp("--fbg-position-y", display.positionY + "%");
      setProp("--fbg-overlay-color", display.overlayColor);
      setProp("--fbg-overlay-opacity", String(display.overlayOpacity));
      setProp("--fbg-home-intensity", String(display.homeIntensity));
      setProp("--fbg-task-intensity", String(display.taskIntensity));
      setProp("--fbg-sidebar-opacity", String(display.sidebarOpacity));
      setProp("--fbg-surface-opacity", String(display.surfaceOpacity));
      setProp("--fbg-card-opacity", String(display.cardOpacity));
      setProp("--fbg-highlight-opacity", String(Math.min(1, display.cardOpacity + 0.25)));
      setProp("--fbg-composer-opacity", String(display.composerOpacity));
      setProp("--fbg-menu-opacity", String(display.menuOpacity));
      setProp("--fbg-terminal-opacity", String(display.terminalOpacity));
      setProp("--fbg-media-url", 'url("' + mediaUrl.replace(/["\\\n\r]/g, "") + '")');

      // 会话详情（#/sessions/<id>）走“会话页”强度，其余页面走“主页”强度。
      const task = /^#\/sessions\/[^/?#]+/.test(location.hash || "");
      setClass("factory-background-home", !task);
      setClass("factory-background-task", task);
      return true;
    };

    const scheduleInstall = () => {
      if (scheduled) return;
      scheduled = requestAnimationFrame(() => { scheduled = 0; install(); });
    };
    const observer = new MutationObserver(scheduleInstall);
    observer.observe(document.documentElement, {
      childList: true,
      attributes: true,
      attributeFilter: ["data-theme", "class", "style"],
    });
    if (document.head) observer.observe(document.head, { childList: true, characterData: true, subtree: true });
    if (document.body) observer.observe(document.body, {
      childList: true,
      attributes: true,
      attributeFilter: ["data-core-ui-v2-portals"],
    });
    const onHash = () => scheduleInstall();
    window.addEventListener("hashchange", onHash);
    let scheme = null;
    try { scheme = window.matchMedia("(prefers-color-scheme: dark)"); } catch {}
    const onScheme = () => scheduleInstall();
    scheme?.addEventListener?.("change", onScheme);
    const timer = setInterval(install, 4000);
    window[STATE] = { revision: config.revision, cleanup, observer, timer, onHash, scheme, onScheme, mediaUrl };
    install();
    return { installed: true, revision: config.revision, mediaKind: config.mediaKind };
  })(${serialized}, ${css}, ${reviewShadowCss}, ${reviewShadowStyleId})`;
}
