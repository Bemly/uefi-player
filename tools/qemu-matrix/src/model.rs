//! `qemu-matrix::model`：矩阵数据模型（`Matrix/Global/Case/Expect/KeyStep` 等）。
//! 纯 `serde::Deserialize` 结构 + 默认值，无逻辑。拆分仅为 ≤1000 行。

use serde::Deserialize;

fn default_wait() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
pub(crate) struct Matrix {
    #[serde(default)]
    pub(crate) global: Global,
    #[serde(default)]
    pub(crate) case: Vec<Case>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct Global {
    pub(crate) smp: Option<u32>,
    pub(crate) mem_mb: Option<u32>,
    pub(crate) ovmf: Option<String>,
    /// 默认 CPU 模型：缺省不用 `-cpu`（QEMU 默认 qemu64，SSE2 基线，门禁口径）；
    /// "host" = 宿主透传（本机 i5-10400 即 AVX2），其它按 QEMU 模型名原样传。
    pub(crate) cpu: Option<String>,
    pub(crate) timeout_sec: Option<u64>,
    pub(crate) settle_sec: Option<u64>,
    #[serde(default)]
    pub(crate) forbid: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Case {
    pub(crate) name: String,
    pub(crate) config: String,
    #[serde(default)]
    pub(crate) boot_expect: Vec<Expect>,
    #[serde(default)]
    pub(crate) keys: Vec<KeyStep>,
    #[serde(default)]
    pub(crate) expect: Vec<Expect>,
    pub(crate) forbid: Option<Vec<String>>,
    pub(crate) timeout_sec: Option<u64>,
    pub(crate) settle_sec: Option<u64>,
    #[serde(default)]
    pub(crate) needs_video: bool,
    #[serde(default)]
    pub(crate) needs_wallpaper: u32,
    /// 播片 raw 片源：`1080p/frameNNNN.raw` 黑场 N 帧（`[play] format = "raw"` 用）。
    #[serde(default)]
    pub(crate) needs_rawvideo: u32,
    /// 播片 raws 片源：`1080p/video.raws` 黑场 N 帧单文件（`[play] format = "raw"` 的单文件路用）。
    #[serde(default)]
    pub(crate) needs_raws: u32,
    /// 壁纸 qois 片源：把测试视频拷成 `wallpaper/wall.qois`
    ///（`[wallpaper] format = "qois"` 用，验开机预解码 + 循环）。
    #[serde(default)]
    pub(crate) needs_wallpaper_qois: bool,
    /// 播片 hevc 片源：探针样片拷成 `1080p/video.hevc`
    ///（`[play] format = "hevc"` 用，Annex-B 软解进 Clip）。
    #[serde(default)]
    pub(crate) needs_hevcvideo: bool,
    /// 播片 vp9 片源：探针样片拷成 `1080p/video.ivf`
    ///（`[play] format = "vp9"` 用，IVF 软解进 Clip）。
    #[serde(default)]
    pub(crate) needs_vp9video: bool,
    /// 播片 avc 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "avc"` 用，石雨堂拆 avc 轨进 Clip）。
    #[serde(default)]
    pub(crate) needs_avcvideo: bool,
    /// 播片 hevc-MP4 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "hevc"` 的容器路，用石雨堂拆 hev 轨进 Clip）。
    #[serde(default)]
    pub(crate) needs_hevcmp4video: bool,
    /// 播片 vp9-MP4 片源：MP4 样片拷成 `1080p/video.mp4`
    ///（`[play] format = "vp9"` 的容器路，用石雨堂拆 vp09 轨进 Clip）。
    #[serde(default)]
    pub(crate) needs_vp9mp4video: bool,
    /// 播片 h263 片源：裸流样片拷成 `1080p/video.h263`
    ///（`[play] format = "h263"` 用，直解进 Clip）。
    #[serde(default)]
    pub(crate) needs_h263video: bool,
    /// 播片 mpeg2v 片源：裸基本流样片拷成 `1080p/video.m2v`
    ///（`[play] format = "mpeg2v"` 用，接入单元切分进 Clip）。
    #[serde(default)]
    pub(crate) needs_mpeg2vvideo: bool,
    /// 播片 theora 片源：Ogg 样片拷成 `1080p/video.ogv`
    ///（`[play] format = "theora"` 用，手写拆包进 Clip）。
    #[serde(default)]
    pub(crate) needs_theoravideo: bool,
    /// 壁纸 avc 片源：1080p MP4 样片拷成 `wallpaper/wall.mp4`
    ///（`[wallpaper] format = "avc"` 用，验壁纸侧容器拆轨 + 预载）。
    #[serde(default)]
    pub(crate) needs_avcwallpaper: bool,
    /// chainload 自举：镜像里再放一份 efi + 一条 BLS entry（单盘场景）。
    #[serde(default)]
    pub(crate) needs_chainload: bool,
    #[serde(default)]
    pub(crate) slow: bool,
    pub(crate) soak_sec: Option<u64>,
    pub(crate) artifact: Option<Artifact>,
    /// 本 case 的固件：缺省跟全局/命令行；"debug" = brew 瓶里的 DEBUG OVMF
    ///（带 ASSERT，抓固件 bug），"release" = 自带 nightly RELEASE
    ///（`assets/ovmf-release-x86_64-code.fd`，无 ASSERT、更快，适合比性能），
    /// 其它值按路径原样用。命令行 `--ovmf/--ovmf-set` 优先于此。
    pub(crate) ovmf: Option<String>,
    /// 本 case 的 CPU 模型：缺省跟全局/命令行；"host" 透传宿主特性，其它按名原样传。
    /// 命令行 `--cpu` 优先于此。
    pub(crate) cpu: Option<String>,
    /// `--changed` 覆盖声明：本 case 锻炼到的仓库根相对路径前缀（文件或目录），
    /// 改动落到其中即选中本 case；`"*"` = 任何改动都跑（只给 menu-smoke 锚点用）。
    /// 缺省空 = 只靠隐式核心（见 CORE）选中，即“纯标定类”case。
    /// 维护：改某子系统代码时顺手核对对应 case 的 covers（和 AGENTS 黑盒规则配套）。
    #[serde(default)]
    pub(crate) covers: Vec<String>,
    /// 附加卷（第二盘及以后）：多卷自动发现的测试夹具，按顺序挂 hd2、hd3…。
    #[serde(default)]
    pub(crate) extra_volumes: Vec<ExtraVolume>,
}

/// 断言项：纯字串 = 出现一次即满足；`{pat,count}` = 出现次数达标
///（chainload 这类"同一镜像起两次"的场景，靠计数区分第几轮）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Expect {
    Once(String),
    Count { pat: String, count: usize },
}

impl Expect {
    pub(crate) fn satisfied(&self, data: &str) -> bool {
        match self {
            Expect::Once(p) => data.contains(p),
            Expect::Count { pat, count } => data.matches(pat).count() >= *count,
        }
    }
    pub(crate) fn describe(&self) -> String {
        match self {
            Expect::Once(p) => format!("{p:?}"),
            Expect::Count { pat, count } => format!("{pat:?} x{count}"),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeyStep {
    pub(crate) send: String,
    #[serde(default = "default_wait")]
    pub(crate) wait_after: u64,
    /// 发键前先等这些串口断言（事件驱动，不靠裸睡；超时即 FAIL）。
    /// 屏显-only 且无标记的界面填空 + `wait_after` 估，case 里注明。
    #[serde(default)]
    pub(crate) wait_for: Vec<Expect>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Artifact {
    pub(crate) glob: String,
    #[serde(default)]
    pub(crate) contains: Vec<String>,
    /// 查哪个盘的落盘工件：缺省 1 = 主盘；2 = 第一附加卷，依此类推
    ///（boot counting 夹具在第二盘，`boot-counting` case 用）。
    #[serde(default = "default_artifact_volume")]
    pub(crate) volume: usize,
}

pub(crate) fn default_artifact_volume() -> usize {
    1
}

/// 附加卷（第二盘及以后）：多卷自动发现的测试夹具。
/// 文件来源三选一：`text` 内联文本 / `zeros` 全零占位 / `efi = true` 拷被测 efi。
#[derive(Debug, Deserialize)]
pub(crate) struct ExtraFile {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) zeros: Option<usize>,
    #[serde(default)]
    pub(crate) efi: bool,
    #[serde(default)]
    pub(crate) asset: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExtraVolume {
    #[serde(default)]
    pub(crate) files: Vec<ExtraFile>,
}
