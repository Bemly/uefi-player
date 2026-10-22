# 右下角动作条像素图标（24栅格单色描线原样直用）

来源：https://github.com/halfmage/pixelarticons（`svg/` 目录），
协议 MIT（Copyright (c) 2019 Gerrit Halfmann；仓库根 `LICENSE` 全文，
本仓 vendor 需保留 upstream 版权行，见下）。

| 本仓文件 | 上游原名 | 语义 |
|---|---|---|
| `open.png` | `folder.svg` | 打开（文件夹） |
| `save.png` | `save.svg` | 保存（软盘） |
| `save_as.png` | `copy.svg` | 另存为（双文档） |
| `reset.png` | `undo.svg` | 重置（粗回退箭头；环形箭头在小尺寸下太细） |
| `theme_dark.png` | `moon.svg` | 明暗切换（浅色下显示，点按切深色） |
| `theme_light.png` | `sun.svg` | 明暗切换（深色下显示，点按切浅色；图标即目的地） |
| `lang.png` | `globe.svg` | 语言切换（地球） |

Upstream 版权（MIT 要求随附）：
`Copyright (c) 2019 Gerrit Halfmann`，完整 `LICENSE` 见上游仓库根。

轮子结论（见根 `AGENTS.md`「轮子优先」）：搜过 `crates.io` 像素图标件——
`egui` 生态无现货（`egui_phosphor` 等是矢量线条风，与 10px 像素字不搭），
故直接 vendor 这套 MIT 单色像素图标（24 栅格整数矩形，
`rsvg-convert -w 24` 原生尺寸栅格化白版，零杂色零抗锯齿）。
渲染：`NEAREST` 20pt 直显（硬边缘不模糊）+ 主题色 tint
（暗色白 / 浅色黑，跟随描边字白黑口径；白底位图 × 主题色 = 纯主色剪影）。
曾用方案已撤：tstamborski CC0 彩色 16px（风格杂）与用户提供的两枚月亮
（与描线风不统一），见 git 历史。
