# 播放器（P3）

## 片源格式矩阵（`[play] format`，`Clip` 统一解码）

| `format` | 片源 | 接线状态 |
|---|---|---|
| `qois`（默认） | `1080p/video.qois`（`u32le 帧长 + QOI 帧` 串接）→ 开机全量预解码 | 已接（三条播放路零分支） |
| `raw` | `1080p/frameNNNN.raw`（1920x1080 BGRA 直存，与壁纸同格式）→ 零解码直读 | 已接（同下游切分） |
| `hevc` | `1080p/video.mp4`（hev 轨，石雨堂拆包 → Annex-B；无容器回 `video.hevc` Annex-B）→ vendor `rusty_h265` 软解 → YUV420 转 BGRA | 已接（预解码进 `Clip`；坏帧计数跳过，全坏报错；只收 8-bit） |
| `avc` | `1080p/video.mp4`（avc 轨，石雨堂拆 avcC → Annex-B）→ vendor `rusty_h264_decoder` 软解 → YUV420 转 BGRA | 已接（tight 8-bit 4:2:0；MP4-only，无裸流路） |
| `vp9` | `1080p/video.mp4`（vp09 轨，样片即裸帧；无容器回 `video.ivf`）→ vendor `vp9dec` 软解 → YUV420 转 BGRA | 已接（只要显示帧，隐藏 altref 跳过；只收 8-bit 4:2:0） |
| `h263` | `1080p/video.h263`（裸 H.263 基线流，无容器）→ vendor `h263-rs`（标准空选项）软解 → YUV420 转 BGRA | 已接（错位/坏包截断，首帧错即报错；基线 `PictType` 上游反转已修正，见 vendor `VENDOR.md`） |
| `mpeg2v` | `1080p/video.m2v`（裸基本流，接入单元切分）→ vendor `filmcraft-mpeg2v` 软解 → YUV420 转 BGRA | 已接（只收 4:2:0；422/444 记数跳过；AU 失败计数，`flush` 收尾帧） |
| `theora` | `1080p/video.ogv`（OggS 磁盘原格式，手写拆包 + CRC32 校验）→ vendor `oxideav-theora` 软解 → 显示区裁剪 + 翻转 + YUV420 转 BGRA | 已接（只收 4:2:0；ident/comment/setup 三头缺一即报错） |

- 八格式全落到同一 `Clip`（`video/decoder.rs` + `video/demux.rs`），三条播放路零分支；
  容器优先裸流：avc/hevc/vp9 有 `video.mp4` 即拆轨（无匹配轨直接报错，不静默换源）；
  hevc/vp9 缺容器回裸流老路（老矩阵不断）；h263/mpeg2v/theora 只读裸流。
- MP4 拆包（石雨堂 `Mp4FileDemuxer`，avcC/hvcC/vpcC，长度前缀→Annex-B 起始码；
  vp09 样片即裸帧直送）与 OggS 拆包（手写：`OggS` 页头 + 段表组包 + MSB-first
  CRC32，错页记数跳过）见 `video/demux.rs` 头注释轮子结论。
- 壁纸同矩阵（`wall.*` 同名文件，见 `docs/wallpaper.md`）。

## `video.qois` 格式

`1080p/video.qois`：`u32le 帧长 + QOI 帧` 串接，无帧率元数据。
生产假设 120Hz 视频（`VIDEO_NATIVE_FPS = 120`，`auto` 上限按它封顶）。

## 管线（`VideoMemoryRaw`）

全量预解码 → RGBA→BGRA → 按生效路径上屏：

- `RenderMode::Blt` → `blt_loop`（单线程 GOP Blt；已是单引擎 push，缓冲无意义）。
- `direct + bsp_only` → `direct_loop`（BSP 单核直写）。
- `direct + all` → `mp_draw`（`copy_block` 位宽选路，OSD 镂空）。
   分工由 `[schedule] dist` 定：`static` 按核固定行区（同构核最优）；
   `tile` 按 64 行切块、BSP+AP 从共享原子游标领活（异构核/负载不均时赢，
   OSD 行划出 tile，AP 不再盖字——旧 static 路 AP 会盖掉 OSD 字，属已知瑕疵，
   保留作扩展比对照）；
   `dynamic` 按实测核速加权分行（校准时 probe 定权计时；播放时全帧源 +
   BSP 每轮按各核上一轮耗时 EWMA 重算边界并发布，快核多拿，无 tile 游标开销，
   OSD 行恒划出）。
- 后缓冲（`[render.tune] buffers`，0-16）：`>0` 时 direct 路先画影子再整推前台
  （多核散写的撕裂窗口压成一次 push；OSD 先画进影子再随推上屏）。
  单/多核路共用同一语义（出屏基址切换 + BSP 推 + `push_gen` 同步，N==1 也无竞态）；
  Blt 单发路不用（已是单 push）。显式旋钮（不进矩阵 bench，改它清记忆重测）；
  分配失败自动退回直写（只记 warning，不断播）。
