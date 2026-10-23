//! 动态背景 GPU 转换版：硬解在 GPU，CPU 只搬运不算像素，玻璃在 live 内采样。
//!
//! 链路（默认三级：选中接管 > 图库首个 avc/hevc > 内嵌 `dsxhgm.mp4`，整片循环；
//! 缺片/失败静默回静态底，不断任何流程；
//! 图库选中 avc/hevc 片源即接管背景（`resolve_source`，切源杀旧 worker 重起）：
//! worker 线程硬解（mac VideoToolbox / Win-Linux Vulkan `BytesDecoder`）→
//! 只传 NV12（Y + 交错 UV，不做 BT.601，不转 RGBA）→ UI 线程 DMA 直传两张
//! R8/RG GPU 纹理 → 自研 20 行 WGSL 全屏三角转 RGBA（与转码页
//! `codecs::yuv420_to_rgba` 同 BT.601 全范围口径，逐字节一致由单测锁死）→
//! 双槽 RGBA native 池（`register_native_texture`，twin 自动镜像）→ 只在
//! `live.run` 闭包内画当前槽（与静态 twin 图同槽位、同右移矩形，玻璃折射源）。
//!
//! 轮子结论（见 `Cargo.toml` 头注释，不复述版本）：demux 复用转码页
//! `demux_groups`；真零拷贝走不通（VideoToolbox 轮子只给 CPU 切片、无裸句柄；
//! Vulkan 纹理版产的是轮子自建 device 的纹理，跨不过 device），故 NV12 上传 +
//! WGSL 转换是本仓轮子下的 GPU 上限。CPU 只剩压缩包搬运 + `write_texture`
//! 提交，零像素数学；上传量为旧 RGBA 路的 3/8；显示与片源同帧率 + 失焦停推。
//!
//! `LiveBackdrop::run` 闭包跑两遍（可见 + 离屏），禁写状态/禁推进时钟：推进只
//! 在 `tick`（闭包外）做，闭包内只读当前 native id 绘制。paint 回调（nounwind，
//! panic 即 abort，血泪）里本模块零逻辑——转换提交全在 `tick` 内同步做完。
//!
//! 内存纪律：整片循环 + `sync_channel(4)` 背压（NV12 约 5.6MB）+ B 帧重排窗
//! （定深 8，约 11MB 上限，不攒整组），UI 按 min(源 fps, 15) 定速消费，播完即
//! 重建解码器回绕（干净 IDR，无跨轮参考）。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use egui_wgpu::wgpu;

/// B 帧重排窗深：VT 按解码序吐帧，攒够即按 pts 取最小发出（显示序）；
/// 定窗免攒整组（长 GOP 不爆内存），深度盖 x264 默认连 B（High 档实测 4 连 B）。
const REORDER_DEPTH: usize = 8;

/// 窗内取最小 pts（B 帧重排；纯规则，可单测；空窗即 None，不断流）。
fn pop_earliest<T>(pending: &mut Vec<(i128, T)>) -> Option<(i128, T)> {
    if pending.is_empty() {
        return None;
    }
    let mut k = 0;
    for i in 1..pending.len() {
        if pending[i].0 < pending[k].0 {
            k = i;
        }
    }
    Some(pending.remove(k))
}

/// 内嵌默认背景（`assets/dsxhgm.mp4`，avc 720p60 整片循环；编进二进制，
/// 图库无集内项时回落于此，无外部文件依赖）。
const EMBEDDED_BG: &[u8] = include_bytes!("../../assets/dsxhgm.mp4");

/// 背景片源：文件（选中接管/图库首个跟随）或内嵌字节（图库无集内项时回落）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BgSource {
    File(String),
    Embedded,
}

/// 后台→UI 有界队列（背压节流硬解；NV12 单帧约 1.4MB，4 帧约 5.6MB）。
pub(crate) const QUEUE_DEPTH: usize = 4;
/// 显示封顶帧率（与片源一致，60fps 全速；CPU 换流畅，见 tick 注释）。
pub(crate) const DISPLAY_FPS_MAX: u32 = 60;

