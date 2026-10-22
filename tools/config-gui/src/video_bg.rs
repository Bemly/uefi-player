//! 动态背景 GPU 转换版：硬解在 GPU，CPU 只搬运不算像素，玻璃在 live 内采样。
//!
//! 链路（`input.mp4` 前 40s 循环，缺片/失败静默回静态底，不断任何流程）：
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
//! 内存纪律：40s 循环 + `sync_channel(4)` 背压（NV12 约 5.6MB），UI 按
//! min(源 fps, 15) 定速消费，超窗即重建解码器回绕（干净 IDR，无跨轮参考）。

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use egui_wgpu::wgpu;

/// 片源（绝对路径，用户拍板；不存在即静默回退，不报错挡流程）。
pub(crate) const VIDEO_PATH: &str = "/Users/bemly/Movies/input.mp4";
/// 循环窗（秒）：只播前 40s，到点即回绕。
pub(crate) const LOOP_SECS: f64 = 40.0;
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

    /// 循环预算帧数（40s × fps，原片 720p60 即 2400；纯规则，可测）。
    pub(crate) fn loop_budget_frames(fps: u32) -> u64 {
        (LOOP_SECS * fps.max(1) as f64).round() as u64
    }

    /// 是否该回绕（pts 秒数 ≥40 即绕；纯规则，可测）。
    pub(crate) fn should_loop(elapsed_secs: f64) -> bool {
        elapsed_secs >= LOOP_SECS
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

/// GPU 侧 NV12→RGBA 转换器 + 双槽显示池（CPU 只 `write_texture`，数学全在 WGSL）。
struct GpuYuv {
    device: wgpu::Device,
    queue: wgpu::Queue,
    w: u32,
    h: u32,
    y_tex: wgpu::Texture,
    uv_tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    pipe: wgpu::RenderPipeline,
    pool: [RgbaSlot; 2],
    back: usize,
}

struct RgbaSlot {
    view: wgpu::TextureView,
    id: egui::TextureId,
}

const YUV_WGSL: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var o: VsOut;
    o.pos = vec4<f32>(p[i], 0.0, 1.0);
    o.uv = p[i] * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    return o;
}
@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let y = textureSample(y_tex, smp, in.uv).r;
    let uv = textureSample(uv_tex, smp, in.uv).rg - vec2<f32>(0.5, 0.5);
    let r = y + 1.402 * uv.y;
    let g = y - 0.344136 * uv.x - 0.714136 * uv.y;
    let b = y + 1.772 * uv.x;
    return vec4<f32>(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}
"#;

impl GpuYuv {
    /// 建转换器 + 双槽池并登记 native id（必须在 `LiveBackdrop::new` 之后调，
    /// twin 自动镜像；尺寸非法/超限返回 None，调用方回静态底，不断流程）。
    fn new(rs: &egui_wgpu::RenderState, w: u32, h: u32) -> Option<Self> {
        // 几何门：偶数 + 上限 2048 + 行距 256 对齐（`write_texture` 硬件要求，
        // 1280x720 天然满足；不满足即回静态底，不炸不断）。
        if w < 16 || h < 16 || w > 2048 || h > 2048 || !w.is_multiple_of(2) || !h.is_multiple_of(2)
        {
            return None;
        }
        if !w.is_multiple_of(256) {
            return None;
        }
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        let y_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video-bg y"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uv_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video-bg uv"),
            size: wgpu::Extent3d {
                width: w / 2,
                height: h / 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let smp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video-bg smp"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video-bg bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-bg bind"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &y_tex.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &uv_tex.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&smp),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video-bg yuv"),
            source: wgpu::ShaderSource::Wgsl(YUV_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video-bg layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video-bg pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let mk_slot = |n: &str| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(n),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let id = egui_glass::register_native_texture(rs, &view);
            RgbaSlot { view, id }
        };
        let pool = [mk_slot("video-bg rgba0"), mk_slot("video-bg rgba1")];
        Some(Self {
            device,
            queue,
            w,
            h,
            y_tex,
            uv_tex,
            bind,
            pipe,
            pool,
            back: 0,
        })
    }

    /// 上传 NV12 + GPU 转换进后槽并换前（返回显示槽 native id；坏尺寸 None）。
    fn upload_convert(&mut self, y: &[u8], uv: &[u8]) -> Option<egui::TextureId> {
        let (w, h) = (self.w as usize, self.h as usize);
        if y.len() < w * h || uv.len() < w * h / 2 {
            return None;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.y_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &y[..w * h],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.w),
                rows_per_image: Some(self.h),
            },
            wgpu::Extent3d {
                width: self.w,
                height: self.h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.uv_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &uv[..w * h / 2],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.w),
                rows_per_image: Some(self.h / 2),
            },
            wgpu::Extent3d {
                width: self.w / 2,
                height: self.h / 2,
                depth_or_array_layers: 1,
            },
        );
        let back = self.back;
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("video-bg convert"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video-bg convert"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.pool[back].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipe);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.back = 1 - back;
        Some(self.pool[back].id)
    }
}

