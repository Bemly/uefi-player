//! `video::play`：片源加载与播放目标裁决（`[play] format/fps/feed` + `[decode.*]` 矩阵）。
//! `need_eager_clip` 决全量/懒形态（dynamic 恒 eager，mp 恒 eager 但 slots 例外走 lazy，
//! 壁纸恒 eager 见 `feed_for`）；
//! `load_video_clip` 八格式落同一 `Clip`；`resolve_play_target`/`measure_max_fps` 决限速目标。

use super::decoder::Clip;
use super::{VIDEO_H, VIDEO_NATIVE_FPS, VIDEO_W, rdtsc};
use crate::config::{self, MediaFormat};
use crate::error::Result;
use crate::fs::Fs;
use crate::graphics::Screen;
use alloc::format;
use uefi::cstr16;

pub(crate) fn need_eager_clip() -> bool {
    // mp/dynamic 恒 eager（全前视行区切分）；其余懒/全量按 feed 走 clip 路。
    // wallpaper 恒 eager 由 `feed_for` tag 裁决（此处不分支）。
    // single 零缓存：eager 会一次全解（正确但多内存），mp/dynamic 下仍 eager（矩阵要求）。
    // slots 例外（manual direct+all + qois/raw + 零拷系供片）：走 lazy 保留编码源，
    // AP 逐核真解码（见 `mp_slots`）；dynamic 后端恒 true（`dynamic_draw` 仍吃 eager）。
    match config::schedule_backend() {
        config::ScheduleBackend::Dynamic => true,
        config::ScheduleBackend::Nano => false,
        config::ScheduleBackend::Manual => {
            let direct_all = config::effective_render() == config::RenderMode::Direct
                && config::effective_workers() == config::WorkersMode::All;
            if direct_all && super::mp::mp_slots::slots_feed_eligible() {
                return false;
            }
            direct_all
        }
    }
}

/// 片源加载（`[play] format` 矩阵）：qois 读整文件预解码，raw 读序列目录，
/// hevc/avc/vp9 读 MP4 拆轨（缺容器回裸流老路），h263/mpeg2v/theora 读裸流直解；
/// 八格式全落到同一 `Clip`（`feed` 懒/全量形态见上），三条播放路零分支。
/// 容器优先裸流：avc/hevc/vp9 有 `video.mp4` 即拆轨（无匹配轨直接报错，
/// 不静默换源）；hevc/vp9 缺容器回裸流老路（`video.hevc`/`video.ivf`，
/// 老矩阵不断）；h263/mpeg2v/theora 只读裸流（`video.h263`/`video.m2v`/
/// `video.ogv`，OggS 是 theora 磁盘原格式，非 MP4 系容器）。
/// `eager` 为真时无视 `feed` 强制全量（矩阵需全前视路径用）；
/// `measure` 传假，边测边解。
pub(crate) fn load_video_clip(fs: &mut Fs, eager: bool) -> Result<Clip> {
    use crate::video::demux::Mp4VideoKind;
    let mut clip = match config::play_format() {
        MediaFormat::Qois => {
            let file = fs.open_file(cstr16!("1080p\\video.qois"))?;
            Clip::from_qois_file(file, "video")?
        }
        MediaFormat::Raw => {
            Clip::from_raw_dir(fs, "1080p", usize::MAX, VIDEO_W * VIDEO_H * 4, "video")?
        }
        MediaFormat::Avc => {
            let file = fs.open_file(cstr16!("1080p\\video.mp4"))?;
            Clip::from_mp4_file(file, Mp4VideoKind::Avc, "video")?
        }
        MediaFormat::Hevc => match fs.open_file(cstr16!("1080p\\video.mp4")) {
            Ok(file) => Clip::from_mp4_file(file, Mp4VideoKind::Hevc, "video")?,
            Err(_) => {
                let file = fs.open_file(cstr16!("1080p\\video.hevc"))?;
                Clip::from_hevc_file(file, "video")?
            }
        },
        MediaFormat::Vp9 => match fs.open_file(cstr16!("1080p\\video.mp4")) {
            Ok(file) => Clip::from_mp4_file(file, Mp4VideoKind::Vp9, "video")?,
            Err(_) => {
                let file = fs.open_file(cstr16!("1080p\\video.ivf"))?;
                Clip::from_vp9_file(file, "video")?
            }
        },
        MediaFormat::H263 => {
            let file = fs.open_file(cstr16!("1080p\\video.h263"))?;
            Clip::from_h263_file(file, "video")?
        }
        MediaFormat::Mpeg2v => {
            let file = fs.open_file(cstr16!("1080p\\video.m2v"))?;
            Clip::from_mpeg2v_file(file, "video")?
        }
        MediaFormat::Theora => {
            let file = fs.open_file(cstr16!("1080p\\video.ogv"))?;
            Clip::from_theora_file(file, "video")?
        }
    };
    if eager {
        clip.ensure_eager()?;
    }
    let fmt = config::play_format();
    // 矩阵靠它断言格式分发（`video-hevc`/`video-vp9` 的 `expect` 行）。
    log::info!(
        "video: fmt={:?} frames={} {}x{}",
        fmt,
        clip.len(),
        clip.w,
        clip.h
    );
    // 供片断言行（矩阵用，不断言画质）：有效 feed 档（每格式 fetch 覆盖全局后的值，
    // 配置文件拼写）+ 每格式生效窗（`decode.<fmt>.buffers` 显式 1-32 覆盖，
    // 0 跟随 `tune.window` 全局默认）+ 解码面回显（threads/zero_copy/streaming/buffers，
    // parse + fallback 断言用）。覆盖激活时追加 ` fetchovr=<fmt>` token（新子串，旧子串逐字保留）。
    {
        let eff = config::effective_feed(fmt);
        let window = config::decode_window(fmt);
        if config::decode_fetch(fmt).is_some() {
            let fkey = match fmt {
                MediaFormat::Qois => "qois",
                MediaFormat::Raw => "raw",
                MediaFormat::Hevc => "hevc",
                MediaFormat::Vp9 => "vp9",
                MediaFormat::H263 => "h263",
                MediaFormat::Mpeg2v => "mpeg2v",
                MediaFormat::Theora => "theora",
                MediaFormat::Avc => "avc",
            };
            log::info!(
                "video: feed={} window={} fetchovr={}",
                eff.as_str(),
                window,
                fkey,
            );
        } else {
            log::info!("video: feed={} window={}", eff.as_str(), window,);
        }
    }
    log::info!(
        "decode: fmt={:?} threads={} zero_copy={} streaming={} buffers={} window={}",
        fmt,
        config::decode_threads(fmt).as_str(),
        config::decode_zero_copy(fmt),
        config::decode_streaming(fmt),
        config::decode_buffers(fmt),
        config::decode_window(fmt),
    );
    Ok(clip)
}