/// 后台→UI 消息（只传 NV12 压缩平面，解码在 worker 线程，转换在 GPU）。
pub(crate) enum BgMsg {
    Ready {
        w: u32,
        h: u32,
        fps: u32,
    },
    FrameNv12 {
        w: u32,
        h: u32,
        y: Vec<u8>,
        uv: Vec<u8>,
    },
    Finished(Result<(), String>),
}

/// UI 侧状态机（`App` 持有；显示纹理是 native id，双 renderer 自动镜像）。
pub(crate) struct VideoBg {
    rx: Option<Receiver<BgMsg>>,
    started: bool,
    pub(crate) playing: bool,
    ready_w: u32,
    ready_h: u32,
    ready_fps: u32,
    next_frame_at: Option<Instant>,
    fallback: Option<String>,
    rs: Option<egui_wgpu::RenderState>,
    gpu: Option<GpuYuv>,
    front: Option<(egui::TextureId, f32, f32)>,
    /// 当前片源（None = 无视频 → 静态底；默认由调用方按选中/图库首个/内嵌三级填入）。
    source: Option<BgSource>,
}

impl VideoBg {
    pub(crate) fn new(rs: Option<egui_wgpu::RenderState>) -> Self {
        Self {
            rx: None,
            started: false,
            playing: false,
            ready_w: 0,
            ready_h: 0,
            ready_fps: 60,
            next_frame_at: None,
            fallback: None,
            rs,
            gpu: None,
            front: None,
            source: None,
        }
    }

    /// 当前显示帧（`live.run` 闭包内只读绘制；无帧即 None，调用方回静态底）。
    pub(crate) fn front(&self) -> Option<(egui::TextureId, f32, f32)> {
        self.front
    }

    /// 缺片即回退的纯函数（可进黑盒矩阵，无 IO 副作用之外的逻辑）。
    pub(crate) fn should_try_start(path_exists: bool) -> bool {
        path_exists
    }

    /// 选中接管背景的纯规则（可单测，无 IO）：选中项编解码在集内才接管，
    /// 否则回落图库首个集内项（默认背景跟随图库，不硬编码）。集：mac
    /// `avc/hevc`（VT 双解）；Win/Linux 仅 `avc`（`gpu-video` 0.4 无 HEVC 解码器）。
    /// `codec` 由图库封面线程探针回填；图库按 mtime 从旧到新，`dsxhgm.mp4` 触古恒居首。
    pub(crate) fn resolve_source(
        entries: &[crate::gallery::GalleryEntry],
        selected: Option<usize>,
        dir: &str,
    ) -> Option<String> {
        if let Some(e) = selected.and_then(|i| entries.get(i)) {
            let ok = matches!(e.codec, Some("avc"))
                || (cfg!(target_os = "macos") && matches!(e.codec, Some("hevc")));
            if ok {
                return Some(
                    std::path::Path::new(dir)
                        .join(&e.name)
                        .display()
                        .to_string(),
                );
            }
        }
        Self::default_first_source(entries, dir)
    }

    /// 默认背景：图库首个集内项（mtime 从旧到新；无集内项即 None，调用方再回落内嵌）。
    pub(crate) fn default_first_source(
        entries: &[crate::gallery::GalleryEntry],
        dir: &str,
    ) -> Option<String> {
        let e = entries.iter().find(|e| {
            matches!(e.codec, Some("avc"))
                || (cfg!(target_os = "macos") && matches!(e.codec, Some("hevc")))
        })?;
        Some(
            std::path::Path::new(dir)
                .join(&e.name)
                .display()
                .to_string(),
        )
    }

    /// 最终默认三级：选中接管 > 图库首个集内项 > 内嵌字节（永不回 None，不断背景）。
    pub(crate) fn default_source(
        entries: &[crate::gallery::GalleryEntry],
        selected: Option<usize>,
        dir: &str,
    ) -> BgSource {
        match Self::resolve_source(entries, selected, dir) {
            Some(p) => BgSource::File(p),
            None => BgSource::Embedded,
        }
    }

