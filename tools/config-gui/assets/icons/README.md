# 右下角动作条像素图标（16x16，原色直用）

来源：https://github.com/tstamborski/pixelart-icons（`png/` 目录），
协议 CC0 1.0（公有领域，可商用、可改、无需署名；仓库根 `LICENSE` 全文）。

| 本仓文件 | 上游原名 | 语义 |
|---|---|---|
| `open.png` | `openfile16.png` | 打开（牛皮纸文件夹） |
| `save.png` | `savefile16.png` | 保存（软盘） |
| `save_as.png` | `copy16.png` | 另存为（折角文档） |
| `reset.png` | `undo16.png` | 重置（粗回退箭头；环形箭头在 16px 下太细，选了更粗的） |
| `theme.png` | `yinyang16.png` | 明暗切换（两态共用；上游无太阳/月亮，阴阳鱼表暗黑/浅色二象） |
| `lang.png` | `earth16.png` | 语言切换（地球） |

轮子结论（见根 `AGENTS.md`「轮子优先」）：搜过 `crates.io` 像素图标件——
`egui` 生态无现货（`egui_phosphor` 等是矢量线条风，与 10px 像素字不搭），
故直接 vendor 这 6 张 CC0 位图（共约 1KB，进二进制零体积压力）。
渲染：`NEAREST` 采样 20pt 直显（硬边缘，不模糊）+ 原色（不 tint，
作者的暗色描边在深浅玻璃上都可读）。