/// 播放帧率决策：max 不限速；capped 直接用；auto 无实测就现测并回写，
/// 按 `min(视频帧率, 实测*3/4)` 跑。返回 `None` = 不限速。
/// 最后叠 vsync 开关：static / half / auto（探测开）按 `effective_vsync_fps()` 再封顶
///（软件帧预算，GOP 无硬件 vsync）。
pub(crate) fn resolve_play_target(screen: &mut Screen, tps: u64) -> Result<Option<u32>> {
    let base = match config::play_policy() {
        config::FpsPolicy::Max => None,
        config::FpsPolicy::Capped(n) => Some(n),
        config::FpsPolicy::Auto => {
            let m = config::measured_max_fps();
            let m = if m == 0 {
                let mm = measure_max_fps(screen, tps)?;
                config::persist_measured_max_fps(mm)?;
                mm
            } else {
                m
            };
            Some((m as u64 * 3 / 4).min(VIDEO_NATIVE_FPS as u64).max(1) as u32)
        }
    };
    if config::vsync_enabled() {
        let cap = config::effective_vsync_fps();
        Ok(Some(base.map(|t| t.min(cap)).unwrap_or(cap)))
    } else {
        Ok(base)
    }
}

/// 首启实测：用生效渲染路径整轮推一遍，算最大播放帧率。
/// 纯展示性快进（和开机 render bench 一样会刷屏），一次性的，结果回写配置。
/// 懒 Clip 下边测边解（`feed` 非 clip 时首启不一次全解，峰值内存小）；
/// 注意这里是单线程 `upload_frame` 推送：workers=all 的真实多核路只会更快，
/// 测出来是保守下限，auto 取 3/4 本来就有余量。
/// `tune.buffers > 0` 时走影子缓冲（与播放同路径，half 取半更准）；测完即扔。
pub(crate) fn measure_max_fps(screen: &mut Screen, tps: u64) -> Result<u32> {
    screen.draw_str("play: benching max playback fps (one fast pass)...");
    let mut fs = Fs::new()?;
    let mut video = load_video_clip(&mut fs, false)?;
    let frames = video.len().max(1);
    let t0 = rdtsc();
    // 实播计数（懒模式 `len` 是分块上界，h263 甚至未知：以播出帧为准，
    // 全量下与旧值一致；`None` 截断）。
    let mut shown = 0u64;
    for _ in 0..frames {
        match video.next_frame() {
            Some(f) => {
                screen.upload_frame(VIDEO_W, VIDEO_H, f)?;
                shown += 1;
            }
            None => break,
        }
    }
    // 影子用完即扔：`mp_draw` 另起自己的环，峰值只留 N 份。
    screen.release_shadows();
    let dt = rdtsc().saturating_sub(t0).max(1);
    let fps = ((shown.max(1) * tps) / dt).clamp(1, 10_000) as u32;
    screen.draw_str(&format!(
        "play: max {} fps, running at 3/4 (capped by video)",
        fps
    ));
    Ok(fps)
}