    /// 切源（App 每帧调；变化才重建 worker；旧 worker 随 rx 丢弃 EPIPE 退出，
    /// 在途帧不污染新源因为通道按 worker 独立）。
    /// 新源清掉旧失败毒丸（`fallback`）与旧末帧（`front`）。
    pub(crate) fn set_source(&mut self, want: Option<BgSource>) {
        if self.source != want {
            self.source = want;
            self.rx = None;
            self.started = false;
            self.fallback = None;
            self.front = None;
            self.playing = false;
            self.next_frame_at = None;
        }
    }

    /// 实时节拍：elapsed 超 wall 才睡（纯规则，可测）。
    pub(crate) fn pace_sleep_ms(elapsed_secs: f64, wall_secs: f64) -> u64 {
        if elapsed_secs > wall_secs {
            ((elapsed_secs - wall_secs) * 1000.0).round() as u64
        } else {
            0
        }
    }

    /// 显示节拍（秒）：min(源 fps, 15) 取倒数；纯规则，可测。
    pub(crate) fn display_period_secs(fps: u32) -> f64 {
        1.0 / fps.clamp(1, 120).min(DISPLAY_FPS_MAX) as f64
    }

    /// NV12 单像素 → RGBA（BT.601 全范围浮点口径，与 WGSL `fs` 同式；
    /// 仅单测/口径文档用，生产转换在 GPU，CPU 不走此函数）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn nv12_pixel_to_rgba(y: u8, u: u8, v: u8) -> [u8; 4] {
        let yf = y as f32 / 255.0;
        let uf = u as f32 / 255.0 - 0.5;
        let vf = v as f32 / 255.0 - 0.5;
        let r = (yf + 1.402 * vf).clamp(0.0, 1.0);
        let g = (yf - 0.344136 * uf - 0.714136 * vf).clamp(0.0, 1.0);
        let b = (yf + 1.772 * uf).clamp(0.0, 1.0);
        [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
            255,
        ]
    }

    /// 每帧推进（闭包外调用）：首帧起线程，draining 队列，定速 GPU 转换。
    /// 返回当前是否有视频可画（调用方据此选画视频还是静态底）。
    pub(crate) fn tick(&mut self, ctx: &egui::Context) -> bool {
        if self.fallback.is_some() {
            self.playing = false;
            return false;
        }
        if !self.started {
            self.started = true;
            let Some(src) = self.source.clone() else {
                self.fallback = Some(String::from("video bg: no source, fallback solid"));
                self.playing = false;
                return false;
            };
            let (tx, rx) = sync_channel::<BgMsg>(QUEUE_DEPTH);
            self.rx = Some(rx);
            std::thread::Builder::new()
                .name(String::from("video-bg"))
                .spawn(move || match src {
                    BgSource::File(p)
                        if Self::should_try_start(std::path::Path::new(&p).exists()) =>
                    {
                        decode_loop(&p, tx)
                    }
                    BgSource::File(p) => {
                        let _ = tx.send(BgMsg::Finished(Err(format!(
                            "video bg: no file {p}, fallback solid"
                        ))));
                    }
                    BgSource::Embedded => decode_bytes(EMBEDDED_BG.to_vec(), tx),
                })
                .ok();
        }
        // 非阻塞收完本帧到达的全部消息（只留最新帧，旧帧丢弃跟上节拍）。
        let mut latest: Option<(u32, u32, Vec<u8>, Vec<u8>)> = None;
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
                    Ok(BgMsg::FrameNv12 { w, h, y, uv }) => {
                        latest = Some((w, h, y, uv));
                    }
                    Ok(BgMsg::Finished(Ok(()))) => {}
                    Ok(BgMsg::Finished(Err(e))) => {
                        finished_err = Some(e);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        if let Some(e) = finished_err {
            self.fallback = Some(e);
            self.playing = false;
            return false;
        }
        // 失焦停推：不推进、不重绘（eframe 有输入事件自会重绘； twin 保留末帧）。
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if !focused {
            self.playing = self.front.is_some();
            return self.playing;
        }
        // 定速消费：按显示封顶节拍，超前则本帧不换图（背压自然形成）。
        if let Some((w, h, y, uv)) = latest {
            let now = Instant::now();
            let due = match self.next_frame_at {
                None => true,
                Some(t) => now >= t,
            };
            if due {
                let period = Duration::from_secs_f64(Self::display_period_secs(self.ready_fps));
                self.next_frame_at = Some(now + period);
                if self.gpu.is_none() {
                    self.gpu = self.rs.as_ref().and_then(|rs| GpuYuv::new(rs, w, h));
                    if self.gpu.is_none() {
                        self.fallback = Some(String::from("video bg: gpu alloc failed, fallback"));
                        self.playing = false;
                        return false;
                    }
                }
                // 几何变化（理论上单文件恒定）则重建，不炸不断。
                if let Some(g) = self.gpu.as_ref() {
                    if g.w != w || g.h != h {
                        let rs = self.rs.clone();
                        self.free_gpu();
                        self.gpu = rs.as_ref().and_then(|rs| GpuYuv::new(rs, w, h));
                        if self.gpu.is_none() {
                            self.fallback = Some(String::from("video bg: gpu realloc failed"));
                            self.playing = false;
                            return false;
                        }
                    }
                }
                if let Some(g) = self.gpu.as_mut() {
                    match g.upload_convert(&y, &uv) {
                        Some(id) => {
                            self.front = Some((id, w as f32, h as f32));
                            self.playing = true;
                        }
                        None => {
                            // 坏帧跳过（装饰不 halt；连续坏才由超时体现）。
                            self.playing = self.front.is_some();
                        }
                    }
                }
            } else {
                self.playing = self.front.is_some();
            }
        }
        if self.front.is_some() {
            // 按显示节拍唤醒（与片源同帧率）：无条件 request_repaint 会把 eframe
            // 顶到显示器刷新率空转——UI 整树 + twin 离屏 + mipgen 每秒跑满 60 次，
            // 而视频只换 15 帧，这是播放时 +20% CPU 的大头（实测）。
            // request_repaint_after 让整条管线跟视频同频，不超前空转。
            let now = Instant::now();
            match self.next_frame_at {
                Some(t) if t > now => ctx.request_repaint_after(t - now),
                _ => ctx.request_repaint(),
            }
            true
        } else {
            // 还没首帧：100ms 轮询等首帧，不 60fps 空转；画静态底。
            ctx.request_repaint_after(Duration::from_millis(100));
            false
        }
    }

    /// 释放 native id（几何重建/回退时； twin 侧同步释放，不断合成）。
    fn free_gpu(&mut self) {
        if let (Some(rs), Some(g)) = (self.rs.as_ref(), self.gpu.take()) {
            for s in &g.pool {
                egui_glass::free_native_texture_id(rs, &s.id);
            }
        }
        self.front = None;
    }
}

