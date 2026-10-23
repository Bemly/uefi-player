//! 开机空闲层（顶层交互入口）：壁纸待机 + 启动选项弹窗。
//!
//! 流程：`main` → 校准（首轮无记忆必测，即“首轮默认测试”）→ `idle::run`（永不返回）：
//! 壁纸全屏循环（无片源就黑屏 + 提示行）；任意键 → 按 `[wallpaper] popup_freeze`
//! 冻结或继续播行动画 → 弹出启动选项（面板化：↑↓ 选、Enter 执行，旧单键全保留）。
//! 选条目交权（回来继续待机）；`p` 进 TUI 外壳（`ptui::run`，`q` 层层回来）；
//! `f` 重启进固件设置（BootToFwUi）；`s` 关机；`r` 重启；`q` 回壁纸；
//! Tab 呼出悬浮跳转菜单（`ptui::jump`，可直跳 TUI 任一子页）。
//! 串口标记见各 `log::` 行（矩阵靠它们同步）。

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use uefi::boot::stall;

use crate::boot::{self, KEYS};
use crate::diag::common::{flush_keys, poll_key};
use crate::error::Result;
use crate::graphics::{Screen, calibrate_tps, tsc};
use crate::ptui::jump::{self, Target};
use crate::ptui::ui::{self, Act, Row, View};
use crate::ptui::{self, Exit};
use crate::video::decoder::Clip;
use crate::wallpaper::{self, WP_H, WP_TOP, WP_W};

/// 顶层循环：待机 → 弹窗 → 动作 → 回待机（交权/播放器返回后亦然；
/// 关机/重启/固件设置永不返回）。
pub fn run(mut screen: Screen) -> ! {
    loop {
        // 每次回待机都重载壁纸（U盘热插拔换片源即生效；便宜）。
        let clip = wallpaper::load().unwrap_or(None);
        let tps = if clip.is_some() { calibrate_tps() } else { 1 };
        // 待机键路由：`p` 进 TUI 外壳，其余任意键走原引导弹窗
        // （弹窗内仍保留 `p` 入口，行为对称）。
        let (key, _raw) = idle_wait(&mut screen, &clip, tps);
        if key == 'p' {
            log::warn!("idle: ptui mode");
            let (s, exit) = ptui::run(screen);
            screen = s;
            if exit == Exit::Idle {
                continue;
            }
        }
        screen = match popup(screen, &clip, tps) {
            Ok(s) => s,
            Err(e) => {
                // 重建屏显都失败：halt（理论到不了，交权失败路径在别处已处理）。
                log::warn!("popup: screen lost: {:?}", e);
                loop {
                    stall(Duration::from_secs(60));
                }
            }
        };
    }
}

/// 壁纸待机：全屏循环（`fps` 预算）；无片源/分辨率不对就黑屏 + 提示行。
/// 返回唤醒按键（调用方路由：`p` 进 TUI，其余走引导弹窗）。串口 `idle: ready` 供矩阵卡点。
fn idle_wait(screen: &mut Screen, clip: &Option<Clip>, tps: u64) -> (char, u16) {
    // 先排空陈旧输入再打标记：标记之后到的键才是有效按键（矩阵靠标记卡点）。
    // 弹窗内不再排空——开弹窗的键已被上轮 poll 消耗，后到的都是真输入。
    flush_keys();
    log::info!("idle: ready (press any key)");
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    let live = match clip {
        Some(c) if !c.is_empty() && (w as usize, h as usize) == (WP_W, WP_H) => true,
        _ => {
            let _ = screen.clear();
            screen.reset_cursor();
            screen.draw_str("press any key for boot options...");
            false
        }
    };
    // 三态帧率：None = 不限速（budget 0 即每轮都画，`>=` 恒成立）。
    let budget = crate::config::effective_wallpaper_fps()
        .map(|f| tps.max(1) / f.max(1) as u64)
        .unwrap_or(0);
    let mut idx = 0usize;
    let mut last = tsc();
    flush_keys();
    loop {
        if let Some(k) = poll_key() {
            return k;
        }
        if live {
            let now = tsc();
            if now.saturating_sub(last) >= budget {
                last = now;
                let clip = clip.as_ref().unwrap_or_else(|| unreachable!());
                // 几何取 clip 真实 `w/h`（`wallpaper::load` 已带几何），让 `display.fit` 正常生效；
                // 写死 `WP_W/WP_H` 会把非 1080p 画廊帧按 1080p 送显（错窗/撕裂）。
                let (cw, ch) = (clip.w, clip.h);
                let _ = screen.upload_frame(cw, ch, clip.frame(idx));
                // 画廊步进：list 回绕，once 到末帧停留（见 `wallpaper::next_gallery_index`）。
                idx = wallpaper::next_gallery_index(idx, clip.len());
            }
        } else {
            stall(Duration::from_millis(5));
        }
    }
}

