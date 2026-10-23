//! `video::play`：播放列表加载与播放目标裁决（`[playlist] loop/items` +
//! `[play] fps/feed` + `[decode.*]` 矩阵）。
//! `need_eager_clip` 决全量/懒形态（dynamic 恒 eager，mp 恒 eager 但 slots 例外走 lazy，
//! 壁纸恒 eager 见 `feed_for`）；
//! `detect_clip_format`/`detect_clip_format_tagged` 按路径自动检测八格式（扩展名 +
//! `.mp4` 容器探针 hevc>avc>vp9；tagged 版探针 tag 由调用方透传，旧包装恒 `"playlist"`）；
//! `first_playable_clip` 取首个可播项（逐项检测，失败跳项 + warn；空列表/全坏 Err）；
//! `play_clip` 起播管线尾巴（`video_run` 与 `play_path` 共用：限速裁决 + 三路分发）；
//! `play_path` 按显式路径单播（TUI 播放页用：自动检测 + 加载 + `play_clip`）；
//! `resolve_play_target`/`measure_max_fps` 决限速目标（fps 政策 + vsync 封顶，逐项不变）。
//!
//! 新串口标记（矩阵卡点用，旧行逐字保留）：
//! - `playlist: items={n} loop={mode}`（`config::load` 启动记一笔）；
//! - `playlist: item {i}/{n} path={path} fmt={:?} feed={}`（每项加载记一笔，i 从 1 起）；
//! - `playlist: skip {path} ({reason})`（检测/打开/解码失败跳项，不断播）；
//! - `playlist: empty list, nothing to play`（空列表，播片直接 Err）；
//!
//! 旧行 `video: fmt=` / `video: feed=`（+`fetchovr=`）/ `decode: fmt=` / `play: path=` 逐字保留。

use super::decoder::Clip;
use super::demux::{Mp4VideoKind, demux_mp4_track};
use super::{VIDEO_NATIVE_FPS, rdtsc};
use crate::config::{self, MediaFormat};
use crate::error::{NyaStatus, Result};
use crate::fs::Fs;
use crate::graphics::Screen;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub(crate) fn need_eager_clip() -> bool {
    // mp/dynamic 恒全量前视（行区切分）；其余懒/全量按 feed 走 clip 路。
    // wallpaper 恒 eager 由 `feed_for` tag 裁决（此处不分支）。
    // single 零缓存：eager 会一次全解（正确但多内存），mp/dynamic 下仍 eager（矩阵要求）。
    // slots 例外（manual direct+all + qois/raw + 零拷系供片 + roles 准分）：
    // 走 lazy 保留编码源，
    // AP 逐核真解码（见 `mp_slots`）；dynamic 后端恒 true（`dynamic_draw` 仍吃 eager）。
    match config::schedule_backend() {
        config::ScheduleBackend::Dynamic => true,
        config::ScheduleBackend::Nano => false,
        config::ScheduleBackend::Manual => {
            let direct_all = config::effective_render() == config::RenderMode::Direct
                && config::effective_workers() == config::WorkersMode::All;
            // 槽路意向（含 roles 门：显式 `combined`/单核预算直接 eager；其余走
            // 乐观 lazy，分发期 `use_frame_slots` 复核 + `ensure_eager` 兜底，
            // 语义与旧版一致）。
            if direct_all && config::mp_slots_wanted() {
                return false;
            }
            direct_all
        }
    }
}

/// 路径转 UEFI 打开路径（斜杠统一为反斜杠；`playlist`/`wallpaper` 条目共用）。
pub(crate) fn path_cstr16(path: &str) -> Result<uefi::CString16> {
    let fixed = path.replace('/', "\\");
    uefi::CString16::try_from(fixed.as_str()).map_err(|_| NyaStatus::FromStrWithBufError)
}

