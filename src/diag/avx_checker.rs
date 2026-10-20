use core::arch::x86_64::{__cpuid, _xgetbv};

use crate::emit;
use crate::error::Result;
use crate::graphics::Screen;

pub fn run(screen: &mut Screen) -> Result {
    // 固件常不开 CR4.OSXSAVE（OVMF 与多台真机实测）；报告开机采到的原状态，
    // 再按需开启。与 hw_bench（菜单 8）一致，免得 7 说不可用、8 却测出来。
    let fw = crate::diag::common::firmware_osxsave();
    crate::diag::common::ensure_xsave_enabled();
    emit!(
        screen,
        "XSAVE: firmware OSXSAVE={} | now={}",
        match fw {
            Some(true) => "on",
            Some(false) => "off",
            None => "unk",
        },
        if crate::diag::common::osxsave_on() {
            "on"
        } else {
            "off"
        }
    );
    if check_avx_support() {
        // 此时可以安全使用 AVX 指令
        emit!(screen, "AVX is enabled and safe to use.");
    } else {
        emit!(screen, "AVX is NOT available. Falling back to SSE/u64.");
    }
    Ok(())
}

pub fn check_avx_support() -> bool {
    unsafe {
        // 1. 检查 CPUID.01h:ECX.AVX[bit 28] 和 CPUID.01h:ECX.OSXSAVE[bit 27]
        let cpuid_res = __cpuid(1);

        // Bit 27: OSXSAVE (系统是否支持扩展状态)
        // Bit 28: AVX (硬件是否支持 AVX)
        let osxsave_mask = 1 << 27;
        let avx_mask = 1 << 28;

        if (cpuid_res.ecx & avx_mask) == 0 {
            return false; // 硬件压根不支持
        }

        if (cpuid_res.ecx & osxsave_mask) == 0 {
            return false; // 固件未开启 OSXSAVE
        }

        // 2. 检查 XCR0 寄存器 (Extended Control Register)
        // 使用 xgetbv 指令，索引为 0
        // Bit 1: SSE 状态, Bit 2: AVX 状态
        let xcr0 = _xgetbv(0);
        let avx_state_mask = 1 << 2;

        (xcr0 & avx_state_mask) != 0
    }
}
