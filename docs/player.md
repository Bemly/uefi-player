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
   分工由 `[render] dist` 定：`static` 按核固定行区（同构核最优）；
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

## 播放管线（`[play] pipeline`，手动切换）

`clip`（默认）：`Clip` 全量预解码 + 现行三路/执行器后端（上文全部）。
其余 7 档是 `Clip` 之前的老实现（修复后进矩阵，可手动切对照）：

- `staged`：`VideoMemory` 整文件进内存 + 逐帧三段解码（QOI→Raw→Blt）+ Blt 整推，
  限速掉帧与现路同预算语义。
- `zero_copy`：同上，省掉 Raw 中转（直解进 Blt 缓冲 + R/B 交换）。
- `stream`：真文件流式（`read_frame_next` 边读边解 + 短读补齐），不全量进内存；
  限速只自旋补齐，不丢帧。
- `single`：`draw_once` 单帧反复（容器取首帧，纯 QOI 整解码），每帧重读文件，
  零缓存下限对照。
- `legacy_blt` / `legacy_direct` / `legacy_u64`：`Clip` 预解码 + 老上传循环
  （Blt 整推 / BSP 直写 / u64 步进直写），无 OSD/多核/影子。

老路无 OSD、无多核、无影子、无执行器后端；只读 qois（raw 回落 clip 并警告）。
矩阵覆盖：`video-pipe-*` 7 case（path 标记 + soak 保活）。

## 真机错位问题（曾 TODO，现状态）

- 现象：真机概率性 QOI 解码失败/数据错位，QEMU 不现。
- 现措施：失败计数 + 阈值报错（单文件全坏/连续坏不再静默），串口留数；
  播放循环不再因坏帧卡死或空转。
- 未根治：错位根因（固件 DMA/缓存一致性？文件系统短读？）仍待真机抓包；
  复现时取 `HWBENCH` + 串口 `skipped` 计数一起归档。
