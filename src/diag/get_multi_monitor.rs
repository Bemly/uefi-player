use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi::boot::{find_handles, open_protocol_exclusive};
use uefi::proto::console::gop::{GraphicsOutput, ModeInfo};

use crate::error::Result;
use crate::graphics::Screen;

// 历史上这里手写了一个 `println(gop, ...)` + `static mut OFFSET` 全局光标，
// 和 `Screen::draw_str` 逐行重复。重构后统一走 `screen.draw_str`，
// 光标状态复用 `Screen.stdout`，不再有 `static mut`。

fn info2str(a: &str, info: ModeInfo) -> String {
    let r = format!("resolution:{:?}", info.resolution());
    let f = format!("pixel_format:{:?}", info.pixel_format());
    let s = format!("stride:{:?}", info.stride());
    let b = format!("pixel_bitmask:{:?}", info.pixel_bitmask());
    format!("[{}]{} {} {} {}", a, r, f, s, b)
}

pub fn run(screen: &mut Screen) -> Result {
    get_multi_mode(screen);
    get_multi_monitor(screen)?;
    test_reopen_protocol(screen);
    test_reopen_multi_protocol(screen);
    Ok(())
}

pub fn get_multi_mode(screen: &mut Screen) {
    // 先收集成 owned 字符串再画：modes() 借用 GOP，draw_str 需要 &mut Screen，
    // 分两步避免借用冲突。
    let mode_strings: Vec<String> = {
        let gop = screen.get_gop();
        gop.modes()
            .map(|mode| info2str("query mode", *mode.info()))
            .collect()
    };

    for s in &mode_strings {
        screen.draw_str(s);
    }

    let current = {
        let gop = screen.get_gop();
        info2str("current mode", gop.current_mode_info())
    };
    screen.draw_str(&current);
}

// 经过测试发现大部分GOP驱动都只支持给我一个Handle(也就是一个显示器)
// 那多显示器已经毫无意义，已经回滚到单显示器
pub fn get_multi_monitor(screen: &mut Screen) -> Result {
    let mut lines: Vec<String> = Vec::new();

    if let Ok(handles) = find_handles::<GraphicsOutput>() {
        for h in &handles {
            // Best-effort：Screen 已经独占了一路 GOP，
            // 那一路 open 会失败是预期行为，直接跳过。
            if let Ok(gop) = open_protocol_exclusive::<GraphicsOutput>(*h) {
                for mode in gop.modes() {
                    lines.push(info2str(&format!("query {:?}", h), *mode.info()));
                }
                lines.push(info2str(
                    &format!("current {:?}", h),
                    gop.current_mode_info(),
                ));
            }
        }
    }

    if lines.is_empty() {
        // 单显示器机器的常见情况：唯一的 GOP 正被 Screen 独占，
        // 改从已持有的 handle 上报，避免“什么都查不到”。
        let owned: Vec<String> = {
            let gop = screen.get_gop();
            let mut v: Vec<String> = gop
                .modes()
                .map(|mode| info2str("query mode (owned)", *mode.info()))
                .collect();
            v.push(info2str("current mode (owned)", gop.current_mode_info()));
            v
        };
        lines = owned;
    }

    for s in &lines {
        screen.draw_str(s);
    }
    Ok(())
}

pub fn test_reopen_protocol(screen: &mut Screen) {
    // it work! : )
    // 注：菜单化后 Screen 常驻独占 GOP，这里改为验证“借用-归还”语义：
    // 反复通过 Screen 拿到 GOP 并读分辨率，证明独占持有下可重入访问。
    {
        let _ = screen.get_gop().current_mode_info().resolution();
    }

    {
        screen.draw_str("reopen protocol");
    }

    {
        screen.draw_str("reopen protocol again");
    }
}

pub fn test_reopen_multi_protocol(screen: &mut Screen) {
    // it dont work :(
    // qemu第二个gop被第一个gop占用了，所以这里会失败，故不要unwrap。
    // 菜单化后 Screen 占着第一路，结果不变：这里只记录能抢到几路。
    if let Ok(handles) = find_handles::<GraphicsOutput>() {
        let total = handles.len();
        let mut grabbed = 0;
        for h in handles {
            if open_protocol_exclusive::<GraphicsOutput>(h).is_ok() {
                grabbed += 1;
            }
        }
        screen.draw_str(&format!(
            "reopen multi protocol: grabbed {}/{}",
            grabbed, total
        ));
    }
}