/// raw 项的同胞目录（`frameNNNN.raw` 所在父目录，反斜杠形；
/// 裸文件名（无目录部分）按 media 口径落在媒体根：media_dir 非空即其自身，否则根 `""`
///（join 后为 `\name` 根绝对形；旧写死 `"1080p"` 的替代）。
fn raw_parent_dir(path: &str) -> String {
    match path.rsplit_once(['/', '\\']) {
        Some((dir, _)) if !dir.is_empty() => dir.replace('/', "\\"),
        _ => config::media_dir().replace('/', "\\"),
    }
}

/// 整文件读出（容器探针用；失败回 None，调用方记 skip）。
fn read_path_bytes(path: &str) -> Option<Vec<u8>> {
    let mut fs = Fs::new().ok()?;
    let c = path_cstr16(path).ok()?;
    fs.read_file(&c).ok()
}

/// 片源自动检测（播放列表/画廊共用），按 SPEC 顺序：
/// 1. 小写路径以 `.qois` 结尾 → qois；
/// 2. 小写路径以 `.raws` 结尾 → raw（单文件 BGRA 序列，`frame_bytes` 整切）；
/// 3. 文件名匹配 `frameNNNN.raw` 模式 → raw（目录扫描）；
/// 4. 小写路径以 `.mp4` 结尾 → 容器探针（demux 视频轨，hevc > avc > vp9 首中者胜，
///    无匹配轨即 None，不断言静默回退）；
/// 5. `.hevc`/`.265` → hevc；6. `.ivf` → vp9；7. `.h263` → h263；
/// 8. `.m2v` → mpeg2v；9. `.ogv` → theora；
///
/// 其余一律 None（调用方记 `playlist: skip` + warn，不断播）。
/// 片源自动检测（旧包装：tag 恒 `"playlist"`，`wallpaper.rs` 调用点不动）。
pub fn detect_clip_format(path: &str) -> Option<MediaFormat> {
    detect_clip_format_tagged(path, "playlist")
}

/// 片源自动检测（调用方透传 `tag`：容器探针串口行按此打标；行语义不变只是 tag 变对）。
/// 调用方：播放列表按加载 tag（`"video"`）、TUI 单路径（`"video"`）；壁纸沿用旧包装。
pub fn detect_clip_format_tagged(path: &str, tag: &str) -> Option<MediaFormat> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".qois") {
        return Some(MediaFormat::Qois);
    }
    if lower.ends_with(".raws") {
        return Some(MediaFormat::Raw);
    }
    if config::is_raw_frame_path(path) {
        return Some(MediaFormat::Raw);
    }
    if lower.ends_with(".mp4") {
        // 容器探针：整文件 demux，hevc > avc > vp9 顺序首中者胜；
        // 无匹配轨/坏容器/缺文件一律 None（调用方 skip + warn）。
        let bytes = read_path_bytes(path)?;
        for kind in [Mp4VideoKind::Hevc, Mp4VideoKind::Avc, Mp4VideoKind::Vp9] {
            if demux_mp4_track(&bytes, kind, tag).is_ok() {
                return Some(match kind {
                    Mp4VideoKind::Hevc => MediaFormat::Hevc,
                    Mp4VideoKind::Avc => MediaFormat::Avc,
                    Mp4VideoKind::Vp9 => MediaFormat::Vp9,
                });
            }
        }
        return None;
    }
    if lower.ends_with(".hevc") || lower.ends_with(".265") {
        return Some(MediaFormat::Hevc);
    }
    if lower.ends_with(".ivf") {
        return Some(MediaFormat::Vp9);
    }
    if lower.ends_with(".h263") {
        return Some(MediaFormat::H263);
    }
    if lower.ends_with(".m2v") {
        return Some(MediaFormat::Mpeg2v);
    }
    if lower.ends_with(".ogv") {
        return Some(MediaFormat::Theora);
    }
    None
}

