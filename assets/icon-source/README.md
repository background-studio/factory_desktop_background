# Factory 插件图标素材

- `background-original.png`：用户指定的 858×784 原图，未裁切、未调色。
- `background-source.png`：上下各补 37px 透明边，成为 858×858。
- `background-40.png`：原图等比缩放后使用公共圆角模板，alpha 乘 0.4；RGB 不调色。
- `factory-official.svg`：来自 https://factory.com/favicon.svg 的官方标志参考。
- `prepare-mark.mjs`：保留官方 path，去除背景，改为冰白填充与橙色细边。文件名 `mark-generated.png` 沿用合成脚本约定，**不是 AI 生图**。
- `frame.svg`：品牌橙色圆角边框。

```powershell
npm ci
node prepare-mark.mjs
npm run prepare-background
npm run compose -- ../../../background-studio
npm run verify -- ../../../background-studio
```

Factory 标识用于识别第三方插件目标，不代表官方出品或授权；素材权利属于各自权利人。
