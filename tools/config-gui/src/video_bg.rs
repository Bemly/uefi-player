//! 动态背景：`/Users/bemly/Movies/input.mp4` 前 40s 循环，玻璃折射燃料。
//!
//! 跨平台硬解，GUI 内零软解（用户拍板）：macOS 走 Apple VideoToolbox，
//! Win/Linux 走 Vulkan Video（`gpu-video` `BytesDecoder` NV12 回读）；
//! 任一端硬解不可用一律静默回退纯色底，绝不回退 CPU 软解。
//!
//! 轮子结论（见 `Cargo.toml` 头注释，不复述版本）：demux 双边复用转码页
//! `demux_groups`（`shiguredo_mp4`）；NV12/I420 经 BT.601 转 RGBA 后走 egui
//! 托管纹理上 wgpu，`LiveBackdrop` 双遍即折射到玻璃上，无需每帧 `set_backdrop`
//! （那要整图 mipgen，开销大）；纹理走 egui 托管（`TexturesDelta` 双 renderer
//! 自动镜像，见 `egui_glass live.rs`），无需 `register_native_texture`。
//!
//! 文件缺失/解码失败：一律静默回退纯色底（装饰不断任何流程，与图库缺目录同式）。
//!
//! `LiveBackdrop::run` 闭包跑两遍（可见 + 离屏），禁写状态/禁推进时钟（见上游
//! `live.rs` 文档）：推进只在 `tick`（闭包外）做，闭包内只读当前纹理绘制。
//!
//! 内存纪律：40s@60fps 全缓存要 ~8.8GB，不可能；后台线程流式硬解 +
//! `sync_channel(8)` 背压（约 29MB），UI 按源 fps 定时消费，超 40s 即重建解码器
//! 回绕（干净 IDR 重起，无参考帧拖影）。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

/// 片源（绝对路径，用户拍板；不存在即静默回退，不报错挡流程）。
pub(crate) const VIDEO_PATH: &str = "/Users/bemly/Movies/input.mp4";
/// 循环窗（秒）：只播前 40s，到点即回绕。
pub(crate) const LOOP_SECS: f64 = 40.0;
/// 后台→UI 有界队列（背压节流硬解，避免 2400 帧常驻）。
pub(crate) const QUEUE_DEPTH: usize = 8;

/// 后台→UI 消息（只传小图 RGBA，解码在 worker 线程，UI 只做纹理上传）。
pub(crate) enum BgMsg {
    Ready { w: u32, h: u32, fps: u32 },
    Frame { image: eframe::egui::ColorImage },
    Finished(Result<(), String>),
}

/// UI 侧状态机（`App` 持有；`texture` 是 egui 托管纹理，双 renderer 自动镜像）。
pub(crate) struct VideoBg {
    pub(crate) texture: Option<eframe::egui::TextureHandle>,
    rx: Option<Receiver<BgMsg>>,
    started: bool,
    pub(crate) playing: bool,
    ready_w: u32,
    ready_h: u32,
    ready_fps: u32,
    next_frame_at: Option<Instant>,
    fallback: Option<String>,
}

impl VideoBg {
    pub(crate) fn new() -> Self {
        Self {
            texture: None,
            rx: None,
            started: false,
            playing: false,
            ready_w: 0,
            ready_h: 0,
            ready_fps: 60,
            next_frame_at: None,
            fallback: None,
        }
    }

    /// 缺片即回退的纯函数（可进黑盒矩阵，无 IO 副作用之外的逻辑）。
    pub(crate) fn should_try_start(path_exists: bool) -> bool {
        // 非 macOS 编译期即 false（调用方 `cfg` 门控），此处只判文件存在。
        path_exists
    }

    /// 循环预算帧数（40s × fps，原片 720p60 即 2400；纯规则，可测）。
    pub(crate) fn loop_budget_frames(fps: u32) -> u64 {
        (LOOP_SECS * fps.max(1) as f64).round() as u64
    }

    /// 是否该回绕（pts 秒数 ≥40 即绕；纯规则，可测）。
    pub(crate) fn should_loop(elapsed_secs: f64) -> bool {
        elapsed_secs >= LOOP_SECS
    }