- 混合整推（`buffers>0` 的多核路）：AP/BSP 只做 CPU 内存写（画影子，行区分工同
  `dist`），BSP 每轮一次 `BufferToVideo` 整推（AP 永不调 Blt：GOP 独占 +
  固件 Blt 非线程安全）。引擎失败退回 CPU 整推（同字节，只记一次 warning）。
  四个 `blt_op` 里只有 `buffer_to_video` 能上屏；其余三项矩阵里只单发出数
  （引擎能力对照，不参选播视频）。

## 格式校验 + 掉帧（P3 闭环项）

- 预解码：坏帧计数跳过；空集直接报错（曾无限 rewind 打空转）；
  几何不一致/超大帧拒绝（混入异形帧会撕裂后续定位）。
- `mp_draw` 同：坏帧计数 + 空集报错（曾 AP 读空指针 OOB + 无限循环）。
- `VideoMemory::next_frame` 越界守卫（尾部不足直接结束，曾直切 panic）。
- 掉帧：单线程 `pace_or_drop`（迟超一整帧丢帧追赶，最多连丢 2 帧，串口记数）；
  多核确定性掉帧（BSP 判迟独写发布、全核执行同一结论，索引永不发散；
  OSD 追加 `Drop:` 计数）。
- 首启实测 `measure_max_fps`：单线程 `upload_frame` 整轮推送，是保守下限
  （workers=all 真实更快）；`auto` 取 `min(视频帧率, 实测*3/4)` 并回写。

## 帧率开关（`[play]`）

`max` 不限速；`capped(1-240)` 直接用；`auto` 无实测现测并回写
（`measured_max_fps`，GUI 只读 + 重测清零）。`vsync` 开（static/half/auto 探测开）
再叠生效预算帽（`effective_vsync_fps()`，static 取滑杆值，half 取实测取半）。

## 统一控制平面（`[play] feed` × `[decode.<fmt>]`，无老管线）

播放只走 `Clip`：拿帧方式由 `[play] feed` 五档切，单格式解码并发/缓冲由
`[decode.<fmt>]` 四键调，上传路（`blt`/`direct`/`mp_draw`/`nano_pump`/
`dynamic_draw`）零改。`[play] pipeline` 与 `[render] workers/dist` 旧键已删除
（正典只剩 `[schedule] workers/dist`；旧文件未知键按 serde 默认忽略，
旧值别名 `avx512`/`on`/整数壁纸 fps 已摘除，命中即整文件回默认 + 警告）。

## 供片模式（`[play] feed`，只切拿帧方式，不管上传路）

| `feed` | 拿帧方式 | 适用 |
|---|---|---|
| `clip`（默认） | 开机全量预解码（现行行为，零行为变化） | 全部 |
| `staged` | qois 显式三段逐帧解码；其余七格式软解码 planar→BGRA 固有分段，结构等价 clip | qois（其余结构等价） |
| `stream` | `tune.window`（1-32，默认 4）窗口边播边解，游标之后淘汰（`rewind` 重建解码器状态重播） | qois + hevc 代表格式 |
| `zero_copy` | 单复用帧缓冲逐显示帧即时解码（调用方须立即上传） | qois |
| `single`（NEW） | 每帧重读文件 + 重解码，零缓存下限对照（旧单帧循环的 Clip 化） | qois + hevc 代表格式 |

- 只切 `Clip` 内部拿帧方式：`blt`/`direct`/`mp_draw`/`nano_pump`/`dynamic_draw`
  上传路零改，只消费 `next_frame`/`rewind`/`len`/`frame`。
- 多核（`mp_draw`）/动态调度/壁纸恒 `clip`/`eager`（壁纸随机访问循环，按 tag 裁决，
  见固件 `lazy::feed_for`；`mp_draw`/`dynamic_draw` 启动即 `ensure_eager` 全量，
  不变）；旧文件缺键按 `clip` 照读。
- 矩阵覆盖：`video-feed-*` 7 case（`video: feed=` + path 行 + soak 保活；
  clip/staged/stream/zero_copy/single × qois + stream/single × hevc 代表格式，
  全组合不铺，避 5×8 膨胀）。
- GUI：播放页供片滑杆 0-4 拨档（`[play] feed = "single"` 为第 5 档）+ 一句话说明；
  `[render.tune] window` 缺键按 4、钳 1..=32（GUI 只保 roundtrip，不摆旋钮）。
- 跳帧同样经懒解码（`pace_or_drop` 调 `next_frame` 跳过）：正确但把被跳帧也解了，
  上传路零改的代价，feed 非 clip 时高掉帧场景多耗解码。
