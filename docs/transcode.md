# 转码与图库（host 配置 GUI，P5）

> 纯 host 功能：给 U 盘制片（片源 → 固件可播格式）+ 与 `player.toml`
> 同目录的媒体浏览。固件侧零改动（无新串口、无 schema 变更），
> 故 QEMU 矩阵不加 case——覆盖靠 GUI 内 `#[cfg(test)]` 表格/roundtrip，
> 另见下文“验证链”。

## 入口

- 导航新增两页（`Page::Transcode` / `Page::Gallery`，`GlassButton` 导航，
  件库约束见根 AGENTS）：
  - 转换编码：选源（当场探针报 codec/几何/帧率/帧数）→ 目标 → 输出 → 编码
    （后台线程 + 分阶段进度/取消，无时间窗，整片转完）。
  - 图库：`player.toml` 所在目录 = 图库根（默认 `"player.toml"` → `"."`），
    只列 `1080p/` + `wallpaper/`（固件认的布局，不递归）；点行选中，
    一键带源跳转转码页（跳转即探针）。

## 转码管线

```text
片源（mp4[h264/hevc/vp9] / png/jpg/bmp / 1920x1080 raw BGRA，全片）
 → demux 分组（关键帧自然分组 + GROUP_CAP=120 硬切）
 → 逐组：解码（单解码器跑完整轨，状态跨组保持）→ 缩到目标几何
 → 按分段设置批量编码（默认10 帧，段首恒关键帧，拼接恒合法）→ 释放（裸帧不过组）
 → 容器包装 → 图库根目录落盘（默认时间戳名）
```
通用设计，不针对任何片源特化：B 帧内容同样正确（组内重排由
`decode_stream` 承担，跨组引用由持续解码器状态承担；硬切处至多
影响边界帧，详见“内存纪律”）。进度按组/段推进，不再有“0/0 卡死”
（曾因整轨全预解码 ~19GB 常驻 + 全程无进度，把 174MB/6840 帧的
示例片卡在“解码中 0/0”）。

- 解码复用与固件同版本轮子（`shiguredo_mp4` demux + `rusty_h264-decoder 0.16` /
  `rusty_h265 0.6.2` / `vp9dec 0.1.1`；h263/mpeg2v 上游无 crates.io，
  源解码暂只走 mp4 三轨 + 静帧/裸帧，后继再接；**不为解码新增 path 依赖**，
  GUI 与 submodule 解耦，见 `Cargo.toml` 头注释）。
- 内存纪律：40s@60fps 全预解码要 ~19GB，不可能；按关键帧分组
  （`GROUP_CAP` 120 封顶 ≈ 170MB，超了硬切），组内 30 帧一段缩到
  1920x1080 后立即编码、立即释放；压缩包累积（小），裸帧不过组不驻留。
  解码器整轨只建一次（avc/hevc/vp9 各一），状态跨组保持。
- 几何：默认文本框 `1920x1080`；留空跟源，H263 无视走内接标准尺寸+黑边；跟源时固件侧 `display.fit` 保比适配。
  分辨率框上限 4096（16..=4096 + 偶数；1440p/QHD 可直填）：avc/hevc/vp9 按像素量升 level
  （L4.x≤2228224 / L5.0≤8912896 / 再上 L5.1+/L6.0，见 `mp_level_tier`）；
  mpeg2v 只到 MP@HL（1920x1152），超了 job 直接拒单（显式档 UI 侧先拦一道）；
  h263 无视分辨率（内接标准档）；theora 超大帧走编码器报错（无静默坏输出）。
- 帧率：滑条 0=跟源（原速），1-240 按此时长重采样（降帧丢帧，升帧补帧，流式不断组）。
- 取消：点取消后界面立即清零（进度条消失），worker 在下一个小批次边界停下来（解复用每 32 样片、编码每分段查一次），状态显示已取消而不是报错。
- 色彩：YUV↔RGBA 用 BT.601 全范围整数近似（正向与固件
  `Clip::yuv420_to_bgra` 同式，反向标准逆变换，roundtrip 容差 ±2，测试锁死）。

## 八目标（默认输出名即固件片源名，`format` 开关决定播哪个；转码页文本框可重命名 stem，扩展名与子目录不变）