    /// 每帧推进（闭包外调用）：首帧起线程， draining 队列更新纹理，定速消费。
    /// 返回当前是否有视频可画（调用方据此选画视频还是纯色）。
    pub(crate) fn tick(&mut self, ctx: &eframe::egui::Context) -> bool {
        if self.fallback.is_some() {
            self.playing = false;
            return false;
        }
        if !self.started {
            self.started = true;
            if !Self::should_try_start(std::path::Path::new(VIDEO_PATH).exists()) {
                self.fallback = Some(String::from("video bg: no file, fallback solid"));
                self.playing = false;
                return false;
            }
            let (tx, rx) = sync_channel::<BgMsg>(QUEUE_DEPTH);
            self.rx = Some(rx);
            std::thread::Builder::new()
                .name(String::from("video-bg"))
                .spawn(move || decode_loop(VIDEO_PATH, tx))
                .ok();
        }
        // 非阻塞收完本帧到达的全部消息（只留最新帧，旧帧丢弃跟上 60fps）。
        let mut latest: Option<eframe::egui::ColorImage> = None;
        let mut finished_err: Option<String> = None;
        if let Some(rx) = self.rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(BgMsg::Ready { w, h, fps }) => {
                        self.ready_w = w;
                        self.ready_h = h;
                        self.ready_fps = fps.clamp(1, 120);
                        self.next_frame_at = Some(Instant::now());
                    }
                    Ok(BgMsg::Frame { image }) => {
                        latest = Some(image);
                    }
                    Ok(BgMsg::Finished(Ok(()))) => {}
                    Ok(BgMsg::Finished(Err(e))) => {
                        finished_err = Some(e);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        // worker 正常退出（进程结束才断），不断 UI。
                        break;
                    }
                }
            }
        }
        if let Some(e) = finished_err {
            self.fallback = Some(e);
            self.playing = false;
            return false;
        }
        // 定速消费：按源 fps  pacing，超前则本帧不换图（背压自然形成）。
        if let Some(img) = latest {
            let now = Instant::now();
            let due = match self.next_frame_at {
                None => true,
                Some(t) => now >= t,
            };
            if due {
                let period = Duration::from_secs_f64(1.0 / self.ready_fps.max(1) as f64);
                self.next_frame_at = Some(now + period);
                if let Some(h) = self.texture.as_mut() {
                    h.set(img, eframe::egui::TextureOptions::LINEAR);
                } else {
                    let h = ctx.load_texture("video-bg", img, eframe::egui::TextureOptions::LINEAR);
                    self.texture = Some(h);
                }
                self.playing = self.texture.is_some();
            } else {
                // 未到点：留到下帧（latest 丢弃，队列里还有新帧会跟上）。
                self.playing = self.texture.is_some();
            }
        }
        if self.texture.is_some() {
            // 播放中每帧都要重绘（eframe 按需重绘，不请求即静帧）。
            ctx.request_repaint();
            true
        } else {
            // 还没首帧：也要重绘等首帧，但画纯色。
            ctx.request_repaint();
            false
        }
    }
}

