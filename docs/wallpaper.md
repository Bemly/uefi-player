# 动态壁纸（P4，播放器 `Clip` 的薄调用层）

> 画廊唯一入口是 `[playlist]`（`[[wallpaper.items]]` 与 `[wallpaper] loop` 已删除，
> 残留键忽略，无迁移/别名；`wallpaper_loop/len/path/frames` 即 `[playlist]` 别名）：
> 每项按路径自动检测（与播放列表同一 `detect_clip_format`，检测序见 `docs/player.md`；
> raw 走命中项所在目录扫描同胞帧）。
> - 显示哪几个：`[playlist]` 全集（`loop_single` 只取首项重播，`once` 全拼+末帧保持）；
> - 显示多少：列表长度即全集长度（B 过渡全长封顶，`preload` 仍按项截断）；
> - 循环：只留 `[playlist] loop` 一处（`loop_list` 回绕 / `loop_single` 重播当前项 /
>   `once` 末帧停留，`next_gallery_index`）。
> - `wallpaper/wall.qois`（`u32le len + QOI` 串接，与视频同格式），开机全量预解码
>   （与播片同代价，`preload` 按项截断）；
> - `wallpaper/wall.mp4`（hev/avc/vp09 轨容器探针，首匹配胜出）/
>   `wall.hevc` / `wall.ivf` / `wall.h263` / `wall.m2v` / `wall.ogv`：
>   软解预解码，坏帧记数跳过、全坏该项跳过（不断菜单；`preload` 同样截断）；
> - `wallpaper/frameNNNN.raw`：零解码直搬（老路）。
> 解码全部复用 `video::decoder::Clip`，壁纸只剩开关 + 画廊拼接 + 帧率。

## 格式与布局（`raw`）

- 画廊项指向 `frameNNNN.raw` 即命中目录扫描（`is_raw_frame_path`：末级文件名
  `frame` + 数字 + `.raw`，大小写不敏感）；同目录同胞帧同属该项，同拼进 Clip。
- 每帧 BGRA 直存（`BltPixel` 内存布局正好 BGRA，零转换；见 `bytes_as_pixels`）。
- 首版定死 1920x1080，非此分辨率的现模式跳过壁纸（log 说明，不挡流程）。
- 转制：`ffmpeg -i in.mp4 -s 1920x1080 -pix_fmt bgra -f rawvideo frame%04d.raw`

## 格式与布局（`qois`）

- 画廊项指向单个 `wallpaper/wall.qois`（`u32le len + QOI` 串接，与 `video.qois` 同格式），
  开机一次性预解码进内存（坏帧记数跳过，全坏该项跳过，不断菜单；`preload` 截断）。
- 几何取 Clip 真实 `w/h`（不再写死 1920x1080 送显，`display.fit` 生效）；
  现模式非 1080p 照旧跳过（与 `raw` 同门控，见调用方）。

## 配置（`[wallpaper]`，四键，详见 `config.md`）

`enable`（默认关，总开关）/ `fps`（默认 15，与 `[play]` 同构三态）/
`preload`（默认 8，按项封顶；单帧约 8MB，qois 预解码后同样按项截断；
0 = 不封顶）/ `popup_freeze`（默认开）。无校准记忆，纯开关。
串口画廊行（矩阵断言用）：
`wallpaper: gallery items={n} loop={mode} preloaded {m} frames (preload {p})`。

## 上屏三处（全传 Clip 真实几何，走 `display.fit`）

- 待机全屏（`idle::idle_wait`）：`upload_frame(clip.w, clip.h, frame)`，
  `present::geometry` 按 `fit` 定目标矩形（clip==屏走 1:1 快捷道）。
- 行区动画（弹窗 `popup` + 测试页 `wait_key_or_animate` 共用口径）：
  `upload_frame_region(clip.w, clip.h, top, frame)`，源窗按真实几何交窗，
  不写屏外；`fit` 同全屏语义（行区即目标矩形）。
- B 引导过渡（`boot::play_transition`）：`upload_frame(set.w, set.h, frame)`，
  全长（`wallpaper_frames` = 画廊长度）按壁纸 fps 限速播完再 `drop` 交权。
- `fit` 五档（见 `config.md`）：`stretch` 拉伸铺满（忽略 `align`）/
  `fit` 缩放最大等比居中 / `cover` 填充=裁切等比放大裁满（按 `align` 定裁切锚点）/
  `scale_down` 缩放最小只缩不放（放得下时按 `align` 摆 1:1）/
  `native` 原生 1:1 按 `align` 摆（默认居中）；留边一律 `bar` 纯色填充。
  写死 `WP_W/WP_H` 送显已删：非 1080p 画廊帧按 1080p 送会错窗/撕裂（血泪，已改四处传真实 `w/h`）。

## 预载（`preload` 按项封顶）

- `wallpaper::load` 逐项 `load_item_clip`（`eager` 全量）后截断：
  `cap > 0 && clip.len() > cap` 即 `truncate(cap)`，0 = 不截断（有多少读多少，
  长动画注意内存，单帧 1920x1080x4 ≈ 8MB）。
- 截断后空项跳过；存活项再做几何一致校验（首项定，后继异形整项跳过 + warn，
  不撕裂后续帧定位）；全拼一个 `Clip`（调用方 `frame(i)`/`len` 零改）。
- 串口 `preloaded {m}` 即截断后拼接总帧数（矩阵断言项数与封顶行为）。

## 共存规则（画廊项 × 播片项）

- 分目录存放是惯例不是强制：壁纸项常放 `wallpaper/` 下，播片项常放 `1080p/` 下；
  同一检测矩阵，互不抢片源（`wall.mp4` 与 `video.mp4` 是两个独立文件，各写各的路径）。
- B 过渡播画廊全长（`frames` 旧键已删除，列表长度治理；`preload` 仍按项截断）。
- `enable` 关 = decor 为 None，壁纸不 halt 任何流程；画廊为空
  （`[playlist]` 无项/全坏）= 静默无壁纸（菜单/引导照走，串口警告）。

## A 菜单背景

`run_test_page` 空闲时在文字区下方行区（`WP_TOP` 起）循环播：
测试页文字不重画（`draw_str` 逐像素 `VideoFill` 跟不上帧率），只画行区
（`upload_frame_region`：Blt 用 `SubRectangle` 取源行区，direct 逐行拷）。
按键轮询 5ms 一次（`poll_key`），延迟可忽略；行区 paint 失败即停背景保测试页。

## B 引导过渡

`boot::run` 选中 PE 项后、chainload 前全屏播画廊全长（`wallpaper_frames` 封顶），
走 `upload_frame` 生效路径（render 矩阵，不另起管线），播完 `drop(Screen)` 交权。

## 缺片源行为

开关开但无目录/无有效帧/几何不对：一律静默无壁纸（菜单/引导照走），
串口记 warning。壁纸是装饰，不 halt 任何流程。
