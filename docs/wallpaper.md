# 动态壁纸（P4，序列位图）

> 不用 QOI：壁纸要的是零解码直搬，压缩反而加 CPU 与首帧抖动。
> 播放器（P3）的 `.qois` 管线不动，两者格式互不干扰。

## 格式与布局

- U 盘 `wallpaper/frameNNNN.raw`，每帧 1920x1080 BGRA 直存
  （`BltPixel` 内存布局正好 BGRA，零转换；见 `bytes_as_pixels`）。
- 首版定死 1920x1080，非此分辨率的现模式跳过壁纸（log 说明，不挡流程）。
- 转制：`ffmpeg -i in.mp4 -s 1920x1080 -pix_fmt bgra -f rawvideo frame%04d.raw`

## 配置（`[wallpaper]`，详见 `config.md`）

`enable`（默认关）/ `fps`（默认 15）/ `frames`（B 封顶，0 = 全播预载集）/
`preload`（默认 8，内存兜底；单帧约 8MB）。无校准记忆，纯开关。

## A 菜单背景

`run_menu` 空闲时在文字区下方行区（`WP_TOP` 起）循环播：
菜单文字不重画（`draw_str` 逐像素 `VideoFill` 跟不上帧率），只画行区
（`upload_frame_region`：Blt 用 `SubRectangle` 取源行区，direct 逐行拷）。
按键轮询 5ms 一次（`poll_key`），延迟可忽略；行区 paint 失败即停背景保菜单。

## B 引导过渡

`boot::run` 选中 PE 项后、chainload 前全屏播 N 帧（`frames` 封顶），
走 `upload_frame` 生效路径（render 矩阵，不另起管线），播完 `drop(Screen)` 交权。

## 缺片源行为

开关开但无目录/无有效帧/几何不对：一律静默无壁纸（菜单/引导照走），
串口记 warning。壁纸是装饰，不 halt 任何流程。