/// 弹窗固定项（快捷键, 标题, 说明）：接在条目之后。
const FIXED: [(char, &str, &str); 5] = [
    ('p', "TUI 播放器菜单", "播放 / 调档 / 诊断 / 壁纸"),
    ('f', "固件设置", "重启进 BIOS/UEFI 设置界面"),
    ('r', "重启", "冷重启"),
    ('s', "关机", "关闭电源"),
    ('q', "返回壁纸", "回到壁纸待机"),
];

/// 启动选项弹窗：条目数字键直选交权；`p` 进 TUI 外壳（与待机直达对称）、`f/s/r` 固定项；`q` 回壁纸；
/// ↑↓ 选、Enter 执行选中行（光标初值即默认项，Enter 不动光标 = 旧“跑默认项”）；
/// Tab 浮层跳转。`frozen` 时静态屏，`live` 时行区继续播（面板之下）。
fn popup(mut screen: Screen, clip: &Option<Clip>, tps: u64) -> Result<Screen> {
    let list = boot::list()?;
    // 冻结语义：开关关 / 无片源 / 分辨率不对 → 静态（行区无东西可播）。
    let (w, h) = screen.get_gop().current_mode_info().resolution();
    let can_live = !clip.as_ref().map(|c| c.is_empty()).unwrap_or(true)
        && (w as usize, h as usize) == (WP_W, WP_H);
    let frozen = !can_live || crate::config::wallpaper_popup_freeze();
    let selectable = list.entries.len().min(KEYS.len());
    let n = selectable + FIXED.len();
    // 光标：默认项（无默认则不高亮，Enter 无动作，与旧口径一致）。
    let mut sel: Option<usize> = list.default_index.filter(|&i| i < selectable);
    ui::clear_status();
    let mut anim_top = draw_popup(&mut screen, &list, sel);
    log::warn!("popup: open ({})", if frozen { "frozen" } else { "live" });

    // 三态帧率：None = 不限速（budget 0 即每轮都画，`>=` 恒成立）。
    let budget = crate::config::effective_wallpaper_fps()
        .map(|f| tps.max(1) / f.max(1) as u64)
        .unwrap_or(0);
    let mut idx = 0usize;
    let mut last = tsc();
    // 注意：这里不 flush——开弹窗的键已被待机 poll 消耗，列表枚举期间到达的
    // 都是真输入；flush 会把矩阵紧随其后的同步键冲掉（曾因此全员超时）。
    loop {
        if !frozen {
            let now = tsc();
            if now.saturating_sub(last) >= budget {
                last = now;
                if let Some(c) = clip {
                    // 同待机：行区源几何取 clip 真实 `w/h`（`clip_blit` 按此交窗，不写屏外）。
                    let _ = screen.upload_frame_region(c.w, c.h, anim_top, c.frame(idx));
                    idx = wallpaper::next_gallery_index(idx, c.len());
                }
            }
        }
        let Some((key, _raw)) = poll_key() else {
            if frozen {
                stall(Duration::from_millis(5));
            }
            continue;
        };
        // 统一语义（`q`/Esc 回壁纸、Tab 浮层、方向键/Enter 选行；其余单键照旧）。
        let key = match ui::map(&mut screen, Target::Boot, key) {
            Act::Back => 'q',
            Act::Jump(Target::Page(next)) => {
                let (s, exit) = ptui::run_at(screen, next);
                screen = s;
                if exit == Exit::Idle {
                    log::warn!("popup: back to idle");
                    return Ok(screen);
                }
                anim_top = draw_popup(&mut screen, &list, sel);
                continue;
            }
            Act::Jump(Target::Idle) => {
                log::warn!("popup: back to idle");
                return Ok(screen);
            }
            Act::Jump(_) | Act::Redraw | Act::Left => {
                anim_top = draw_popup(&mut screen, &list, sel);
                continue;
            }
            act @ (Act::Up | Act::Down) => {
                let up = matches!(act, Act::Up);
                sel = Some(match sel {
                    Some(i) => ui::step_sel(i, n, up),
                    None if up => n - 1,
                    None => 0,
                });
                anim_top = draw_popup(&mut screen, &list, sel);
                continue;
            }
            Act::Enter | Act::Right => match sel {
                Some(i) if i < selectable => KEYS[i] as char,
                Some(i) => FIXED[i - selectable].0,
                None => continue,
            },
            Act::Key(c) => c,
        };
        // 固定项（先判；`q` 永不进条目表）。
        match key {
            'q' => {
                log::warn!("popup: back to idle");
                return Ok(screen);
            }
            'p' => {
                log::warn!("popup: player mode");
                let (s, exit) = ptui::run(screen);
                screen = s;
                if exit == Exit::Idle {
                    log::warn!("popup: back to idle");
                    return Ok(screen);
                }
                anim_top = draw_popup(&mut screen, &list, sel);
                continue;
            }
            'f' => {
                jump::boot_to_firmware(&mut screen);
                anim_top = draw_popup(&mut screen, &list, sel);
                continue;
            }
            's' => {
                log::warn!("popup: shutdown");
                uefi::runtime::reset(
                    uefi::runtime::ResetType::SHUTDOWN,
                    uefi::Status::SUCCESS,
                    None,
                );
            }
            'r' => {
                log::warn!("popup: reboot");
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None);
            }
            _ => {}
        }
        // 条目键（`p/f/s/r` 已被固定项占，`q` 已返回；剩下的才查表）。
        if let Some(pos) = KEYS.iter().position(|&k| k as char == key) {
            if pos < list.entries.len() {
                screen = boot::boot_index(screen, &list, clip, tps, pos)?;
                return Ok(screen);
            }
        }
    }
}