| 目标 | 编码器 | 容器/布局 |
|---|---|---|
| avc → `./<yymmddhhmmss>.mp4` | 硬解优先（mac VideoToolbox H.264 Baseline/CAVLC / Win-Linux Vulkan Video Baseline；段首恒关键帧，30 帧一段与 CPU 时代同语义；失败回退 `less-avc` baseline，1080p 裁剪由轮内 `frame_cropping` 兜底） | `shiguredo_mp4::Mp4FileMuxer`（avc1/avcC + 长度前缀；与固件 demux **同轮子**，回读零口径差） |
| hevc → `./<yymmddhhmmss>.mp4` | 硬解优先（mac VideoToolbox HEVC Main 8-bit / Win-Linux Vulkan Video Main；VPS/SPS/PPS 进 hvcc，in-band 参数集照收；失败回退 `oxideav-h265` registry） | 同 muxer（hvc1/hvcC + 长度前缀；Main 8-bit 420 保守值，level 按分辨率取 4.x/5.0/5.1（名义 30fps 时钟，1440p60+ 名义上要更高 tier，固件不校验）） |
| vp9 → `./<yymmddhhmmss>.mp4` | `rusty_vp9`（crf32 + lag8 + speed3） | 同 muxer（vp09/vpcC，裸帧样片；stss 按 keyframe 写） |
| h263 → `./<yymmddhhmmss>.h263` | `oxideav-h263` registry（只收 §5.1.3 五档标准源格式，生产按源取最大内接档 + 黑边保比，如 1440p 源进 16CIF；固件按帧几何 1:1 左上播） | 裸流 PSC 分隔 |
| mpeg2v → `./<yymmddhhmmss>.m2v` | `oxideav-mpeg12video` registry | 裸基本流（`00 00 01 B3` 起始） |
| theora → `./<yymmddhhmmss>.ogv` | `oxideav-theora` registry intra-only（`qi` 32，setup 用 VP3 默认合成） | `ogg` 组 OggS（三头 + 数据包，同一逻辑流，granule 单调） |
| qois → `./<yymmddhhmmss>.qois` | `qoi` 逐帧 | `u32le len + QOI` 串接（固件同布局） |
| raw → `./<yymmddhhmmss>.raws` | BGRA 直存（`yuv420_to_rgba` + R/B 对调，与固件 `BltPixel` 同布局） | 单文件序列（帧首尾相接无头，`frame_bytes` 整切，固件 `from_raws_bytes` 同口径；流式追加不经 `EncodeAcc`，免大内存；定死 1920x1080，无视分辨率/分段） |

avc/hevc/vp9 三档同名 `video.mp4`（播哪个由 `[play] format` 开关决定，与固件
`video.mp4 优先轨` 语义一致）。裸流只留给无 MP4 封装的格式
（h263/mpeg2v/theora/qois），不再有“mp4-rs 支持却出裸流”的半截子。

## 轮子结论（强制流程记录，不复述版本，见 `tools/config-gui/Cargo.toml` 头注释）

- 搜过 `crates.io`“vp9 encoder / mp4 muxer / ogg mux / qoi”：
  `rusty_vp9`（Remade-With-Rust 同门，Apache-2.0，零依赖，`Vp9Encoder` + 自带解码器，
  文档 honesty 注：比 libvpx 多 ~36% 码率）、`oxideav-mp4`（有 muxer，
  但与固件 demux 不同轮子，有跨解析器口径差，故**不用**，选同轮子的
  `shiguredo_mp4`）、`ogg`（RustAudio，BSD-3，1100 万下载）、`qoi`（0.4）。
- 搜过 theora/h263/h265/mpeg2v 编码器：`oxideav-theora` 上游即带 `FrameEncoder`
  （本仓 patch 只加了 `encoder` 特性门，crates.io 默认特性即有）；
  `oxideav-h263` 有 `H263StreamEncoder`（docs 实锤，非纯解码器）；
  `oxideav-h265` 有 `H265Encoder`（inter/intra/pcm + registry，现为 HEVC 硬解失败兜底）；
  `oxideav-mpeg12video` 有 MPEG-1/2 双编码器 + runtime。
  `rusty_h264`（同门 H.264 编码器）存在，但用户点名 `less-avc`，故 CPU 兜底仍用它。
