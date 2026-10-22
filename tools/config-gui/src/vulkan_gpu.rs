//! Win/Linux Vulkan 硬解选卡（转码页，host `std`；mac 无 Vulkan 恒空行隐藏）。
//!
//! 背景（2026-10-22 用户要转码页加硬解选卡按钮）：`gpu-video 0.4` 的
//! `create_adapter` 是 first-match（只看编解码支持位，作者留 TODO 没做偏好）；
//! 但 `iter_adapters()` + `AdapterInfo{name/device_type/编解码位}` 全 public，
//! 故选卡不用 fork 轮子。本文件平台无关：偏好类型 + 纯选择函数全平台可测；
//! 真正碰轮子的 `enum_adapters()` 在 mac 下返回空（`gpu-video` 依赖本身只在
//! 非 mac 编入，转码页整行隐藏）。
//!
//! 语义：`Auto`＝现行 first-match（零行为变化）；`Discrete`/`Integrated`/
//! `Named` 命中即用未命中回退自动（保硬解不断 job，UI 另行“当前：”实际设备名）；
//! 空列表/无编解码能力卡即 `None`（调用方记 `note_hw_fallback` 走 CPU）。
//! `device_type` 判独显走 Debug 字串（`ash` 是轮子私有依赖本仓够不到类型，
//! 字串 `"DISCRETE_GPU"` 即 `ash::vk::PhysicalDeviceType` 同名变体锁定后继轮子
//! 改名此处单测会先红）。

/// 转码硬解显卡偏好（会话级，`TranscodeState` 持有；`Auto` 默认）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VulkanGpuPref {
    /// 现行 first-match（零行为变化）。
    Auto,
    /// 首个独显（无则回退自动）。
    Discrete,
    /// 首个核显/集显（无则回退自动）。
    Integrated,
    /// 按适配器名精确匹配（无则回退自动）。
    Named(String),
}

/// 适配器快照（UI 行 + 纯选择函数共用；轮子 `AdapterInfo` 的最小投影，
/// 枚举一次后轮子句柄即 dropped 建 device 时重枚举同序取）。
/// mac 非 test 构建下仅透传占位（行恒空），死了也别告警。
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct VkAdapterView {
    pub(crate) name: String,
    pub(crate) discrete: bool,
    pub(crate) dec: bool,
    pub(crate) enc: bool,
}

/// 纯选择：偏好 → 候选下标（无可用即 `None`；回退自动保证硬解不断流）。
/// 基准序＝轮子枚举序（`Auto` 与旧 `create_adapter` first-match 同口径）。
/// mac 非 test 构建下无调用方（同上）。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn select_vulkan_adapter(
    cands: &[VkAdapterView],
    pref: &VulkanGpuPref,
) -> Option<usize> {
    let capable = |i: usize| cands[i].dec && cands[i].enc;
    let first_capable = (0..cands.len()).find(|&i| capable(i));
    match pref {
        VulkanGpuPref::Auto => first_capable,
        VulkanGpuPref::Discrete => (0..cands.len())
            .find(|&i| cands[i].discrete && capable(i))
            .or(first_capable),
        VulkanGpuPref::Integrated => (0..cands.len())
            .find(|&i| !cands[i].discrete && capable(i))
            .or(first_capable),
        VulkanGpuPref::Named(want) => (0..cands.len())
            .find(|&i| cands[i].name == *want && capable(i))
            .or(first_capable),
    }
}

/// 枚举本机 Vulkan 视频适配器（UI 行用；失败/无 Vulkan 即空）。
pub(crate) fn list_adapters() -> Vec<VkAdapterView> {
    #[cfg(not(target_os = "macos"))]
    {
        enum_adapters().unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        Vec::new()
    }
}

/// 真枚举（非 mac；`gpu-video` 0.4 实测 API 口径：`iter_adapters` + `info()`）。
#[cfg(not(target_os = "macos"))]
fn enum_adapters() -> Result<Vec<VkAdapterView>, String> {
    let inst = gpu_video::VulkanInstance::new().map_err(|e| format!("vk instance: {e:?}"))?;
    let mut out = Vec::new();
    for a in inst
        .iter_adapters()
        .map_err(|e| format!("vk adapters: {e:?}"))?
    {
        let info = a.info();
        out.push(VkAdapterView {
            name: info.name.clone(),
            discrete: format!("{:?}", info.device_type) == "DISCRETE_GPU",
            dec: info.supports_decoding,
            enc: info.supports_encoding,
        });
    }
    Ok(out)
}

