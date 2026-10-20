//! uefi-input2 交互探测：Ex 协议在位 + shift/toggle 状态上报。
//!
//! 只读探测（`reset` 清空陈旧键码除外，无 toggle 灯副作用）：
//! 打开 Ex 版 stdin，等一键（矩阵会发 `z`），把 `KeyData` 的修饰/锁定
//! 状态打到串口（`input2: key=` 行供矩阵断言），最后 `input2: PASS`。

use core::time::Duration;

use uefi::boot::stall;
use uefi::proto::console::text::Key;

use crate::emit;
use crate::error::{NyaStatus, Result};
use crate::graphics::Screen;

fn fail(msg: &str) -> NyaStatus {
    NyaStatus::_Debug(alloc::string::String::from(msg))
}

pub fn run(screen: &mut Screen) -> Result {
    emit!(screen, "=== uefi-input2 extended input probe ===");
    let got = uefi_input2::with_stdin(|input| {
        // 清空陈旧键码，保证随后读到的是这次按下的键。
        input.reset(false)?;
        emit!(screen, "input2: waiting up to 10s for a key...");
        for _ in 0..200 {
            if let Some(kd) = input.read_key_stroke_ex() {
                let key_desc = match kd.key {
                    Key::Printable(c) => {
                        let v = u16::from(c);
                        alloc::format!(
                            "'{}'",
                            char::from_u32(v as u32).unwrap_or('?')
                        )
                    }
                    Key::Special(s) => alloc::format!("scan={:#06X}", s.0),
                };
                emit!(
                    screen,
                    "input2: key={} mods_valid={} shift={} ctrl={} alt={} toggles_valid={} caps={} realtime={}",
                    key_desc,
                    kd.supports_modifiers(),
                    kd.shift(),
                    kd.ctrl(),
                    kd.alt(),
                    kd.supports_toggles(),
                    kd.caps_lock(),
                    kd.is_realtime_mode(),
                );
                return Ok(true);
            }
            stall(Duration::from_millis(50));
        }
        Ok(false)
    })
    .map_err(NyaStatus::from)?;
    if !got {
        return Err(fail("input2: no key within 10s"));
    }
    emit!(screen, "input2: PASS");
    Ok(())
}
