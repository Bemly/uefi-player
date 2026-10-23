//! `player.toml` 跨平台可视化编辑器（host 工具，非 UEFI）。
//!
//! 轮子选择（结论，见根 `AGENTS.md`「轮子优先」）：
//! - 搜过 `crates.io`：“egui glass blur”只有 `egui_glass`（Apple Liquid Glass，
//!   GPU 经 egui-wgpu 渲染）是现货，其余多为整窗 vibrancy（`window-vibrancy`）
//!   或已停更的 css 式皮肤，不满足“任意面板玻璃拟态”，故直接用它，不自研模糊管线。
//! - 搜过文件对话框：“egui file dialog”多为手写 picker；`rfd` 是原生对话框的
//!   事实标准轮子，故用 `rfd`，不手写。
//! - 搜过 i18n：“rust i18n”里 `rust-i18n`（400 万下载，YAML 编译期进二进制，
//!   `t!` + `set_locale`）最轻；`fluent` 要 unic-langid + ftl 文件，杀鸡用牛刀，
//!   故用前者。文案全在 `locales/`，中英切换只是 `set_locale`。
//! - 搜过 CJK 字体：egui 默认字体无中文字形，系统字体各平台 ttc 路径不一且
//!   egui 只取合集第 0 个字重；`egui-chinese-font` 是调系统字体的 std 轮，
//!   免打包但吃整套系统字库。最终用缝合像素 fusion-pixel 10px proportional
//!   简中 TTF 全量文件（真 `glyf` 轮廓，skrifa 可吃；`ms.bitmap/otb/pcf` 纯位图的不行）：
//!   `assets/fusion-pixel-10px-proportional-zh_hans.ttf`（上游 v2026.10.16 原包，
//!   `include_bytes!` 编进二进制，OFL-1.1 见 `assets/OFL.txt`），放字体链首位，
//!   全 UI 像素风，西文 fallback 到默认字体。8x8 中文 Rust 轮子无现货
//!   （font8x8 无中文），不找了。
//!   注意：字体禁止子集化（AGENTS 有令），缺字直接用全量，体积换完整。
//! - 驱动：`eframe` 的 `wgpu` 特性即 wgpu 驱动；`egui_glass` 不支持 glow，
//!   故 `NativeOptions.renderer` 强制 `Wgpu`.
//! - 图标：用户给的复古像素显示器图，按内容框裁掉下边框留白后透明垫正方形
//!   （`assets/icon.png` 1024 主图）；`icon-256.rgba` 预烘焙进二进制做运行时窗口图标
//!   （零新依赖，mac Dock 走 bundle 的 `icon.icns`，见 `bundle_macos.sh`），
//!   `icon.ico` 给 Windows 用。重做图标跑 `make_icon.py <原图>` + `iconutil` 即可。
//!
//! UI 布局照抄 `egui_glass` 官方 README 配方：“Sidebar and opaque content”
//! （右上 toast 状态 → 左侧玻璃导航卡 → 中央透明内容放稠密表单 →
//! 右下角悬浮动作胶囊），分隔一律留白，不用 `Separator`。
//! 试错记录：曾因 Panel 边界直线误伤过胶囊/薄片，真凶是 `Panel` 默认的
//! `show_separator_line`（已全关），详见 AGENTS.md。
//!
//! 实时背景（`LiveBackdrop`，用户拍板）：折射源优先动态视频帧（`video_bg`
//! GPU 转换 native 纹理，无帧/回退即静态背景图），根 `live.run` 纯绘制一遍，
//! 不占布局；页面提示行是中央薄片内的普通内容，不进折射源。
//! 顺序是铁律：`live.run` 必须在全部 `Glass*` 之前跑，否则先画的玻璃拿不到
//! 本帧 `LiveState`，回退到自身 rect 做 backdrop，会把整图压进每颗按钮。
//! 代价是每帧多一次离屏渲染 + mipmap，本工具页面小，可接受。
//!
//! 全玻璃强制要求（用户拍板）：所有可点击控件一律用库内组件——`GlassButton`
//! 做动作按钮/导航/单选开关，`GlassSlider` 做滑杆，`Glass`/`GlassToolbar` 做容器。
//! 注意这盖过了官方“玻璃不叠玻璃”（GlassButton 不嵌 Glass/Toolbar）的建议：
//! 嵌套处的折射是同一张 backdrop，多一层 tint，在本工具这种浅层 UI 上可接受。
//! 唯一的例外是无 GPU 兜底分支——玻璃管线没注册，只能用普通 egui 控件。
//!
//! 与固件侧 `src/config/` 同 schema（字段名/枚举字符串一致），改完存回 U 盘根目录即可.

use eframe::egui;

use crate::app::App;

rust_i18n::i18n!("locales", fallback = "en");

mod app;
mod codecs;
// RAW-GEOM：`[decode]` 子分类（`schema` 已到行数上限，拆出；重导出路径不变）。
mod decode_cfg;
mod display;
mod file_pick;
mod gallery;
mod pages;
mod rules;
// P5 触达拆分：`rules` 已超 1000 行红线，纯规则按域下沉（bench 待测/显隐 +
// coerce 读盘/落盘钳制；对外 `crate::rules::*` 路径不变，行为零改）。
mod rules_bench;
mod rules_coerce;
mod schema;
mod text;
mod transcode;
#[cfg(test)]
mod transcode_hw;
mod video_bg;
mod vulkan_gpu;

/// 背景图：片源第一帧（构建时用 ffmpeg 从片源抽好，见 `assets/bg.png`；
/// `ffmpeg -i input.mp4 -vframes 1 assets/bg.png`，1280x720，约 600KB）。
/// 直接喂 glass 自家的 `set_backdrop`（不另起模块、不手写纹理管线）。
/// 解失败就纯白 256 底（编进包断不了，不断任何流程）。
pub(crate) fn decode_bg() -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(include_bytes!("../assets/bg.png"))
        .ok()?
        .to_rgba8();
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    Some((img.width(), img.height(), img.into_raw()))
}

fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon-256.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}

fn main() -> eframe::Result<()> {
    rust_i18n::set_locale("en");
    let opts = eframe::NativeOptions {
        // egui_glass 硬性要求 wgpu 渲染器，glow 不可用。
        renderer: eframe::Renderer::Wgpu,
        // 主题自管（见 `App::new` 的 `set_theme`）：不跟系统，桌面是多少都不影响。
        viewport: egui::ViewportBuilder::default()
            // 默认打开尺寸（内容区；外框 1229x791，减 macOS 标题栏 32）。
            .with_inner_size([1229.0, 759.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "UEFI Player",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod bg_tests {
    #[test]
    fn bundled_first_frame_decodes() {
        // 打包图必须可解且几何正确（坏了构建期即知，运行时不再检查）。
        let bytes = include_bytes!("../assets/bg.png");
        // PNG 魔数。
        assert_eq!(&bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        let img = image::load_from_memory(bytes).expect("bg.png decodes");
        assert_eq!((img.width(), img.height()), (1280, 720));
    }
}