/// 单项加载（`tag` 只进串口：`video` / `wallpaper`；壁纸恒 eager 由调用方传 true，
/// `feed_for` 以 tag 裁决忽略每格式 fetch 覆盖）。
/// 扩展名定读写路（mp4 走容器，裸后缀走裸流，raw 扫同胞目录）；
/// `eager` 为真无视 `feed` 强制全量（矩阵需全前视路径用）。
///
/// 栈纪律（血泪：2026-10-22 三重故障复位根因）：固件栈在 load 点仅剩 ~20KB，
/// `Source` 枚举（最大变体 vp9 ≈ 8.6KB）与各 vendor `Decoder::new()` 暂存
/// （vp9 8.5KB / avc 5KB）绝不能以值驻留在 load 调用链上——构造一律经
/// `Source::make_*`（`#[inline(never)]`，暂存死在 helper 帧里）+ 解码器堆装箱
/// （`Box<Decoder>`，枚举常驻 <300B）。QEMU 实测：`video-decode-fetch-override`
/// （lazy hevc）/ `video-decode-avc`（eager avc）在修之前静默复位（串口无 panic、
/// `-d int` 见 `#PF→#DF→三重故障`，RSP 触栈底 `0x3FE00000`）。
pub(crate) fn load_item_clip(
    fs: &mut Fs,
    path: &str,
    fmt: MediaFormat,
    tag: &str,
    eager: bool,
) -> Result<Clip> {
    let lower = path.to_ascii_lowercase();
    let is_mp4 = lower.ends_with(".mp4");
    let mut clip = match fmt {
        MediaFormat::Qois => {
            let c = path_cstr16(path)?;
            Clip::from_qois_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::Raw if lower.ends_with(".raws") => {
            let c = path_cstr16(path)?;
            Clip::from_raws_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::Raw => {
            let dir = raw_parent_dir(path);
            // RAW-GEOM：帧字节按 `[decode.raw]` 生效几何（缺键 1920x1080x4 照旧）。
            let (rw, rh) = crate::config::decode_raw_wh();
            Clip::from_raw_dir(fs, &dir, usize::MAX, rw * rh * 4, tag)?
        }
        MediaFormat::Hevc if is_mp4 => {
            let c = path_cstr16(path)?;
            Clip::from_mp4_file(fs.open_file(&c)?, Mp4VideoKind::Hevc, tag)?
        }
        MediaFormat::Hevc => {
            let c = path_cstr16(path)?;
            Clip::from_hevc_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::Avc => {
            let c = path_cstr16(path)?;
            Clip::from_mp4_file(fs.open_file(&c)?, Mp4VideoKind::Avc, tag)?
        }
        MediaFormat::Vp9 if is_mp4 => {
            let c = path_cstr16(path)?;
            Clip::from_mp4_file(fs.open_file(&c)?, Mp4VideoKind::Vp9, tag)?
        }
        MediaFormat::Vp9 => {
            let c = path_cstr16(path)?;
            Clip::from_vp9_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::H263 => {
            let c = path_cstr16(path)?;
            Clip::from_h263_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::Mpeg2v => {
            let c = path_cstr16(path)?;
            Clip::from_mpeg2v_file(fs.open_file(&c)?, tag)?
        }
        MediaFormat::Theora => {
            let c = path_cstr16(path)?;
            Clip::from_theora_file(fs.open_file(&c)?, tag)?
        }
    };
    if eager {
        clip.ensure_eager()?;
    }
    Ok(clip)
}

/// 首个可播项扫描（`video_run`/`measure_max_fps` 用）：逐项自动检测（探针 tag `"video"`），
/// 失败跳项 + warn，不断播；空列表 / 全坏直接 Err（无静默换源）。无游标：
/// 单播语义，播完调用方循环本片，不步进列表。`eager` 为真无视 `feed` 强制全量。
pub(crate) fn first_playable_clip(fs: &mut Fs, eager: bool) -> Result<Clip> {
    let total = config::playlist_len();
    if total == 0 {
        log::warn!("playlist: empty list, nothing to play");
        return Err(NyaStatus::_Debug(String::from("playlist: empty list")));
    }
    for i in 0..total {
        let path = match config::playlist_path(i) {
            Some(p) => p,
            None => continue,
        };
        let fmt = match detect_clip_format_tagged(&path, "video") {
            Some(f) => f,
            None => {
                log::warn!("playlist: skip {} (unknown format or probe failed)", path);
                continue;
            }
        };
        let clip = match load_item_clip(fs, &path, fmt, "video", eager) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("playlist: skip {} ({:?})", path, e);
                continue;
            }
        };
        // 矩阵靠它断言逐项分发（`playlist: item` + `video: fmt=`/`feed=`/`decode:` 行逐字保留）。
        log::info!(
            "playlist: item {}/{} path={} fmt={:?} feed={}",
            i + 1,
            total,
            path,
            fmt,
            config::effective_feed(fmt).as_str(),
        );
        log::info!(
            "video: fmt={:?} frames={} {}x{}",
            fmt,
            clip.len(),
            clip.w,
            clip.h
        );
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
        return Ok(clip);
    }
    Err(NyaStatus::_Debug(String::from(
        "playlist: no playable item",
    )))
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
/// `[display]`：源几何取 `Clip::w/h`（旧 `VIDEO_W/H` 常量 1080p 恒等）；不记
/// `display:` marker（bench 非起播）。
pub(crate) fn measure_max_fps(screen: &mut Screen, tps: u64) -> Result<u32> {
    screen.draw_str("play: benching max playback fps (one fast pass)...");
    let mut fs = Fs::new()?;
    let mut video = first_playable_clip(&mut fs, false)?;
    let (sw, sh) = (video.w, video.h);
    let frames = video.len().max(1);
    let t0 = rdtsc();
    // 实播计数（懒模式 `len` 是分块上界，h263 甚至未知：以播出帧为准，
    // 全量下与旧值一致；`None` 截断）。
    let mut shown = 0u64;
    for _ in 0..frames {
        match video.next_frame() {
            Some(f) => {
                screen.upload_frame(sw, sh, f)?;
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

/// 起播管线尾巴（`video_run` 与 `play_path` 共用）：限速裁决 + 按 `schedule.backend`
/// 分发三路（nano 单核泵 / dynamic 多核泵 / manual 手写调度，零改）。
/// 调用方已备好单片 `Clip`（来源：播放列表首个可播项 / TUI 显式路径）；
/// 播完循环本片，不步进列表。
pub(crate) fn play_clip(screen: &mut Screen, clip: Clip) -> Result {
    let tps = super::calibrate_tps();
    let target = resolve_play_target(screen, tps)?;
    let (cw, ch) = (clip.w, clip.h);
    match crate::config::schedule_backend() {
        crate::config::ScheduleBackend::Nano => {
            log::info!(
                "play: path=nano_pump (single executor task; schedule workers/dist ignored)"
            );
            super::exec::nano_pump(screen, clip, cw, ch, target, tps)
        }
        crate::config::ScheduleBackend::Dynamic => {
            super::exec::dynamic_draw(screen, clip, cw, ch, target, tps)
        }
        crate::config::ScheduleBackend::Manual => super::manual_run(screen, clip, target, tps),
    }
}

/// TUI 播放页单路径起播（`screen` 值进值出，供页栈流转）：
/// 按 `path` 自动检测（探针 tag `"video"`）+ 加载 `Clip` + `play_clip` 管线。
/// `Err` 时 `screen` 已随所有权释放（加载期不需要屏），调用方需 `Screen::reacquire()` 接管。
pub(crate) fn play_path(mut screen: Screen, path: &str) -> Result<Screen> {
    let fmt = match detect_clip_format_tagged(path, "video") {
        Some(f) => f,
        None => {
            log::warn!("play: skip {} (unknown format or probe failed)", path);
            return Err(NyaStatus::_Debug(format!("play: skip {}", path)));
        }
    };
    let mut fs = Fs::new()?;
    let clip = load_item_clip(&mut fs, path, fmt, "video", need_eager_clip())?;
    log::info!(
        "play: path={} fmt={:?} frames={} {}x{}",
        path,
        fmt,
        clip.len(),
        clip.w,
        clip.h
    );
    play_clip(&mut screen, clip)?;
    Ok(screen)
}