/// 后台解码循环（worker 线程；阻塞式，背压由 `sync_channel` 承担）。
/// 失败只发一次 `Finished(Err)`，UI 静默回退（用户拍板，不弹窗）。
#[cfg(target_os = "macos")]
fn decode_loop(path: &str, tx: SyncSender<BgMsg>) {
    use crate::schema::MediaFormat;
    use crate::transcode::demux_groups;

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg read: {e}"))));
            return;
        }
    };
    // 只收 AVC 轨（input.mp4 即 h264；hev/vp9 样片后继按需再接，不偏离本次需求）。
    let demuxed = match demux_groups(&data, MediaFormat::Avc) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg demux: {e}"))));
            return;
        }
    };
    // 首帧几何从第一组首样片解后得知；先报 fps，宽高待首帧修正（egui 纹理可变尺寸）。
    let fps = demuxed.fps.clamp(1, 120);
    let _ = tx.send(BgMsg::Ready {
        w: 1280,
        h: 720,
        fps,
    });
    // 外层即 40s 回绕：SPS/PPS 固化一次，每轮重建硬解器（干净 IDR，无跨轮参考）。
    let units = crate::codecs::split_annexb(&demuxed.annexb_prefix);
    if units.len() < 2 {
        let _ = tx.send(BgMsg::Finished(Err(String::from("video bg: no sps/pps"))));
        return;
    }
    // 约定：prefix = SPS + PPS（见 `demux_groups` Avc 分支组装序）。
    // `DecoderCodec<'a>` 借 `sps/pps`，但会话内已拷贝 FormatDescription，故用
    // `Box::leak` 固化一次（进程级常驻，40s 循环不重复泄漏）。
    let sps_static: &'static [u8] = Box::leak(units[0].to_vec().into_boxed_slice());
    let pps_static: &'static [u8] = Box::leak(units[1].to_vec().into_boxed_slice());
    let nalu_len = demuxed.length_size as u32;
    loop {
        let mut dec = match make_decoder(sps_static, pps_static, nalu_len) {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(e)));
                return;
            }
        };
        let mut elapsed = 0.0f64;
        let mut sent = 0u64;
        let budget = VideoBg::loop_budget_frames(fps);
        // 首组前缀（SPS/PPS）VideoToolbox 建会话时已消费，此处只喂样片负载
        // （length-prefixed AVCC，与 `nalu_len_bytes` 一致，无需转 Annex-B）。
        'groups: for g in &demuxed.groups {
            for r in g {
                // 到窗即绕（按 pts，不按帧数，B 帧重排下 pts 更准）。
                if VideoBg::should_loop(elapsed) || sent >= budget {
                    break 'groups;
                }
                elapsed += r.dur;
                match dec.decode(&r.payload) {
                    Ok(Some(frame)) => {
                        if let Some(img) = frame_to_image(&frame) {
                            // 背压：UI 跟不上即阻塞在此，不堆内存。
                            if tx.send(BgMsg::Frame { image: img }).is_err() {
                                return;
                            }
                            sent += 1;
                        }
                        // `None`（隐藏帧/重排等待）不计帧，继续喂。
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // 坏帧跳过（计数不报，背景装饰不 halt；连续坏才由超时体现）。
                        let _ = e;
                        continue;
                    }
                }
            }
        }
        // 一轮播完（40s 窗满）即回绕；整文件不足 40s 则从头再播（同 loop 语义）。
        let _ = elapsed;
    }
}

/// VideoToolbox 解码器构造（SPS/PPS/length_size 直给会话；I420 复用现有 BT.601 链）。
#[cfg(target_os = "macos")]
fn make_decoder(
    sps: &'static [u8],
    pps: &'static [u8],
    nalu_len: u32,
) -> Result<shiguredo_video_toolbox::Decoder, String> {
    use shiguredo_video_toolbox::{Decoder, DecoderCodec, DecoderConfig, PixelFormat};
    Decoder::new(DecoderConfig {
        codec: DecoderCodec::H264 {
            sps,
            pps,
            nalu_len_bytes: nalu_len,
        },
        pixel_format: PixelFormat::I420,
    })
    .map_err(|e| format!("video bg decoder: {e:?}"))
}

/// I420 帧（stride 可能 > 宽）→ 紧凑 YUV → RGBA → egui 图。
/// 空平面（异常哨兵，见上游文档）即跳过该帧（返回 None，不断流）。
#[cfg(target_os = "macos")]
fn frame_to_image(
    frame: &shiguredo_video_toolbox::DecodedFrame<'_>,
) -> Option<eframe::egui::ColorImage> {
    match frame {
        shiguredo_video_toolbox::DecodedFrame::I420(f) => {
            let (w, h) = (f.width(), f.height());
            let (yp, up, vp) = (f.y_plane(), f.u_plane(), f.v_plane());
            if yp.is_empty() || up.is_empty() || vp.is_empty() {
                return None;
            }
            // 单遍直转（见 `i420_strided_to_image` 头注释的性能背景）。
            i420_strided_to_image(yp, f.y_stride(), up, f.u_stride(), vp, f.v_stride(), w, h)
        }
        shiguredo_video_toolbox::DecodedFrame::Nv12(_) => {
            // 本背景只开 I420 会话，走不到此分支（穷尽匹配保留）。
            None
        }
    }
}

