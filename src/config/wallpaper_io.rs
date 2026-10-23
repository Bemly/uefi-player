//! 壁纸/画廊文件侧（`src/config` 的 wallpaper 半；P2 配置拆分载体）。
//!
//! - P2 破坏式更新：`[[wallpaper.items]]` 已删（画廊唯一入口收敛到 `[playlist]`），
//!   `[wallpaper] loop` 已删（循环语义只留 `[playlist] loop` 一处）；旧键残留按
//!   serde 默认忽略，无迁移/别名兜底。
//! - 本文件持有文件侧原子（playlist 列表 + wallpaper 四键开关），播放器侧
//!   （accel/render/schedule/play/feed/decode/display/tune/media_dir）一律读
//!   `runtime`，不在此。
//! - `wallpaper_loop/len/path/frames` 为 `[playlist]` 别名（签名不变，
//!   `wallpaper.rs`/`idle`/`menu`/`boot` 零改即跟随收敛）。
//! - TUI 写回 API（契约锁定）：`set_wallpaper_enable/fps/preload/popup_freeze`
//!   + `save_wallpaper()`（只写 `[wallpaper]` 表，别的表原样保留，
//!   复用落盘钳制口径：`preload.min(32)`，`fps` 经 `parse_fps` 往返）。

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use uefi::cstr16;

use super::{Config, FpsPolicy, LoopMode, PlaylistItem, parse_fps};
use crate::error::NyaStatus;
use crate::fs::Fs;

/// loop：0=loop_single，1=loop_list，2=once（`LoopMode` 判别值）。
static PLAYLIST_LOOP: AtomicU8 = AtomicU8::new(1);
static PLAYLIST_PTR: AtomicUsize = AtomicUsize::new(0);
static PLAYLIST_LEN: AtomicUsize = AtomicUsize::new(0);
static WALLPAPER_ENABLE: AtomicU8 = AtomicU8::new(0);
/// 0=auto，1=capped，2=max（`wallpaper.fps` 解析结果，与 `PLAY_MODE` 同构）。
static WALLPAPER_MODE: AtomicU8 = AtomicU8::new(1);
static WALLPAPER_CAP: AtomicU32 = AtomicU32::new(15);
static WALLPAPER_PRELOAD: AtomicU32 = AtomicU32::new(8);
static WALLPAPER_POPUP_FREEZE: AtomicU8 = AtomicU8::new(1);

/// 条目路径表装填（`load()` 唯一调用）：Vec 收敛为常驻泄漏切片，
/// 指针+长度进原子存储（`no_std` 无新依赖，`Box::leak` 会话期风格）。
fn store_paths(ptr: &AtomicUsize, len: &AtomicUsize, items: Vec<PlaylistItem>) {
    let paths: Vec<String> = items.into_iter().map(|it| it.path).collect();
    let n = paths.len();
    let p = Box::leak(paths.into_boxed_slice()).as_ptr() as usize;
    ptr.store(p, Ordering::Relaxed);
    len.store(n, Ordering::Relaxed);
}

fn load_path(ptr: &AtomicUsize, len: &AtomicUsize, i: usize) -> Option<String> {
    if i >= len.load(Ordering::Relaxed) {
        return None;
    }
    let p = ptr.load(Ordering::Relaxed) as *const String;
    if p.is_null() {
        return None;
    }
    unsafe { Some((*p.add(i)).clone()) }
}

/// 开机装填播放列表（`[playlist] loop` + 条目；画廊唯一入口）。
pub(crate) fn store_playlist(loop_mode: LoopMode, items: Vec<PlaylistItem>) {
    PLAYLIST_LOOP.store(loop_mode.as_u8(), Ordering::Relaxed);
    store_paths(&PLAYLIST_PTR, &PLAYLIST_LEN, items);
}

/// 开机装填壁纸开关表（四键；`loop`/`items` 已删，不再装填）。
pub(crate) fn store_wallpaper(enable: bool, fps: &str, preload: u32, popup_freeze: bool) {
    WALLPAPER_ENABLE.store(enable as u8, Ordering::Relaxed);
    match parse_fps(fps) {
        FpsPolicy::Auto => WALLPAPER_MODE.store(0, Ordering::Relaxed),
        FpsPolicy::Capped(n) => {
            WALLPAPER_MODE.store(1, Ordering::Relaxed);
            WALLPAPER_CAP.store(n, Ordering::Relaxed);
        }
        FpsPolicy::Max => WALLPAPER_MODE.store(2, Ordering::Relaxed),
    }
    WALLPAPER_PRELOAD.store(preload.min(32), Ordering::Relaxed);
    WALLPAPER_POPUP_FREEZE.store(popup_freeze as u8, Ordering::Relaxed);
}

/// 播放列表循环模式（`[playlist] loop`，默认回绕；壁纸画廊列表共用）。
pub fn playlist_loop() -> LoopMode {
    LoopMode::from_u8(PLAYLIST_LOOP.load(Ordering::Relaxed))
}

/// 播放列表项数快照（原子计数；画廊项数即此）。
pub fn playlist_len() -> usize {
    PLAYLIST_LEN.load(Ordering::Relaxed)
}

/// 播放列表路径（按下标克隆；越界/空表回 None）。
pub fn playlist_path(i: usize) -> Option<String> {
    load_path(&PLAYLIST_PTR, &PLAYLIST_LEN, i)
}