/// 弹窗面板（条目 + 固定项；条目串口行同 `boot::entry_line` 口径，屏显拆列）；
/// 返回行区动画起点（`max(WP_TOP, 面板底)`）。
fn draw_popup(
    screen: &mut Screen,
    list: &crate::boot::entries::BootList,
    sel: Option<usize>,
) -> usize {
    let selectable = list.entries.len().min(KEYS.len());
    let keys: Vec<String> = (0..selectable)
        .map(|i| format!("{}", KEYS[i] as char))
        .chain(FIXED.iter().map(|f| format!("{}", f.0)))
        .collect();
    let mut rows: Vec<Row> = Vec::with_capacity(keys.len());
    for (i, e) in list.entries.iter().take(selectable).enumerate() {
        let mut v = format!("<{}>", e.kind.kind_str());
        if let Some(t) = e.tries_left {
            v.push_str(&format!(" tries {}", t));
        }
        if Some(i) == list.default_index {
            v.push_str("  ★ 默认");
        }
        rows.push(Row::new(&keys[i], &e.title, v));
    }
    for (j, (_, name, desc)) in FIXED.iter().enumerate() {
        rows.push(Row::new(&keys[selectable + j], name, String::from(*desc)));
    }
    let mut notes: Vec<String> = list.warnings.iter().map(|w| format!("! {}", w)).collect();
    if list.entries.is_empty() {
        notes.push(String::from("没有可引导的条目（no bootable entries）"));
    }
    if list.entries.len() > KEYS.len() {
        notes.push(format!(
            "! {} entries beyond key range, not selectable",
            list.entries.len() - KEYS.len()
        ));
    }
    let bottom = ui::draw(
        screen,
        &View {
            title: "启动选项",
            rows: &rows,
            sel,
            notes: &notes,
            hint: "↑↓ 选择  Enter 启动/执行  单键直选  Tab 跳转  q 返回壁纸",
            compact: true,
        },
    );
    bottom.max(WP_TOP)
}
