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
 → 关键帧分组（GROUP_CAP=60，超了硬切）→ 缩到 1920x1080（YUV 平面双线性）
 → 七目标之一编码 → 容器包装 → 图库同名落盘（`1080p/video.*`）
```

- 解码复用与固件同版本轮子（`shiguredo_mp4` demux + `rusty_h264-decoder 0.16` /
  `rusty_h265 0.6.2` / `vp9dec 0.1.1`；h263/mpeg2v 上游无 crates.io，
  源解码暂只走 mp4 三轨 + 静帧/裸帧，后继再接；**不为解码新增 path 依赖**，
  GUI 与 submodule 解耦，见 `Cargo.toml` 头注释）。
- 内存纪律：组是关键帧自然边界（组首恒关键帧，拼接恒合法）；
  60 帧×8MB ≈ 480MB 瞬时上限；压缩包累积（小），裸帧不过组不驻留。
- 几何：固件定死 1920x1080（`1280x720→×1.5` 纯放大）。
- 色彩：YUV↔RGBA 用 BT.601 全范围整数近似（正向与固件
  `Clip::yuv420_to_bgra` 同式，反向标准逆变换，roundtrip 容差 ±2，测试锁死）。

## 七目标（输出名即固件片源名，`format` 开关决定播哪个）

| 目标 | 编码器 | 容器/布局 |
|---|---|---|
| avc → `1080p/video.mp4` | `less-avc` baseline（1080p 裁剪由轮内 `frame_cropping` 兜底） | `shiguredo_mp4::Mp4FileMuxer`（avc1/avcC + 长度前缀；与固件 demux **同轮子**，回读零口径差） |
| hevc → `1080p/video.mp4` | `oxideav-h265` registry（`params.width/height` 必填；VPS/SPS/PPS 进 hvcc，in-band 参数集照收） | 同 muxer（hvc1/hvcC + 长度前缀；Main 8-bit 420 保守值，level 按帧率取 4.0/4.1） |
| vp9 → `1080p/video.mp4` | `rusty_vp9`（crf32 + lag8 + speed3） | 同 muxer（vp09/vpcC，裸帧样片；stss 按 keyframe 写） |
| h263 → `1080p/video.h263` | `oxideav-h263` registry（只要 §5.1.3 标准源格式；测试用 QCIF 176x144） | 裸流 PSC 分隔 |
| mpeg2v → `1080p/video.m2v` | `oxideav-mpeg12video` registry | 裸基本流（`00 00 01 B3` 起始） |
| theora → `1080p/video.ogv` | `oxideav-theora` registry intra-only（`qi` 32，setup 用 VP3 默认合成） | `ogg` 组 OggS（三头 + 数据包，同一逻辑流，granule 单调） |
| qois → `1080p/video.qois` | `qoi` 逐帧 | `u32le len + QOI` 串接（固件同布局） |

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
  `oxideav-h265` 有 `H265Encoder`（inter/intra/pcm + registry）；
  `oxideav-mpeg12video` 有 MPEG-1/2 双编码器 + runtime。
  `rusty_h264`（同门 H.264 编码器）存在，但用户点名 `less-avc`，故不用。
- 搜过缩放：`image`（0.25，大轮子，一次覆盖静帧解码 + `Triangle` 缩放；
  平面 420 按平面手写双线性——image 只吃打包 RGBA；QOI 编码走独立 `qoi` 箱，
  不绕 image）。
- MP4 源超过 3 轨只收首个匹配视频轨（与固件 `demux_mp4_track` 同顺序；
  entry 缺省样片沿用同轨结论，与固件 `kinds` 回退同式）；
  静帧源 = 单帧；raw 只收 1920x1080 BGRA（半帧不上屏由调用方保证）。

## 验证链（100% Rust，host 内闭环）

- `codecs_tests`：色彩 roundtrip ±2、Annex-B 切分/长度前缀互转、缩放恒等、
  七目标冒烟（avc 回 rusty_h264_decoder；vp9 回 vp9dec 跳隐藏帧；
  theora 三头 + 解码器回读；hevc Annex-B 回 rusty_h265；h263 PSC 头；
  mpeg2v sequence_header 头；qois `qoi` 编解回读）。
- `codecs_readback_tests`（固件侧视角）：GUI 产出被**固件同款解码器**接受——
  vp9→`vp9dec`（同解码器跨帧引用，解码器复用）、hevc→`rusty_h265`、
  theora→`oxideav-theora` 解码器、qois→`qoi`。
- `transcode_tests`：avc/hevc/vp9 三路 mp4 roundtrip（编码→同轮子 mux→
  同轮子 demux：样片数 + 关键帧 + 首 entry 種別 avc1/hvc1/vp09，
  即“固件 demux 视角”）；探针测试（mux 产物再探，轨种/几何/样片数对得上）；
  目标输出路径矩阵（avc/hevc/vp9 同名 video.mp4，其余 == 固件片源名）。
- 已知残留口径差（诚实记录）：h263/mpeg2v 的 GUI 自测是**本家编解码**
  自 roundtrip，固件侧是异箱解码器（h263-rs / filmcraft-mpeg2v）；
  跨箱一致性靠 U 盘真机/QEMU 人工闭环（待补：把 GUI 产物拷进矩阵镜像跑一遍
  format-* case，另立项，不挡本页落地）。
- QEMU 矩阵：本次无固件变更，不加 case（`covers` 指向的 format-* case
  本来就覆盖全部八格式回放）。