/// 壁纸总开关（菜单背景+引导过渡共用）。
pub fn wallpaper_enabled() -> bool {
    WALLPAPER_ENABLE.load(Ordering::Relaxed) != 0
}

/// 画廊循环模式（P2 起为 `[playlist] loop` 别名；签名不变）。
pub fn wallpaper_loop() -> LoopMode {
    playlist_loop()
}

/// 画廊项数快照（P2 起为 `[playlist]` 项数别名；签名不变）。
pub fn wallpaper_len() -> usize {
    playlist_len()
}

/// 画廊路径（P2 起为 `[playlist]` 别名；越界/空表回 None）。
pub fn wallpaper_path(i: usize) -> Option<String> {
    playlist_path(i)
}

/// 选项弹窗时冻结壁纸（默认 true；false 则行区继续播）。
pub fn wallpaper_popup_freeze() -> bool {
    WALLPAPER_POPUP_FREEZE.load(Ordering::Relaxed) != 0
}

/// 开机解析出的壁纸帧率开关（与 `play_policy` 同构）。
pub fn wallpaper_fps_policy() -> FpsPolicy {
    match WALLPAPER_MODE.load(Ordering::Relaxed) {
        1 => FpsPolicy::Capped(WALLPAPER_CAP.load(Ordering::Relaxed).max(1)),
        2 => FpsPolicy::Max,
        _ => FpsPolicy::Auto,
    }
}

/// 实际生效的壁纸帧预算（`None`=不限速）：`capped` 直接用（1-240 钳制）；
/// `max` 不限；`auto` 跟 vsync（开则按 `effective_vsync_fps()`，关则不限）。
pub fn effective_wallpaper_fps() -> Option<u32> {
    match wallpaper_fps_policy() {
        FpsPolicy::Max => None,
        FpsPolicy::Capped(n) => Some(n.clamp(1, 240).max(1)),
        FpsPolicy::Auto => {
            if super::vsync_enabled() {
                Some(super::effective_vsync_fps())
            } else {
                None
            }
        }
    }
}

/// B 过渡长度（画廊语义：列表长度即全集；只读 `boot` 过渡用）。
pub fn wallpaper_frames() -> u32 {
    wallpaper_len().min(u32::MAX as usize) as u32
}

/// 预载帧数上限（0=不封顶；1-32，已钳制）。
pub fn wallpaper_preload() -> u32 {
    WALLPAPER_PRELOAD.load(Ordering::Relaxed).min(32)
}

// ---------- TUI 壁纸写回 API（契约锁定，给 TUI 页面用） ----------

/// TUI 写 `[wallpaper] enable`（只改内存，落盘另调 `save_wallpaper`）。
pub fn set_wallpaper_enable(enable: bool) {
    WALLPAPER_ENABLE.store(enable as u8, Ordering::Relaxed);
}

/// TUI 写 `[wallpaper] fps`（字符串三态 `"auto"`/`"max"`/`"1"`-`"240"`；
/// 只改内存，非法值按 `parse_fps` 回 `Auto`，落盘另调 `save_wallpaper`）。
pub fn set_wallpaper_fps(fps: &str) {
    match parse_fps(fps) {
        FpsPolicy::Auto => WALLPAPER_MODE.store(0, Ordering::Relaxed),
        FpsPolicy::Capped(n) => {
            WALLPAPER_MODE.store(1, Ordering::Relaxed);
            WALLPAPER_CAP.store(n, Ordering::Relaxed);
        }
        FpsPolicy::Max => WALLPAPER_MODE.store(2, Ordering::Relaxed),
    }
}

/// TUI 写 `[wallpaper] preload`（只改内存，已钳 0-32，落盘另调 `save_wallpaper`）。
pub fn set_wallpaper_preload(preload: u32) {
    WALLPAPER_PRELOAD.store(preload.min(32), Ordering::Relaxed);
}

/// TUI 写 `[wallpaper] popup_freeze`（只改内存，落盘另调 `save_wallpaper`）。
pub fn set_wallpaper_popup_freeze(popup_freeze: bool) {
    WALLPAPER_POPUP_FREEZE.store(popup_freeze as u8, Ordering::Relaxed);
}

/// TUI 落盘 `[wallpaper]` 表（只写本表四键，别的表原样保留；
/// 复用落盘钳制口径：`preload.min(32)`，`fps` 按当前策略往返为字符串）。
pub fn save_wallpaper() -> crate::error::Result {
    let fps = match wallpaper_fps_policy() {
        FpsPolicy::Auto => String::from("auto"),
        FpsPolicy::Max => String::from("max"),
        FpsPolicy::Capped(n) => alloc::format!("{}", n.clamp(1, 240).max(1)),
    };
    let enable = wallpaper_enabled();
    let preload = wallpaper_preload();
    let popup_freeze = wallpaper_popup_freeze();
    let mut fs = Fs::new()?;
    let mut cfg = match fs.read_file(cstr16!("player.toml")) {
        Ok(bytes) => toml::from_str::<Config>(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
        Err(_) => Config::default(),
    };
    cfg.wallpaper.enable = enable;
    cfg.wallpaper.fps = fps;
    cfg.wallpaper.preload = preload.min(32);
    cfg.wallpaper.popup_freeze = popup_freeze;
    let text = toml::to_string(&cfg)
        .map_err(|_| NyaStatus::_Debug(String::from("cfg: serialize failed")))?;
    fs.write_file(cstr16!("player.toml"), text.as_bytes())?;
    Ok(())
}