- 硬编码器（avc/hevc 转码 + avc 解码，用户拍板 2026-10-22）：mac
  `shiguredo_video_toolbox`（与动态背景同轮子，已在依赖里，不新增；
  编码 `Encoder` + 解码 `Decoder` 同箱，I420/NV12 平面直传）/ Win-Linux
  `gpu-video 0.4`（`BytesDecoder` NV12 回读 + `BytesEncoderH264/H265`，`default-features`
  关 wgpu 互操作——经 CPU 侧 NV12 搬运上下屏，无版本耦合；与动态背景不同，
  这里不要纹理互操作，只要字节流）。
  搜过不用：`ffmpeg-next 9`（系统动态库，brew 升版即断 ABI）；内置 ffmpeg 二进制
  （30~80MB + 进程开销）；`egui-video` 系（播放件，不是编码轮子）。
  码控口径：1080p 8Mbps 上限（两家统一；1440p 同上限，单位像素码率同比下降）；
  nominal 30fps 只定码控时钟
  （PTS 自洽，与源 fps 无关，见 `codecs.rs` 头注释）；AVC Baseline + CAVLC
  （与旧 CPU 输出同 profile，固件解码零风险），HEVC Main 8-bit；B 帧全关
  （in-order 输出，与旧管线同序）。
  解码只动 AVC（mac 全 profile 走硬解：VT 同步 `decode()` 1:1 吐解码序，
  转码按样片 PTS（DTS + ctts）排回显示序，Baseline 无 B 帧时排序即 no-op，
  旧 Baseline 门已摘；Vulkan 侧 `FrameSorter` 保序全 profile 可吃）。
  验证：`transcode_hw::avc_high_decodes_hw_with_pts_reorder`
 （High 样片 `src/diag/data/video-avc.mp4` 复用，零回退 + 帧数 1:1 + 与 CPU
  显示序逐帧最佳匹配恒等）。
  选卡（Win/Linux Vulkan，用户拍板 2026-10-22）：`gpu-video 0.4` 的
  `create_adapter` 是 first-match（作者留 TODO 没做偏好），但
  `iter_adapters()` + `AdapterInfo{name/device_type}` 全 public，故不用 fork
  轮子（`vulkan_gpu::select_vulkan_adapter` 即全部选择逻辑；mac 无 Vulkan
  转码页该行恒空隐藏）。转码页“硬解显卡”行（`GlassButton` 单选组，
  件库约束同目标格式行）：自动（=现行 first-match，零行为变化）/ 独显 /
  核显 / 逐适配器名；未命中回退自动（保硬解不断 job，行下另有“当前：”
  实际设备名）。偏好会话级不落盘（转码页状态本就不进 `player.toml`），
  device 缓存以偏好为键，换键下次懒重建。覆盖靠 GUI 内单测
  （`vulkan_gpu_tests` 纯选择四例全平台可跑；真机枚举 Win/Linux 实机自验）。
  真零拷贝不做（VideoToolbox 只收 CPU 切片、无裸句柄；Vulkan 纹理版产的是
  轮子自建 device 的纹理，跨不过 device，必须外部内存导入，无现货轮子；
  NV12 上传 + 平面拼装是 DMA 级搬运，无像素运算，与动态背景 `video_bg` 同纪律）。
- `less-avc` 要调用方给 16 对齐缓冲：GUI 侧补齐（边缘复制）+ pic 报原尺寸，
  轮内 `frame_cropping` 写 SPS 裁剪（`AvcInputFrame`；64x40 回读裁回原几何，
  测试锁死，故 1920x1080 同理可播）。
- 搜过缩放：`image`（0.25，大轮子，一次覆盖静帧解码 + `Triangle` 缩放；
  平面 420 按平面手写双线性——image 只吃打包 RGBA；QOI 编码走独立 `qoi` 箱，
  不绕 image）。
- MP4 源超过 3 轨只收首个匹配视频轨（与固件 `demux_mp4_track` 同顺序；
  entry 缺省样片沿用同轨结论，与固件 `kinds` 回退同式）；
  静帧源 = 单帧；raw 只收 1920x1080 BGRA（半帧不上屏由调用方保证）。

## 验证链（100% Rust，host 内闭环）

- `codecs_tests`：色彩 roundtrip ±2、Annex-B 切分/长度前缀互转、缩放恒等、
  纯搬运单测（AVCC 切分/SPS 门控/NV12 互逆）、七目标冒烟（avc 硬解→rusty_h264_decoder
  回读；vp9 回 vp9dec 跳隐藏帧；theora 三头 + 解码器回读；hevc 硬解→rusty_h265
  Annex-B 回读；h263 PSC 头；mpeg2v sequence_header 头；qois `qoi` 编解回读）。
- `codecs_readback_tests`（固件侧视角）：GUI 产出被**固件同款解码器**接受——
  vp9→`vp9dec`（同解码器跨帧引用，解码器复用）、hevc→`rusty_h265`、
  theora→`oxideav-theora` 解码器、qois→`qoi`。
- `transcode_tests`：avc/hevc/vp9 三路 mp4 roundtrip（编码→同轮子 mux→
  同轮子 demux：样片数 + 关键帧 + 首 entry 種別 avc1/hvc1/vp09，
  即“固件 demux 视角”）；探针测试（mux 产物再探，轨种/几何/样片数对得上）；
  目标输出路径矩阵（avc/hevc/vp9 同名 video.mp4，其余 == 固件片源名）；
  端到端冒烟（合成小 mp4 → 完整 `run_transcode_job` → 产物可被同轮子解回）。
- UI 健壮性（“解码中 0/0 卡死”教训）：转码页运行时每帧 `request_repaint`
  （否则 eframe 按需重绘，进度/完成/报错永远刷不出来）；worker 包
  `catch_unwind`，panic 落袋为 `Finished(Err)`；`try_recv` 遇
  `Disconnected` 直接判失败，不装活。
- 已知残留口径差（诚实记录）：h263/mpeg2v 的 GUI 自测是**本家编解码**
  自 roundtrip，固件侧是异箱解码器（h263-rs / filmcraft-mpeg2v）；
  跨箱一致性靠 U 盘真机/QEMU 人工闭环（待补：把 GUI 产物拷进矩阵镜像跑一遍
  format-* case，另立项，不挡本页落地）。
- QEMU 矩阵：本次无固件变更，不加 case（`covers` 指向的 format-* case
  本来就覆盖全部八格式回放）。