mod gpu;
#[allow(unused_imports)]
pub(crate) use gpu::*;
mod decode;
#[allow(unused_imports)]
pub(crate) use decode::*;

#[cfg(test)]
mod video_bg_tests {
    use super::*;

    #[test]
    fn pitch_pads_to_256() {
        assert_eq!(GpuYuv::padded_pitch(320), 512);
        assert_eq!(GpuYuv::padded_pitch(1280), 1280);
        assert_eq!(GpuYuv::padded_pitch(1920), 2048);
        assert_eq!(GpuYuv::padded_pitch(180), 256);
    }

    #[test]
    fn pace_sleep_is_realtime() {
        assert_eq!(VideoBg::pace_sleep_ms(1.0, 0.2), 800);
        assert_eq!(VideoBg::pace_sleep_ms(1.0, 1.0), 0);
        assert_eq!(VideoBg::pace_sleep_ms(0.5, 1.0), 0);
    }

    #[test]
    fn full_file_loops_without_window() {
        // 整片循环：无预算无窗口函数，纯靠播完回绕（解码循环内证）。
        // 此处只锁死实时节拍仍在（逐帧按 pts 睡，不赶帧）。
        assert_eq!(VideoBg::pace_sleep_ms(2.0, 1.0), 1000);
        assert_eq!(VideoBg::pace_sleep_ms(1.0, 2.0), 0);
    }