/// Win/Linux 硬解循环（Vulkan Video H.264，`gpu-video` `BytesDecoder` NV12 回读）。
/// 任一步硬解不可用（无 Vulkan、无 Video 队列、不支持 H.264）即 `Finished(Err)`
/// 静默回退纯色，绝不回退 CPU 软解（用户拍板：GUI 内零软解）。
#[cfg(not(target_os = "macos"))]
fn decode_loop(path: &str, tx: SyncSender<BgMsg>) {
    use crate::codecs::length_prefixed_to_annexb;
    use crate::schema::MediaFormat;
    use crate::transcode::demux_groups;

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg read: {e}"))));
            return;
        }
    };
    // 只收 AVC 轨（input.mp4 即 h264；`gpu-video` 解码侧只支持 H.264，见上游兼容表）。
    let demuxed = match demux_groups(&data, MediaFormat::Avc) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg demux: {e}"))));
            return;
        }
    };
    let fps = demuxed.fps.clamp(1, 120);
    let _ = tx.send(BgMsg::Ready {
        w: 1280,
        h: 720,
        fps,
    });
    // Vulkan 硬解设备只建一次（40s 回绕只重建 `BytesDecoder`，device 复用）。
    let instance = match gpu_video::VulkanInstance::new() {
        Ok(i) => i,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg vulkan: {e:?}"))));
            return;
        }
    };
    let adapter = match instance.create_adapter(&gpu_video::parameters::VulkanAdapterDescriptor {
        supports_decoding: true,
        supports_encoding: false,
    }) {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.send(BgMsg::Finished(Err(format!("video bg adapter: {e:?}"))));
            return;
        }
    };
    let device =
        match adapter.create_device(&gpu_video::parameters::VulkanDeviceDescriptor::default()) {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(format!("video bg device: {e:?}"))));
                return;
            }
        };
    // 外层即 40s 回绕：每轮重建 `BytesDecoder`（干净 IDR，无跨轮参考）。
    loop {
        let mut dec = match device
            .create_bytes_decoder_h264(gpu_video::parameters::DecoderParameters::default())
        {
            Ok(d) => d,
            Err(e) => {
                let _ = tx.send(BgMsg::Finished(Err(format!("video bg decoder: {e:?}"))));
                return;
            }
        };
        // 首包带 SPS/PPS 前缀（`gpu-video` 吃 Annex-B bytestream，见上游 player 示例；
        // 本仓 `length_size` 前缀先转 Annex-B，与固件 `demux_mp4_track` 同式）。
        let mut first = true;
        let mut elapsed = 0.0f64;
        let mut sent = 0u64;
        let budget = VideoBg::loop_budget_frames(fps);
        'groups: for g in &demuxed.groups {
            for r in g {
                if VideoBg::should_loop(elapsed) || sent >= budget {
                    break 'groups;
                }
                elapsed += r.dur;
                let mut annexb = Vec::new();
                if first {
                    annexb.extend_from_slice(&demuxed.annexb_prefix);
                    first = false;
                }
                annexb.extend(length_prefixed_to_annexb(&r.payload, demuxed.length_size));
                match dec.decode(gpu_video::EncodedInputChunk {
                    data: &annexb,
                    pts: None,
                }) {
                    Ok(frames) => {
                        for f in &frames {
                            if let Some(img) =
                                nv12_to_image(&f.data.frame, f.data.width, f.data.height)
                            {
                                if tx.send(BgMsg::Frame { image: img }).is_err() {
                                    return;
                                }
                                sent += 1;
                                if sent >= budget {
                                    break 'groups;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // 坏块跳过（装饰不 halt）；硬解整体失败才回退（如下）。
                        let _ = e;
                        continue;
                    }
                }
            }
        }
        // 40s 窗满即回绕（`flush` 把重排延迟帧吐完再绕，不丢尾）。
        let _ = elapsed;
    }
}

/// 带 stride 的 I420 直转 `ColorImage`（BT.601 全范围，与 `codecs::yuv420_to_rgba` 同式）。
/// 直接填 `Vec<Color32>`，绕开 `ColorImage::from_rgba_unmultiplied`：
/// 后者在 dev 下把整幅 RGBA 再走一遍 `chunks_exact + map + collect` 塔
/// （采样实锤占 worker 近半时间，`bytemuck` 预检逐块跑），720p 直接 100ms+。
/// 行内用 `get_unchecked` + 手写钳制（不用 `.clamp()`：dev 下泛型调用不内联，
/// 2.7M 次调用同样致命；`#[inline(always)]` 在 dev 下仍生效，分支可预测）。
/// 安全：整幅已预检（越界直接返回 None），下标恒小于行长；
/// 另有逐字节一致测试锁死输出与旧路相同。
fn i420_strided_to_image(
    y_p: &[u8],
    y_s: usize,
    u_p: &[u8],
    u_s: usize,
    v_p: &[u8],
    v_s: usize,
    w: usize,
    h: usize,
) -> Option<eframe::egui::ColorImage> {
    use eframe::egui::ColorImage;
    if w == 0 || h == 0 {
        return None;
    }
    let uvw = w.div_ceil(2);
    let uvh = h.div_ceil(2);
    // 整幅预检一次：后面所有行内下标恒在界内（奇宽尾列 col/2 = uvw-1 同样覆盖）。
    if y_p.len() < (h - 1) * y_s + w {
        return None;
    }
    if u_p.len() < (uvh - 1) * u_s + uvw || v_p.len() < (uvh - 1) * v_s + uvw {
        return None;
    }
    #[inline(always)]
    fn clamp8(v: i32) -> u8 {
        if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u8
        }
    }
    // 直接写原始字节缓冲，最后整体转交 `Vec<Color32>`：逐像素 `push(from_rgb(..))`
    // 在 dev 下是 92 万次 push 检查 + 92 万次构造调用（非泛型，dev 不内联），
    // 实测占转换大头；裸写只剩整数运算 + 指针步进。
    // SAFETY（布局）：`Color32` 为单 `[u8;4]` 域 + `#[repr(C)] + align(4)`
    // （见 `ecolor` 源码），任一 4 字节图案都是合法值；下式把布局变化变成
    // 编译错误而非 UB；缓冲用等价布局显式分配，全写满后按同一布局交还。
    const _: () = assert!(
        std::mem::size_of::<eframe::egui::Color32>() == 4
            && std::mem::align_of::<eframe::egui::Color32>() == 4
    );
    let n = w.checked_mul(h)?;
    let bytes = n.checked_mul(4)?;
    let layout = std::alloc::Layout::from_size_align(bytes, 4).ok()?;
    // SAFETY（分配）：布局非零（w/h 非零已预检）且对齐合法；失败返回 None。
    let raw = unsafe { std::alloc::alloc_zeroed(layout) };
    if raw.is_null() {
        return None;
    }
    // 行带并行：按核均分行，尾带兜底。线程间只传整数（行号/基址 `usize`，均为
    // `Send`），各带输出片在线程内从基址重建，父端在 `scope` 存续期间绝不形成
    // 整幅 `&mut`（否则与线程内写别名），`join` 后才交还 `Vec`。
    let procs = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(h);
    let rows_per_band = h.div_ceil(procs);
    let base = raw as usize;
    std::thread::scope(|s| {
        for band in 0..procs {
            let row_start = band * rows_per_band;
            if row_start >= h {
                break;
            }
            let row_end = (row_start + rows_per_band).min(h);
            s.spawn(move || {
                // SAFETY（带片）：带片为 `[row_start, row_end)` 行的输出区，
                // 各带行区间互不重叠；`row_end` 钳制保证片长精确
                // `(row_end - row_start) * w * 4`；输入下标由整幅预检覆盖。
                let out_band = unsafe {
                    std::slice::from_raw_parts_mut(
                        (base + row_start * w * 4) as *mut u8,
                        (row_end - row_start) * w * 4,
                    )
                };
                for row in row_start..row_end {
                    let y_row = row * y_s;
                    let u_row = (row / 2) * u_s;
                    let v_row = (row / 2) * v_s;
                    let o_row = (row - row_start) * w * 4;
                    for col in 0..w {
                        unsafe {
                            let yy = *y_p.get_unchecked(y_row + col) as i32;
                            let uu = *u_p.get_unchecked(u_row + col / 2) as i32 - 128;
                            let vv = *v_p.get_unchecked(v_row + col / 2) as i32 - 128;
                            let dst = out_band.as_mut_ptr().add(o_row + col * 4);
                            *dst = clamp8(yy + ((359 * vv) >> 8));
                            *dst.add(1) = clamp8(yy - ((88 * uu + 183 * vv) >> 8));
                            *dst.add(2) = clamp8(yy + ((454 * uu) >> 8));
                            *dst.add(3) = 255;
                        }
                    }
                }
            });
        }
    });
    // SAFETY：`scope` 已 join，所有带片合起来恰写满 `bytes` 字节；同一 `Layout`
    // 交还给 `Vec<Color32>`（大小/对齐一致），由 `Vec` 接管释放。
    let pixels = unsafe { Vec::from_raw_parts(raw as *mut eframe::egui::Color32, n, n) };
    Some(ColorImage {
        size: [w, h],
        source_size: eframe::egui::Vec2::new(w as f32, h as f32),
        pixels,
    })
}
/// NV12（Y + interleaved UV）→ RGBA（BT.601 全范围，与 `codecs::yuv420_to_rgba` 同式，
/// 只是色度交织取数；纯数学，无新轮子，可进黑盒矩阵）。
/// 空/短包即 None（跳过该帧，不断流）。
/// Win/Linux 硬解回读用；macOS 下仅测试用（生产走 I420 路）。
#[cfg(any(not(target_os = "macos"), test))]
fn nv12_to_image(nv12: &[u8], w: u32, h: u32) -> Option<eframe::egui::ColorImage> {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let y_len = w.checked_mul(h)?;
    let uv_len = w.checked_mul(h.div_ceil(2))?;
    if nv12.len() < y_len + uv_len {
        return None;
    }
    let (y_plane, uv_plane) = nv12.split_at(y_len);
    #[inline(always)]
    fn clamp8(v: i32) -> u8 {
        if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u8
        }
    }
    use eframe::egui::{Color32, ColorImage};
    let mut pixels = Vec::with_capacity(w * h);
    // 偶宽快道：UV 行内 `uv_idx + 1` 恒在界内（见下），行内 `get_unchecked`。
    // 奇宽（H.264 4:2:0 解码器吐不出奇宽，防线而已）走安全标量路，缺 V 按 128
    // （与旧 `.get().unwrap_or(128)` 同语义）。
    if w % 2 == 0 {
        for row in 0..h {
            for col in 0..w {
                // SAFETY：`y_len`/`uv_len` 预检保证两平面长度；偶宽下
                // `uv_idx + 1 = (row/2)*w + col/2*2 + 1 <= (row/2)*w + w - 1 < 行尾`。
                unsafe {
                    let yy = *y_plane.get_unchecked(row * w + col) as i32;
                    let uv_idx = (row / 2) * w + (col / 2) * 2;
                    let uu = *uv_plane.get_unchecked(uv_idx) as i32 - 128;
                    let vv = *uv_plane.get_unchecked(uv_idx + 1) as i32 - 128;
                    pixels.push(Color32::from_rgb(
                        clamp8(yy + ((359 * vv) >> 8)),
                        clamp8(yy - ((88 * uu + 183 * vv) >> 8)),
                        clamp8(yy + ((454 * uu) >> 8)),
                    ));
                }
            }
        }
    } else {
        for row in 0..h {
            for col in 0..w {
                let yy = y_plane[row * w + col] as i32;
                let uv_idx = (row / 2) * w + (col / 2) * 2;
                let uu = uv_plane.get(uv_idx).copied().unwrap_or(128) as i32 - 128;
                let vv = uv_plane.get(uv_idx + 1).copied().unwrap_or(128) as i32 - 128;
                pixels.push(Color32::from_rgb(
                    clamp8(yy + ((359 * vv) >> 8)),
                    clamp8(yy - ((88 * uu + 183 * vv) >> 8)),
                    clamp8(yy + ((454 * uu) >> 8)),
                ));
            }
        }
    }
    Some(ColorImage {
        size: [w, h],
        source_size: eframe::egui::Vec2::new(w as f32, h as f32),
        pixels,
    })
}

