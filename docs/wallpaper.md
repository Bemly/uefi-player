# 动态壁纸（P4，播放器 `Clip` 的薄调用层）

> 画廊模型（`[wallpaper]` enable/loop/fps/preload/popup_freeze +
> `[[wallpaper.items]] path`，默认 `loop_list`）：每项按路径自动检测
> （与播放列表同一 `detect_clip_format`，检测序见 `docs/player.md`；
> raw 走 `wallpaper/` 目录扫描，`format` 旧键已删除）。
> - `wallpaper/wall.qois`（`u32le len + QOI` 串接，与视频同格式），开机预解码
>   （与播片同代价，首帧无抖动但启动慢一拍，`preload` 按项截断）；
> - `wallpaper/wall.mp4`（hev/avc/vp09 轨容器探针，首匹配胜出）/
>   `wall.hevc` / `wall.ivf` / `wall.h263` / `wall.m2v` / `wall.ogv`：
>   软解预解码，坏帧记数跳过、全坏该项跳过（不断菜单；`preload` 同样截断）；
> - `wallpaper/frameNNNN.raw`：零解码直搬（老路，首帧无抖动）。
> 解码全部复用 `video::decoder::Clip`，壁纸只剩开关 + 画廊 + 帧率 + 循环语义。

## 格式与布局（`raw`）

- U 盘 `wallpaper/frameNNNN.raw`，每帧 1920x1080 BGRA 直存
  （`BltPixel` 内存布局正好 BGRA，零转换；见 `bytes_as_pixels`）。
- 首版定死 1920x1080，非此分辨率的现模式跳过壁纸（log 说明，不挡流程）。
- 转制：`ffmpeg -i in.mp4 -s 1920x1080 -pix_fmt bgra -f rawvideo frame%04d.raw`
- 画廊项指向其中一帧（如 `wallpaper/frame0000.raw`）即命中目录扫描；
  同目录多帧同属该画廊集。

## 格式与布局（`qois`）

- U 盘单个 `wallpaper/wall.qois`（`u32le len + QOI` 串接，与 `video.qois` 同格式），
  开机一次性预解码进内存（坏帧记数跳过，全坏该项跳过，不断菜单；`preload` 截断）。
- 几何取文件头一致值；现模式非 1920x1080 照样跳过（与 `raw` 同规则）。

## 配置（`[wallpaper]`，详见 `config.md`）

`enable`（默认关，总开关）/ `loop`（默认 `loop_list`，与播放列表同构：
`loop_single` 重播当前项 / `once` 播完即停）/ `fps`（默认 15）/
`preload`（默认 8，按项封顶；单帧约 8MB，qois 预解码后同样按项截断；
0 = 不封顶）/ `popup_freeze`（默认开）+ `[[wallpaper.items]]` 画廊。
无校准记忆，纯开关。串口画廊行（矩阵断言用）：
`wallpaper: gallery items={n} loop={mode} preloaded {m} frames (preload {p})`。

## 共存规则（`wall.*` × 播片项）

- 分目录存放：壁纸项常放 `wallpaper/` 下，播片项常放 `1080p/` 下；
  同一检测矩阵，互不抢片源（`wall.mp4` 与 `video.mp4` 是两个独立文件）。
- B 过渡播画廊全长（`frames` 旧键已删除，列表长度治理；`preload` 仍按项截断）。
- `enable` 关 = decor 为 None，壁纸不 halt 任何流程；画廊为空
  （无项/全坏）= 静默无壁纸（菜单/引导照走，串口警告）。

## A 菜单背景

`run_menu` 空闲时在文字区下方行区（`WP_TOP` 起）循环播：
菜单文字不重画（`draw_str` 逐像素 `VideoFill` 跟不上帧率），只画行区
（`upload_frame_region`：Blt 用 `SubRectangle` 取源行区，direct 逐行拷）。
按键轮询 5ms 一次（`poll_key`），延迟可忽略；行区 paint 失败即停背景保菜单。

## B 引导过渡

`boot::run` 选中 PE 项后、chainload 前全屏播 N 帧（画廊全长封顶），
走 `upload_frame` 生效路径（render 矩阵，不另起管线），播完 `drop(Screen)` 交权。

## 缺片源行为

开关开但无目录/无有效帧/几何不对：一律静默无壁纸（菜单/引导照走），
串口记 warning。壁纸是装饰，不 halt 任何流程。
