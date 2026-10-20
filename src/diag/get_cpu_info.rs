use raw_cpuid::CpuId;
use uefi::boot::{get_handle_for_protocol, open_protocol_exclusive};
use uefi::proto::pi::mp::MpServices;

use crate::diag::get_smbios;
use crate::emit;
use crate::error::Result;
use crate::graphics::Screen;

pub fn run(screen: &mut Screen) -> Result {
    cpu_info_by_mp(screen)?;
    get_smbios::get_cpu_info_by_smbios(screen);
    cpu_info_by_cpuid(screen);
    Ok(())
}

fn cpu_info_by_mp(screen: &mut Screen) -> Result {
    // 在定位器中寻找 MpServices 协议
    let mp_services_handle = get_handle_for_protocol::<MpServices>()?;

    let mp_services = open_protocol_exclusive::<MpServices>(mp_services_handle)?;

    // BSP 喊话
    let my_index = mp_services.who_am_i()?;
    let my_info = mp_services.get_processor_info(my_index)?;

    emit!(
        screen,
        "I am the BSP {}, located in the physical slot {}, the kernel {}",
        my_index,
        my_info.location.package,
        my_info.location.core
    ); // 我是逻辑核心 {}, 位于物理插槽 {}, 内核 {}

    // 获取处理器计数 (ProcessorCount)
    let counts = mp_services.get_number_of_processors()?;

    emit!(screen, "Total number of logic processors: {}", counts.total); // 总逻辑处理器数
    emit!(
        screen,
        "Number of currently enabled processors: {}",
        counts.enabled
    ); //当前启用的处理器数

    // 遍历并获取每个处理器的详细信息 (ProcessorInformation)
    for i in 0..counts.total {
        let info = mp_services.get_processor_info(i)?;

        emit!(
            screen,
            "Processor Index: {}, uuid:{}, enabled: {}, healthy: {}, bsp: {}",
            i,
            info.processor_id,
            info.is_enabled(),
            info.is_healthy(),
            info.is_bsp()
        );

        // 提取物理位置 (CpuPhysicalLocation)
        let loc = info.location;
        emit!(
            screen,
            "Physical location: Slot={}, Kernel={}, Thread={}",
            loc.package,
            loc.core,
            loc.thread
        ); // 物理位置: 插槽={}, 内核={}, 线程={}
    }

    Ok(())
}

fn cpu_info_by_cpuid(screen: &mut Screen) {
    // 主要是指令集
    let cpuid = CpuId::new();
    emit!(screen, "[cpuid_Vendor]{:?}", cpuid.get_vendor_info());
    emit!(screen, "[cpuid_Feature]{:?}", cpuid.get_feature_info());
    emit!(
        screen,
        "[cpuid_Extended_Feature]{:?}",
        cpuid.get_extended_feature_info()
    );
    emit!(
        screen,
        "[cpuid_Extended_State]{:?}",
        cpuid.get_extended_state_info()
    );
    emit!(
        screen,
        "[cpuid_Brand]{:?}",
        cpuid.get_processor_brand_string()
    );

    // 寻找大核
    // unimplemented!()
    // cpuid根本就找不到！气死我了 直接动态跑分吧
}