- `measure_max_fps` 照常（边测边解：懒 Clip 下首启实测不一次全解，按播出帧计数，
  全量下与旧值一致）。
- 已知退化（待 Agent-B `split_annexb_aus` 真 AU 组联调结论）：stub 期整流单 AU，
  avc/hevc 懒模式首 `next_frame` 即推整流（hevc 经输出队列仍逐帧出，avc 只留末帧），
  多 slice/B 帧重排语义以联调结论为准；冒烟只用 qois 断言帧数一致。

## 解码表（`[decode.<fmt>]`，每格式独立四键）

| 键 | 取值 | 语义 |
|---|---|---|
| `threads` | `"bsp_only"`（默认）\| `"all"` | 帧级并行：`all` 在无状态格式（qois/raw）即时生效；有状态软解码（hevc/avc/vp9/h263/mpeg2v/theora 单核顺序状态机）解析但警告并驻 `bsp_only`，待 per-codec 安全流水线落地 |
| `zero_copy` | `false`（默认）\| `true` | 单缓冲即时解码开关（全格式可行；GUI 单选开关） |
| `streaming` | `false`（默认）\| `true` | 边播边解开关（全格式可行；GUI 单选开关） |
| `buffers` | `0`（默认，跟随 `tune.window`）\| `1`-`32` 显式 | 解码帧缓存数（GUI 滑杆下标 0=跟随，1-32 显式；`tune.window` 仍是全局默认） |

- 六表：`[decode.hevc]` / `[decode.avc]` / `[decode.vp9]` / `[decode.h263]` /
  `[decode.mpeg2v]` / `[decode.theora]`（qois/raw 无表， trivially 全 yes）。
- 串口契约（矩阵断言用，不断言画质）：`decode: fmt=<Fmt>` 解析行 +
  `decode: <Fmt> threads all deferred (staying bsp_only)` 回退警告行（仅 threads=all
  落有状态格式时）；`video: fmt=` + `play: path=` + soak 保活同旧口径。
- Per-codec 可行性（全单核现状，`lazy.rs Source` 状态机顺序）：
  qois 无状态逐帧 → 帧级并行可行，zero_copy/streaming/多缓冲 yes；raw 零解码 → 全 yes；
  hevc 有状态 Annex-B + 输出队列/重排 → 解码驻顺序（stage 重叠 demux→decode→convert 待排），
  zero_copy/streaming/buffers(window) yes；avc 解码序 vs 显示序 + B 帧/多 slice 待联调 →
  仅顺序，其余 yes；vp9 有状态 + 隐藏 altref 跳过 → 仅顺序，其余 yes；
  h263 流式 reader 有状态、总量未知 → 仅顺序，其余 yes；mpeg2v AU 切分有状态 4:2:0-only →
  仅顺序，其余 yes；theora Ogg 包 + 3 头状态 → 仅顺序，其余 yes。
- 矩阵覆盖：`video-decode-*` 6 case（每格式一样：`threads = "all"` + `zero_copy/streaming = true`
  + `buffers = 8` 显式，断言解析 + 回退警告 + soak；画质不断言）。
- GUI：解码页每格式四行（threads 两档滑杆 + zero_copy/streaming 两个单选开关 +
  buffers 0-32 滑杆下标），见 SPEC `gui_controls` 24 行。

## 老管线映射（已删除，仅作旧行为对照）

`[play] pipeline` 八档已删除。旧 7 行为统一落到 Clip 路径（`render_additions` 为空，
`copy_block` 位宽分发已覆盖 u64 步进循环）：staged→`feed=staged`+`render.blt`
（manual/bsp_only，pace/drop 保留）；zero_copy→`feed=zero_copy`+`render.blt`；
stream→`feed=stream`+`render.blt`（delta：旧 stream_loop 自旋无丢帧，
新 `pace_or_drop` 最多连丢 2/帧，soak 用 `fps=max` 对齐）；single→NEW `feed=single`
（重读+重解码零缓存，同旧单帧循环）；legacy_blt→clip+eager+`render.blt`；
legacy_direct→clip+eager+`render.direct`+workers=bsp_only；
legacy_u64→clip+eager+`render.direct`+workers=bsp_only+`accel.level=scalar`
（U64 位宽）。旧文件 `pipeline` 键直接忽略（按 `clip` 跑），不另警告。

## 真机错位问题（曾 TODO，现状态）

- 现象：真机概率性 QOI 解码失败/数据错位，QEMU 不现。
- 现措施：失败计数 + 阈值报错（单文件全坏/连续坏不再静默），串口留数；
  播放循环不再因坏帧卡死或空转。
- 未根治：错位根因（固件 DMA/缓存一致性？文件系统短读？）仍待真机抓包；
  复现时取 `HWBENCH` + 串口 `skipped` 计数一起归档。
