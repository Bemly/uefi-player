# 动态壁纸（P4，播放器 `Clip` 的薄调用层）

> 片源格式矩阵（`[wallpaper] format`，默认 `raw`，与 `[play]` 同一 `MediaFormat`
> 矩阵，八格式全通，`wall.*` 同名文件）：
> - `raw`：零解码直搬（老路，首帧无抖动）；
> - `qois`：`wallpaper/wall.qois`（与视频同格式），开机预解码
>   （与播片同代价，首帧无抖动但启动慢一拍，`preload` 同样封顶）；
> - `hevc`（`wall.mp4` hev 轨优先，无则 `wall.hevc`）/ `avc`（`wall.mp4` avc 轨）
>   / `vp9`（`wall.mp4` vp09 轨优先，无则 `wall.ivf`）/ `h263`（`wall.h263`）
>   / `mpeg2v`（`wall.m2v`）/ `theora`（`wall.ogv`）：软解预解码，
>   坏帧记数跳过、全坏静默无壁纸（不断菜单；`preload` 同样截断）。
> 解码全部复用 `video::decoder::Clip`，壁纸只剩开关 + 帧率 + 循环语义。

## 格式与布局（`raw`）

- U 盘 `wallpaper/frameNNNN.raw`，每帧 1920x1080 BGRA 直存
  （`BltPixel` 内存布局正好 BGRA，零转换；见 `bytes_as_pixels`）。
- 首版定死 1920x1080，非此分辨率的现模式跳过壁纸（log 说明，不挡流程）。
- 转制：`ffmpeg -i in.mp4 -s 1920x1080 -pix_fmt bgra -f rawvideo frame%04d.raw`

## 格式与布局（`qois`）

- U 盘单个 `wallpaper/wall.qois`（`u32le len + QOI` 串接，与 `video.qois` 同格式），
  开机一次性预解码进内存（坏帧记数跳过，全坏报错，不断菜单；`preload` 截断）。
- 几何取文件头一致值；现模式非 1920x1080 照样跳过（与 `raw` 同规则）。

## 配置（`[wallpaper]`，详见 `config.md`）

`enable`（默认关）/ `format`（默认 `raw`）/ `fps`（默认 15）/
`frames`（B 封顶，0 = 全播预载集）/ `preload`（默认 8，内存兜底；
单帧约 8MB，qois 预解码后同样按此截断）。无校准记忆，纯开关。

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