/// 后台解码循环（worker 线程；阻塞式，背压由 `sync_channel` 承担）。
/// 只传 NV12 平面（无 BT.601、无 RGBA，CPU 零像素运算）；失败只发一次
/// `Finished(Err)`，UI 静默回退（用户拍板，不弹窗）。
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
                        if let Some((w, h, y, uv)) = frame_to_nv12(&frame) {
                            // 背压：UI 跟不上即阻塞在此，不堆内存。
                            if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
                                return;
                            }
                            sent += 1;
                        }
                        // `None`（隐藏帧/重排等待）不计帧，继续喂。
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // 坏帧跳过（装饰不 halt）。
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

/// VideoToolbox 解码器构造（SPS/PPS/length_size 直给会话；NV12 输出，CPU 只搬运）。
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
        pixel_format: PixelFormat::Nv12,
    })
    .map_err(|e| format!("video bg decoder: {e:?}"))
}

/// Nv12 帧（stride 可能 > 宽）→ 紧凑 Y + 交错 UV（纯搬运，无像素运算）。
/// 空平面（异常哨兵，见上游文档）即跳过该帧（返回 None，不断流）。
#[cfg(target_os = "macos")]
fn frame_to_nv12(
    frame: &shiguredo_video_toolbox::DecodedFrame<'_>,
) -> Option<(u32, u32, Vec<u8>, Vec<u8>)> {
    match frame {
        shiguredo_video_toolbox::DecodedFrame::Nv12(f) => {
            let (w, h) = (f.width(), f.height());
            if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
                return None;
            }
            let (yp, uvp) = (f.y_plane(), f.uv_plane());
            if yp.is_empty() || uvp.is_empty() {
                return None;
            }
            let (ys, uvs) = (f.y_stride(), f.uv_stride());
            let y = pack_plane(yp, ys, w, h)?;
            let uv = pack_plane(uvp, uvs, w, h / 2)?;
            Some((w as u32, h as u32, y, uv))
        }
        shiguredo_video_toolbox::DecodedFrame::I420(_) => {
            // 本背景只开 Nv12 会话，走不到此分支（穷尽匹配保留）。
            None
        }
    }
}

/// Win/Linux 硬解循环（Vulkan Video H.264，`gpu-video` `BytesDecoder` NV12 回读）。
/// 只拆 Y/UV（memcpy），不做 BT.601；任一步硬解不可用即 `Finished(Err)`
/// 静默回退，绝不回退 CPU 软解（用户拍板：GUI 内零软解）。
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
                            if let Some((w, h, y, uv)) =
                                split_nv12(&f.data.frame, f.data.width, f.data.height)
                            {
                                if tx.send(BgMsg::FrameNv12 { w, h, y, uv }).is_err() {
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
                        // 坏块跳过（装饰不 halt）。
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

/// 紧凑 NV12（Y 整幅 + UV 交错半幅）→ 拆 Y/UV（纯搬运； stride 即宽，无需打包）。
#[cfg(not(target_os = "macos"))]
fn split_nv12(frame: &[u8], w: u32, h: u32) -> Option<(u32, u32, Vec<u8>, Vec<u8>)> {
    if w < 16 || h < 16 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    if frame.len() < w * h * 3 / 2 {
        return None;
    }
    Some((
        w as u32,
        h as u32,
        frame[..w * h].to_vec(),
        frame[w * h..w * h * 3 / 2].to_vec(),
    ))
}

/// 带 stride 平面 → 紧凑行（逐行 memcpy，无像素运算；越界即 None，不断流）。
/// 仅 mac VideoToolbox 路用（Win/Linux 的 NV12 已紧凑），门控避他平台死码警告。
#[cfg(target_os = "macos")]
fn pack_plane(src: &[u8], stride: usize, w: usize, h: usize) -> Option<Vec<u8>> {
    if stride < w || src.len() < stride * h {
        return None;
    }
    if stride == w {
        return Some(src[..w * h].to_vec());
    }
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        out.extend_from_slice(&src[row * stride..row * stride + w]);
    }
    Some(out)
}

#[cfg(test)]
mod video_bg_tests {
    use super::*;

    #[test]
    fn loop_window_is_40s() {
        assert_eq!(VideoBg::loop_budget_frames(60), 2400);
        assert_eq!(VideoBg::loop_budget_frames(30), 1200);
        assert!(VideoBg::should_loop(40.0));
        assert!(!VideoBg::should_loop(39.9));
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