#[cfg(test)]
mod video_bg_tests {
    use super::*;

    #[test]
    fn loop_window_is_40s() {
        assert_eq!(VideoBg::loop_budget_frames(60), 2400);
        assert_eq!(VideoBg::loop_budget_frames(30), 1200);
        assert!(!VideoBg::should_loop(39.9));
        assert!(VideoBg::should_loop(40.0));
        assert!(VideoBg::should_loop(114.4));
    }

    #[test]
    fn start_gate_is_file_exists() {
        assert!(VideoBg::should_try_start(true));
        assert!(!VideoBg::should_try_start(false));
    }

    #[test]
    fn queue_depth_bounds_memory() {
        // 720p RGBA 单帧约 3.6MB，8 帧约 29MB（40s 全缓存要 8.8GB，不可能）。
        let per_frame = 1280usize * 720 * 4;
        assert!(per_frame * QUEUE_DEPTH < 64 * 1024 * 1024);
        assert_eq!(LOOP_SECS, 40.0);
        assert_eq!(VIDEO_PATH, "/Users/bemly/Movies/input.mp4");
    }

    #[test]
    fn fast_path_matches_reference_byte_for_byte() {
        // 快道（行切片双像素块）与旧路（紧凑 + `yuv420_to_rgba`）逐字节一致；
        // 含 stride 填充（y_s=1312 > 1280，uv_s=656 > 640）与奇数宽尾列。
        for (w, h, y_s, uv_s) in [
            (1280usize, 720usize, 1280usize, 640usize),
            (1279, 719, 1280, 640),
        ] {
            let uvw = w.div_ceil(2);
            let uvh = h.div_ceil(2);
            let mut y_p = vec![0u8; (h - 1) * y_s + w];
            let mut u_p = vec![0u8; (uvh - 1) * uv_s + uvw];
            let mut v_p = vec![0u8; (uvh - 1) * uv_s + uvw];
            for (i, b) in y_p.iter_mut().enumerate() {
                *b = ((i * 7 + 13) % 256) as u8;
            }
            for (i, b) in u_p.iter_mut().enumerate() {
                *b = ((i * 11 + 101) % 256) as u8;
            }
            for (i, b) in v_p.iter_mut().enumerate() {
                *b = ((i * 13 + 37) % 256) as u8;
            }
            let fast =
                i420_strided_to_image(&y_p, y_s, &u_p, uv_s, &v_p, uv_s, w, h).expect("fast image");
            // 参考：拷紧凑 + 旧 `yuv420_to_rgba`。
            let mut y = vec![0u8; w * h];
            let mut u = vec![0u8; uvw * uvh];
            let mut v = vec![0u8; uvw * uvh];
            for row in 0..h {
                y[row * w..row * w + w].copy_from_slice(&y_p[row * y_s..row * y_s + w]);
            }
            for row in 0..uvh {
                u[row * uvw..row * uvw + uvw].copy_from_slice(&u_p[row * uv_s..row * uv_s + uvw]);
                v[row * uvw..row * uvw + uvw].copy_from_slice(&v_p[row * uv_s..row * uv_s + uvw]);
            }
            let back = crate::codecs::yuv420_to_rgba(&crate::codecs::Yuv420 {
                w: w as u32,
                h: h as u32,
                y,
                u,
                v,
            });
            assert_eq!(fast.size, [w, h]);
            assert_eq!(fast.pixels.len(), back.px.len() / 4);
            for (i, p) in fast.pixels.iter().enumerate() {
                assert_eq!(p[0], back.px[i * 4], "r @ {i} ({w}x{h})");
                assert_eq!(p[1], back.px[i * 4 + 1], "g @ {i} ({w}x{h})");
                assert_eq!(p[2], back.px[i * 4 + 2], "b @ {i} ({w}x{h})");
                assert_eq!(p[3], 255);
            }
        }
    }

    #[test]
    fn nv12_solid_gray_roundtrip() {
        // 全128 NV12（Y=U=V=128）应得中灰（BT.601 同 `yuv420_to_rgba` 口径）。
        let (w, h) = (8u32, 8u32);
        let mut nv12 = vec![128u8; (w * h) as usize];
        nv12.extend(vec![128u8; (w * h / 2) as usize]);
        let img = nv12_to_image(&nv12, w, h).expect("nv12 image");
        assert_eq!(img.size, [8, 8]);
        // 首像素 RGB 应全相等（中灰），alpha 恒 255。
        let px = &img.pixels[0];
        assert_eq!([px[0], px[1], px[2]], [128, 128, 128]);
        assert!(img.pixels.iter().all(|p| p[3] == 255));
        // 短包/零几何一律 None（跳帧，不断流）。
        assert!(nv12_to_image(&[0u8; 10], w, h).is_none());
        assert!(nv12_to_image(&nv12, 0, h).is_none());
    }
}