    #[test]
    fn reorder_window_emits_display_order() {
        // 解码序 P0,P4,B1,B2,B3（High 档连 B）→ 显示序 0,1,2,3,4；空窗 None。
        assert_eq!(pop_earliest(&mut Vec::<(i128, u8)>::new()), None);
        let mut w: Vec<(i128, u8)> = vec![];
        let mut out = Vec::new();
        for (pts, v) in [(0, 0u8), (4, 4), (1, 1), (2, 2), (3, 3)] {
            w.push((pts, v));
            while w.len() > 2 {
                out.push(pop_earliest(&mut w).unwrap());
            }
        }
        while let Some(p) = pop_earliest(&mut w) {
            out.push(p);
        }
        let seq: Vec<u8> = out.into_iter().map(|(_, v)| v).collect();
        assert_eq!(seq, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn start_gate_is_file_exists() {
        assert!(VideoBg::should_try_start(true));
        assert!(!VideoBg::should_try_start(false));
    }

    #[test]
    fn display_follows_source_up_to_60fps() {
        assert!((VideoBg::display_period_secs(60) - 1.0 / 60.0).abs() < 1e-9);
        assert!((VideoBg::display_period_secs(120) - 1.0 / 60.0).abs() < 1e-9);
        assert!((VideoBg::display_period_secs(30) - 1.0 / 30.0).abs() < 1e-9);
        assert!((VideoBg::display_period_secs(10) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn queue_depth_bounds_memory() {
        // NV12 720p 单帧约 1.4MB，4 帧背压约 5.6MB（旧 RGBA 路 4 帧约 14MB）。
        assert_eq!(QUEUE_DEPTH, 4);
        assert_eq!(1280 * 720 * 3 / 2, 1_382_400);
    }

    #[test]
    fn nv12_formula_spot_check() {
        // 黑/白/中灰（BT.601 全范围，与 WGSL 同式，容差 ±2）。
        let b = VideoBg::nv12_pixel_to_rgba(0, 128, 128);
        assert!(b[0] <= 2 && b[1] <= 2 && b[2] <= 2);
        let w = VideoBg::nv12_pixel_to_rgba(255, 128, 128);
        assert!(w[0] >= 253 && w[1] >= 253 && w[2] >= 253);
        let g = VideoBg::nv12_pixel_to_rgba(128, 128, 128);
        assert!((g[0] as i16 - 128).abs() <= 2);
        // 纯红近似（Y≈76, U≈84, V≈255 量级）：R 明显最大。
        let r = VideoBg::nv12_pixel_to_rgba(76, 84, 255);
        assert!(r[0] > 200 && r[0] > r[1] + 100 && r[0] > r[2] + 100);
    }

    fn bg_entry(name: &str, codec: Option<&'static str>) -> crate::gallery::GalleryEntry {
        crate::gallery::GalleryEntry {
            name: name.to_string(),
            size: 100,
            kind: "mp4",
            codec,
            meta: None,
            mtime: 0,
        }
    }

    #[test]
    fn selection_takes_over_only_for_avc_hevc() {
        let entries = vec![
            bg_entry("1080p/a.mp4", Some("avc")),
            bg_entry("1080p/h.mp4", Some("hevc")),
            bg_entry("1080p/v.mp4", Some("vp9")),
            bg_entry("wallpaper/w.qois", None),
        ];
        assert_eq!(
            VideoBg::resolve_source(&entries, Some(0), "/usb"),
            Some(String::from("/usb/1080p/a.mp4"))
        );
        // hevc 在 mac 接管，Win/Linux 回落图库首个 avc（轮子无 HEVC 解码器）。
        let h = VideoBg::resolve_source(&entries, Some(1), "/usb");
        #[cfg(target_os = "macos")]
        assert_eq!(h, Some(String::from("/usb/1080p/h.mp4")));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(h, Some(String::from("/usb/1080p/a.mp4")));
        // vp9/无探针/越界/未选一律回落图库首个 avc（默认跟随，不硬编码）。
        assert_eq!(
            VideoBg::resolve_source(&entries, Some(2), "/usb"),
            Some(String::from("/usb/1080p/a.mp4"))
        );
        assert_eq!(
            VideoBg::resolve_source(&entries, Some(3), "/usb"),
            Some(String::from("/usb/1080p/a.mp4"))
        );
        assert_eq!(
            VideoBg::resolve_source(&entries, Some(9), "/usb"),
            Some(String::from("/usb/1080p/a.mp4"))
        );
        assert_eq!(
            VideoBg::resolve_source(&entries, None, "/usb"),
            Some(String::from("/usb/1080p/a.mp4"))
        );
        // 空图库/无集内项即 None（→ 静态底）。
        assert_eq!(VideoBg::resolve_source(&[], Some(0), "/usb"), None);
        assert_eq!(VideoBg::resolve_source(&[], None, "/usb"), None);
        let noavc = vec![
            bg_entry("1080p/v.mp4", Some("vp9")),
            bg_entry("wallpaper/w.qois", None),
        ];
        assert_eq!(VideoBg::resolve_source(&noavc, None, "/usb"), None);
    }

    #[test]
    fn set_source_restarts_worker_and_clears_poison() {
        let mut bg = VideoBg::new(None);
        assert_eq!(bg.source, None);
        // 同值不变（started 保持，worker 不重建）。
        bg.started = true;
        bg.set_source(None);
        assert!(bg.started);
        // 换源：杀 worker 状态 + 清失败毒丸与旧末帧。
        bg.fallback = Some(String::from("boom"));
        bg.front = Some((egui::TextureId::default(), 1.0, 1.0));
        bg.set_source(Some(BgSource::File(String::from("/usb/1080p/a.mp4"))));
        assert_eq!(
            bg.source,
            Some(BgSource::File(String::from("/usb/1080p/a.mp4")))
        );
        assert!(!bg.started);
        assert_eq!(bg.fallback, None);
        assert_eq!(bg.front, None);
        assert!(!bg.playing);
    }

    #[test]
    fn default_source_never_none() {
        // 三级默认：选中接管 > 图库首个 > 内嵌字节（永不断背景）。
        let entries = vec![
            bg_entry("1080p/v.mp4", Some("vp9")),
            bg_entry("1080p/a.mp4", Some("avc")),
        ];
        assert_eq!(
            VideoBg::default_source(&entries, Some(0), "/usb"),
            BgSource::File(String::from("/usb/1080p/a.mp4"))
        );
        assert_eq!(
            VideoBg::default_source(&[], None, "/usb"),
            BgSource::Embedded
        );
        // 内嵌字节非空（assets/dsxhgm.mp4 打进二进制）。
        assert!(!EMBEDDED_BG.is_empty());
    }

    #[test]
    fn hevc_param_sets_split_by_nal_type() {
        // VPS(32)/SPS(33)/PPS(34) + 一个 IDR(19) 干扰项。
        let mut prefix = vec![0, 0, 0, 1, 0x40, 1, 2];
        prefix.extend_from_slice(&[0, 0, 0, 1, 0x42, 3, 4, 5]);
        prefix.extend_from_slice(&[0, 0, 1, 0x26, 9, 9]);
        prefix.extend_from_slice(&[0, 0, 0, 1, 0x44, 6]);
        let (v, sp, pp) = split_hevc_param_sets(&prefix).expect("splits");
        assert_eq!(v, vec![0x40, 1, 2]);
        assert_eq!(sp, vec![0x42, 3, 4, 5]);
        assert_eq!(pp, vec![0x44, 6]);
        // 缺 PPS 即 None；空输入即 None。
        let mut short = vec![0, 0, 0, 1, 0x40, 1];
        short.extend_from_slice(&[0, 0, 0, 1, 0x42, 3]);
        assert!(split_hevc_param_sets(&short).is_none());
        assert!(split_hevc_param_sets(&[]).is_none());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn split_nv12_roundtrip() {
        let (w, h) = (64u32, 32u32);
        let mut f = vec![0u8; w as usize * h as usize * 3 / 2];
        for (i, b) in f.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let (sw, sh, y, uv) = split_nv12(&f, w, h).expect("splits");
        assert_eq!((sw, sh), (w, h));
        assert_eq!(y.len(), w as usize * h as usize);
        assert_eq!(uv.len(), w as usize * h as usize / 2);
        assert!(split_nv12(&f[..10], w, h).is_none());
        assert!(split_nv12(&f, 63, h).is_none());
    }
}
