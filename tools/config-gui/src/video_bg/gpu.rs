//! 动态背景 GPU 侧：NV12→RGBA WGSL 转换 + 双槽显示池（拆自 `video_bg.rs`，行为零改）。

use super::*;

/// GPU 侧 NV12→RGBA 转换器 + 双槽显示池（CPU 只 `write_texture`，数学全在 WGSL）。
pub(crate) struct GpuYuv {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pub(crate) w: u32,
    pub(crate) h: u32,
    y_tex: wgpu::Texture,
    uv_tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    pipe: wgpu::RenderPipeline,
    pub(crate) pool: [RgbaSlot; 2],
    back: usize,
}

pub(crate) struct RgbaSlot {
    view: wgpu::TextureView,
    pub(crate) id: egui::TextureId,
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
    pub(crate) fn new(rs: &egui_wgpu::RenderState, w: u32, h: u32) -> Option<Self> {
        // 几何门：偶数 + 上限 2048。`write_texture` 的 256 字节行距要求用填充行搞定
        // （见 `upload_convert` 内 `padded_pitch`），不再拒绝非 256 宽（320x180 等图库小片必须可播）。
        if w < 16 || h < 16 || w > 2048 || h > 2048 || !w.is_multiple_of(2) || !h.is_multiple_of(2)
        {
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

    /// 行距填充到 256 字节（`write_texture` 硬件要求）：纯规则，可单测。
    pub(crate) fn padded_pitch(w: u32) -> u32 {
        ((w + 255) / 256) * 256
    }

    /// 上传 NV12 + GPU 转换进后槽并换前（返回显示槽 native id；坏尺寸 None）。
    pub(crate) fn upload_convert(&mut self, y: &[u8], uv: &[u8]) -> Option<egui::TextureId> {
        let (w, h) = (self.w as usize, self.h as usize);
        if y.len() < w * h || uv.len() < w * h / 2 {
            return None;
        }
        // 256 字节行距填充：`write_texture` 要求 `bytes_per_row` 对齐，纹理宽保持原值即可。
        let pitch = Self::padded_pitch(self.w) as usize;
        let y_src: &[u8];
        let mut y_pad;
        if pitch == w {
            y_src = &y[..w * h];
        } else {
            y_pad = vec![0u8; pitch * h];
            for row in 0..h {
                y_pad[row * pitch..row * pitch + w].copy_from_slice(&y[row * w..row * w + w]);
            }
            y_src = &y_pad;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.y_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            y_src,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch as u32),
                rows_per_image: Some(self.h),
            },
            wgpu::Extent3d {
                width: self.w,
                height: self.h,
                depth_or_array_layers: 1,
            },
        );
        let uv_src: &[u8];
        let mut uv_pad;
        if pitch == w {
            uv_src = &uv[..w * h / 2];
        } else {
            uv_pad = vec![0u8; pitch * h / 2];
            for row in 0..h / 2 {
                uv_pad[row * pitch..row * pitch + w].copy_from_slice(&uv[row * w..row * w + w]);
            }
            uv_src = &uv_pad;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.uv_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            uv_src,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch as u32),
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
