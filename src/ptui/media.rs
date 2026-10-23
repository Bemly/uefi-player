//! ptui 供片扫描：`config::media_dir()` 单层目录枚举 + tagged 探针只读识别。
//!
//! - `""`=根目录，否则单层子目录（非递归）；目录不存在直接返回 `Err`
//!   （不回退、不静默，调用方屏显报错）。
//! - 用 `Fs::list_dir` 枚举直属项（子目录不进），每个文件名用既有
//!   `video::play::detect_clip_format` 逐个识别（只读调用，不改它；
//!   `.mp4` 容器探针会整文件读，属既有行为）；不可识别的跳过。
//! - 返回相对路径（`/` 分隔，固件侧打开前统一转反斜杠，见 `play::path_cstr16`
//!   同口径），按**文件名字节序**排序（UTF-8 下 `String::Ord` 即字节序，
//!   此处显式按字节比，中文名同样稳定）。

use alloc::string::String;
use alloc::vec::Vec;
use uefi::cstr16;

use crate::error::{NyaStatus, Result};
use crate::fs::Fs;
use crate::video::play::detect_clip_format_tagged;

/// 扫描媒体目录：返回可播片源相对路径（`/` 分隔），按字节序排序。
pub fn scan() -> Result<Vec<String>> {
    // 读媒体目录（`config::media_dir`，`""`=根目录；契约已落地，直调）。
    let dir = crate::config::media_dir();
    let mut fs = Fs::new()?;
    // 单层枚举：根走 `\`，子目录走其名（`/` 转 `\`；不存在直接 Err）。
    let entries: Vec<(String, bool)> = if dir.is_empty() {
        fs.list_dir(cstr16!("\\"))?
    } else {
        let sub = dir.replace('/', "\\");
        let c =
            uefi::CString16::try_from(sub.as_str()).map_err(|_| NyaStatus::FromStrWithBufError)?;
        fs.list_dir(&c)?
    };
    let mut out = Vec::new();
    for (name, is_dir) in entries {
        if is_dir {
            // 非递归：子目录不进（单层语义）。
            continue;
        }
        let rel = if dir.is_empty() {
            name
        } else {
            alloc::format!("{}/{}", dir, name)
        };
        // 只读调用：tagged 探针（tag="ptui"，串口探针行与播放列表的 "video" 区分）；
        // 识别失败（未知格式/mp4 无匹配轨）直接跳过，不改 detect 逻辑。
        if detect_clip_format_tagged(&rel, "ptui").is_some() {
            out.push(rel);
        }
    }
    // 文件名字节序（矩阵断言顺序稳定）。
    out.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    log::info!(
        "ptui: media dir='{}' count={}",
        if dir.is_empty() { "<root>" } else { &dir },
        out.len()
    );
    Ok(out)
}
