//! `video::play`：播放列表加载与播放目标裁决（`[playlist] loop/items` +
//! `[play] fps/feed` + `[decode.*]` 矩阵）。
//! `need_eager_clip` 决全量/懒形态（dynamic 恒 eager，mp 恒 eager 但 slots 例外走 lazy，
//! 壁纸恒 eager 见 `feed_for`）；
//! `detect_clip_format` 按路径自动检测八格式（扩展名 + `.mp4` 容器探针 hevc>avc>vp9）；
//! `load_playlist_clip` 逐项加载（失败跳项 + warn，调用方推进；空列表 Err）；
//! `next_playlist_index` 按循环模式推进（single 停留 / list 回绕 / once 到头结束）；
//! `load_video_clip` 取首个可播项（三条播放路零改）；
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
use crate::config::{self, LoopMode, MediaFormat};
use crate::error::{NyaStatus, Result};
use crate::fs::Fs;
use crate::graphics::Screen;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

/// 最近一次成功加载的播放列表下标（`load_video_clip`/`load_playlist_clip` 写入，
/// `loops` 步进时读回；`u32::MAX` = 尚未加载，走旧 rewind 语义）。
static PLAY_CURSOR: AtomicU32 = AtomicU32::new(u32::MAX);

/// 当前剪辑在列表中的下标（加载后步进用；空列表/未加载回 None）。
pub(crate) fn playlist_cursor() -> Option<usize> {
    let v = PLAY_CURSOR.load(Ordering::Relaxed);
    if v == u32::MAX {
        None
    } else {
        Some(v as usize)
    }
}

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

/// raw 项的同胞目录（`frameNNNN.raw` 所在父目录，反斜杠形；裸文件名默认 `1080p`）。
fn raw_parent_dir(path: &str) -> String {
    match path.rsplit_once(['/', '\\']) {
        Some((dir, _)) if !dir.is_empty() => dir.replace('/', "\\"),
        _ => String::from("1080p"),
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
pub fn detect_clip_format(path: &str) -> Option<MediaFormat> {
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
            if demux_mp4_track(&bytes, kind, "playlist").is_ok() {
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

/// 播放列表示项加载（`index` 下标；失败跳项 + warn，调用方推进下标；
/// 空列表直接 Err）。成功记 `playlist: item` 行并更新游标。
pub fn load_playlist_clip(fs: &mut Fs, index: usize, eager: bool) -> Result<Clip> {
    let total = config::playlist_len();
    if total == 0 {
        log::warn!("playlist: empty list, nothing to play");
        return Err(NyaStatus::_Debug(String::from("playlist: empty list")));
    }
    let path = match config::playlist_path(index) {
        Some(p) => p,
        None => {
            log::warn!("playlist: skip index {} (out of range)", index);
            return Err(NyaStatus::_Debug(format!("playlist: bad index {}", index)));
        }
    };
    let fmt = match detect_clip_format(&path) {
        Some(f) => f,
        None => {
            log::warn!("playlist: skip {} (unknown format or probe failed)", path);
            return Err(NyaStatus::_Debug(format!("playlist: skip {}", path)));
        }
    };
    let clip = match load_item_clip(fs, &path, fmt, "video", eager) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("playlist: skip {} ({:?})", path, e);
            return Err(e);
        }
    };
    PLAY_CURSOR.store(index as u32, Ordering::Relaxed);
    // 矩阵靠它断言逐项分发（`playlist: item` + `video: fmt=` 行）。
    log::info!(
        "playlist: item {}/{} path={} fmt={:?} feed={}",
        index + 1,
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

/// 列表步进（调用方在条目播完调；`total == 0` 直接 None）：
/// `loop_single` 原地停留（调用方重载同项），`loop_list` 回绕，`once` 到头结束（None）。
pub fn next_playlist_index(cur: usize, total: usize, loop_mode: LoopMode) -> Option<usize> {
    if total == 0 {
        return None;
    }
    match loop_mode {
        LoopMode::LoopSingle => Some(cur),
        LoopMode::LoopList => Some((cur + 1) % total.max(1)),
        LoopMode::Once => {
            if cur + 1 < total {
                Some(cur + 1)
            } else {
                None
            }
        }
    }
}

/// 条目播完推进（`loops` 唯一调用）：按循环模式算下标，重载（成功即走，
/// 逐项 `playlist: item` 行即重播证明）；坏项跳过（已 warn，至多绕一圈）；
/// 无处可去（`once` 到头 / 全坏）回 None（调用方结束播放）。
pub(crate) fn advance_playlist(video: &mut Clip, cur: usize, eager: bool) -> Option<usize> {
    let total = config::playlist_len();
    let mode = config::playlist_loop();
    let mut nxt = next_playlist_index(cur, total, mode)?;
    let mut tries = 0usize;
    loop {
        let mut fs = Fs::new().ok()?;
        match load_playlist_clip(&mut fs, nxt, eager) {
            Ok(c) => {
                *video = c;
                return Some(nxt);
            }
            Err(_) => {
                tries += 1;
                if tries >= total.max(1) {
                    return None;
                }
                nxt = next_playlist_index(nxt, total, mode)?;
            }
        }
    }
}

/// 片源加载（播放列表首个可播项）：逐项自动检测，失败跳项 + warn，不断播；
/// 空列表 / 全坏直接 Err（无静默换源）。`eager` 为真无视 `feed` 强制全量；
/// `measure` 传假，边测边解。
pub(crate) fn load_video_clip(fs: &mut Fs, eager: bool) -> Result<Clip> {
    let total = config::playlist_len();
    if total == 0 {
        log::warn!("playlist: empty list, nothing to play");
        return Err(NyaStatus::_Debug(String::from("playlist: empty list")));
    }
    for i in 0..total {
        match load_playlist_clip(fs, i, eager) {
            Ok(c) => return Ok(c),
            // 跳项已 warn，此处直接推进（无静默回退：mp4 无匹配轨不换裸流）。
            Err(_) => continue,
        }
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
    let mut video = load_video_clip(&mut fs, false)?;
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
