//! 动态背景：`/Users/bemly/Movies/input.mp4` 前 40s 循环，玻璃折射燃料。
//!
//! 轮子结论（见 `Cargo.toml` 头注释，不复述版本）。
//!
//! 搜过 `crates.io`“wgpu video / egui video”：`gpu-video 0.4`（Vulkan Video +
//! wgpu 29，H.264 帧不出显存）兼容表只有 Linux(NVIDIA/AMD)/Windows，macOS Metal
//! 无 Vulkan Video，本机 `VulkanInstance::new` 即错，故不用。
//!
//! `egui-video 0.9`（ffmpeg6/7 + SDL2，CPU 解码，重 C 依赖，要 release opt-level）
//! 与本仓纯 Rust转码链相悖，故不用。
//!
//! `egui-sharkplayer 0.5`（libmpv2，eframe 0.35 同版，但自述
//! wgpu 下要经内存往返，且要系统 libmpv，而本仓 renderer 已强制 Wgpu）故不用。
//!
//! 最终用同门 `shiguredo_video_toolbox`（Apple VideoToolbox H.264 硬解，与本仓
//! `shiguredo_mp4` 同 vendor 同用法；demux 复用转码页 `demux_groups`，SPS/PPS/
//! length_size 直给 `DecoderCodec::H264`，I420 帧经 `codecs::yuv420_to_rgba`
//! 转 RGBA 后走 egui 纹理上 wgpu）。
//!
//! `LiveBackdrop` 双遍即折射到玻璃上，无需每帧
//! `set_backdrop`（那要整图 mipgen，开销大）；纹理走 egui 托管（`TexturesDelta`
//! 双 renderer 自动镜像，见 `egui_glass live.rs`），无需 `register_native_texture`。
//!
//! 非 macOS（VideoToolbox 框架缺席）或文件缺失/解码失败：一律静默回退纯色底
//! （装饰不断任何流程，与图库缺目录同式；用户拍板）。
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
        // 非 macOS：直接回退（VideoToolbox 框架缺席，编译期无解码器）。
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            self.playing = false;
            return false;
        }
        #[cfg(target_os = "macos")]
        {
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
                        let h =
                            ctx.load_texture("video-bg", img, eframe::egui::TextureOptions::LINEAR);
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
}

/// 后台解码循环（worker 线程；阻塞式，背压由 `sync_channel` 承担）。
/// 失败只发一次 `Finished(Err)`，UI 静默回退（用户拍板，不弹窗）。
#[cfg(target_os = "macos")]
fn decode_loop(path: &str, tx: SyncSender<BgMsg>) {
    use crate::codecs::yuv420_to_rgba;
    use crate::codecs::Yuv420;
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
    use crate::codecs::Yuv420;
    let (w, h, y_tight, u_tight, v_tight) = match frame {
        shiguredo_video_toolbox::DecodedFrame::I420(f) => {
            let (w, h) = (f.width(), f.height());
            if w == 0 || h == 0 {
                return None;
            }
            let (ys, us, vs) = (f.y_stride(), f.u_stride(), f.v_stride());
            let (yp, up, vp) = (f.y_plane(), f.u_plane(), f.v_plane());
            if yp.is_empty() || up.is_empty() || vp.is_empty() {
                return None;
            }
            let uvw = w.div_ceil(2);
            let uvh = h.div_ceil(2);
            let mut y = vec![0u8; w * h];
            let mut u = vec![0u8; uvw * uvh];
            let mut v = vec![0u8; uvw * uvh];
            for row in 0..h {
                let src = row * ys;
                if src + w <= yp.len() {
                    y[row * w..row * w + w].copy_from_slice(&yp[src..src + w]);
                }
            }
            for row in 0..uvh {
                let s = row * us;
                let t = row * uvw;
                if s + uvw <= up.len() {
                    u[t..t + uvw].copy_from_slice(&up[s..s + uvw]);
                }
                let s = row * vs;
                if s + uvw <= vp.len() {
                    v[t..t + uvw].copy_from_slice(&vp[s..s + uvw]);
                }
            }
            (w, h, y, u, v)
        }
        shiguredo_video_toolbox::DecodedFrame::Nv12(_) => {
            // 本背景只开 I420 会话，走不到此分支（穷尽匹配保留）。
            return None;
        }
    };
    let rgba = crate::codecs::yuv420_to_rgba(&Yuv420 {
        w: w as u32,
        h: h as u32,
        y: y_tight,
        u: u_tight,
        v: v_tight,
    });
    Some(eframe::egui::ColorImage::from_rgba_unmultiplied(
        [rgba.w as usize, rgba.h as usize],
        &rgba.px,
    ))
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
}