/// 会话级偏好（默认 `Auto`；UI 点选即换，device 缓存以偏好为键懒重建）。
static VK_PREF: std::sync::Mutex<VulkanGpuPref> = std::sync::Mutex::new(VulkanGpuPref::Auto);

/// 读当前偏好（编解码建 device 时消费）。
/// mac 非 test 构建下无调用方（同上）。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn vulkan_gpu_pref() -> VulkanGpuPref {
    VK_PREF
        .lock()
        .map(|g| g.clone())
        .unwrap_or(VulkanGpuPref::Auto)
}

/// 写偏好（UI 按钮行调用；`codecs` 侧 device 缓存以偏好为键，换键下次懒重建，
/// 在途 job 的旧 device（`Arc`）不受影响，编码按段建会话故新段即切新卡，
/// 解码会话随 `TrackDecoder` 一生，跨 job 才切）。
pub(crate) fn set_vulkan_gpu_pref(p: VulkanGpuPref) {
    if let Ok(mut g) = VK_PREF.lock() {
        *g = p;
    }
}

#[cfg(test)]
mod vulkan_gpu_tests {
    use super::*;

    fn cand(name: &str, discrete: bool, dec: bool, enc: bool) -> VkAdapterView {
        VkAdapterView {
            name: name.to_string(),
            discrete,
            dec,
            enc,
        }
    }

    fn mixed() -> Vec<VkAdapterView> {
        vec![
            cand("Intel UHD", false, true, true),
            cand("NVIDIA RTX", true, true, true),
            cand("llvmpipe", false, false, false),
        ]
    }

    #[test]
    fn auto_is_first_capable() {
        assert_eq!(
            select_vulkan_adapter(&mixed(), &VulkanGpuPref::Auto),
            Some(0)
        );
        // 首个无能力即跳过。
        let mut v = mixed();
        v[0].dec = false;
        assert_eq!(select_vulkan_adapter(&v, &VulkanGpuPref::Auto), Some(1));
    }

    #[test]
    fn discrete_integrated_with_fallback() {
        let v = mixed();
        assert_eq!(select_vulkan_adapter(&v, &VulkanGpuPref::Discrete), Some(1));
        assert_eq!(
            select_vulkan_adapter(&v, &VulkanGpuPref::Integrated),
            Some(0)
        );
        // 无独显→回退自动（首个可用）；全无能力→None。
        let ig_only = vec![cand("Intel UHD", false, true, true)];
        assert_eq!(
            select_vulkan_adapter(&ig_only, &VulkanGpuPref::Discrete),
            Some(0)
        );
        let dead = vec![cand("llvmpipe", false, false, false)];
        assert_eq!(select_vulkan_adapter(&dead, &VulkanGpuPref::Discrete), None);
        assert_eq!(select_vulkan_adapter(&dead, &VulkanGpuPref::Auto), None);
        assert_eq!(select_vulkan_adapter(&[], &VulkanGpuPref::Auto), None);
    }

    #[test]
    fn named_exact_or_fallback() {
        let v = mixed();
        assert_eq!(
            select_vulkan_adapter(&v, &VulkanGpuPref::Named(String::from("NVIDIA RTX"))),
            Some(1)
        );
        // 大小写/子串不算命中（精确匹配），回退自动。
        assert_eq!(
            select_vulkan_adapter(&v, &VulkanGpuPref::Named(String::from("rtx"))),
            Some(0)
        );
        assert_eq!(
            select_vulkan_adapter(&v, &VulkanGpuPref::Named(String::from("不存在"))),
            Some(0)
        );
    }

    #[test]
    fn pref_storage_roundtrip() {
        set_vulkan_gpu_pref(VulkanGpuPref::Discrete);
        assert_eq!(vulkan_gpu_pref(), VulkanGpuPref::Discrete);
        set_vulkan_gpu_pref(VulkanGpuPref::Named(String::from("X")));
        assert_eq!(vulkan_gpu_pref(), VulkanGpuPref::Named(String::from("X")));
        set_vulkan_gpu_pref(VulkanGpuPref::Auto);
        assert_eq!(vulkan_gpu_pref(), VulkanGpuPref::Auto);
    }
}
