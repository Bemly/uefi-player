//! 转换编码页：片源 → 固件可播格式，落在图库（`player.toml` 同目录）。
//!
//! 流程：选源（图库文件/`rfd`/示例片）→ 整轨解复用 → 逐组解码→缩放→编码
//! → 图库根目录落盘（默认时间戳名；avc/hevc/vp9 全进 MP4）。
//! 编码在 worker 线程跑（`mpsc` 进度回传），UI 只轮询不阻塞；取消用
//! 原子旗标按组检查。内存纪律见 `codecs.rs` 头注释（关键帧分组 GROUP_CAP=120）。
//! 件库约束：选项行全是 GlassButton；分辨率/重命名用原语 TextEdit、帧率用自家 content_slider
//! （用户拍板：文本框直输比弹系统字典精确；库内无文本件）；
//! 路径显示只用 monospace 文本）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use shiguredo_mp4::boxes::{SampleEntry, VisualSampleEntryFields};
use shiguredo_mp4::Uint;

use crate::app::App;
use crate::codecs::*;
use crate::schema::MediaFormat;

/// 转码目标（八档；输出名即固件认的片源文件名，`format` 开关决定播哪个；
/// `Raw` 为 wallpaper BGRA 序列 `wallpaper/<stem>/frameNNNN.raw`，定死 1920x1080）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscodeTarget {
    Avc,
    Hevc,
    Vp9,
    H263,
    Mpeg2v,
    Theora,
    Qois,
    Raw,
}

impl TranscodeTarget {
    // 顺序与解码页 `DecodeTable::ORDER`/`DecodeTab::ALL` 同序（QOIS/RAW/HEVC…），tab 一行只求可扫读。
    pub(crate) const ALL: [Self; 8] = [
        Self::Qois,
        Self::Raw,
        Self::Hevc,
        Self::Vp9,
        Self::H263,
        Self::Mpeg2v,
        Self::Theora,
        Self::Avc,
    ];
    /// 短名与解码页 tab 同口径：文件串大写（QOIS/RAW/HEVC…）。
    pub(crate) fn file_str(self) -> &'static str {
        match self {
            Self::Qois => "qois",
            Self::Raw => "raw",
            Self::Hevc => "hevc",
            Self::Vp9 => "vp9",
            Self::H263 => "h263",
            Self::Mpeg2v => "mpeg2v",
            Self::Theora => "theora",
            Self::Avc => "avc",
        }
    }
    /// 图库相对路径（固件布局；avc/hevc/vp9 同名 video.mp4，
    /// 播哪个由 [play] format 开关决定；`Raw` 为单文件 BGRA 序列）。
    pub(crate) fn out_path(self) -> &'static str {
        match self {
            Self::Avc => "1080p/video.mp4",
            Self::Hevc => "1080p/video.mp4",
            Self::Vp9 => "1080p/video.mp4",
            Self::H263 => "1080p/video.h263",
            Self::Mpeg2v => "1080p/video.m2v",
            Self::Theora => "1080p/video.ogv",
            Self::Qois => "1080p/video.qois",
            Self::Raw => "1080p/video.raws",
        }
    }
}

/// 分辨率文本解析（纯规则，可单测）：`1920x1080` / `1280*720` / `854X480`；
/// 空串或 `source` = None（保持源几何直通，H263 本来就无视）；非法返回 Err 供 UI 报错。
/// 钳到 16..=4096 且偶数（奇数降一；编码器与写纹理都要求偶数行；4096 档覆盖
/// 1440p/QHD，大于 L4.x（2228224 像素）由各 entry 按分辨率升 level，见下）。
pub(crate) fn parse_res_wh(text: &str) -> Result<Option<(u32, u32)>, String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("source") {
        return Ok(None);
    }
    let sep = t
        .find(|c| c == 'x' || c == 'X' || c == '*')
        .ok_or_else(|| String::from("want WxH like 1920x1080"))?;
    let w: u32 = t[..sep]
        .trim()
        .parse()
        .map_err(|_| String::from("bad width"))?;
    let h: u32 = t[sep + 1..]
        .trim()
        .parse()
        .map_err(|_| String::from("bad height"))?;
    if w < 16 || h < 16 || w > 4096 || h > 4096 {
        return Err(String::from("range 16..=4096"));
    }
    Ok(Some((w & !1, h & !1)))
}

/// 默认输出名：本地时间戳 yymmddhhmmss（纯规则，可单测格式）。
pub(crate) fn default_stem_now() -> String {
    chrono::Local::now().format("%y%m%d%H%M%S").to_string()
}

/// 输出相对路径（落在图库根目录；stem 重命名，扩展名跟目标走；纯规则，可单测）。
/// 空 stem 即按当前时间命名；最终文件名以调用时刻为准（显示行每秒重算，
/// 落盘只在点开始转码时算一次）。
/// 分段解析（纯规则，可单测）。1..=60，默认10。
pub(crate) fn parse_seg(text: &str) -> Result<usize, String> {
    let v: usize = text
        .trim()
        .parse()
        .map_err(|_| String::from("want 1..=60"))?;
    if (1..=SEG_MAX).contains(&v) {
        Ok(v)
    } else {
        Err(String::from("want 1..=60"))
    }
}

pub(crate) fn out_path_for(target: TranscodeTarget, stem: &str) -> String {
    let def = target.out_path();
    let base = def.rsplit('/').next().unwrap_or(def);
    let dot = base.rfind('.');
    let ext = dot.map(|i| &base[i + 1..]).unwrap_or("");
    let t = stem.trim();
    let want = if t.is_empty() {
        return [default_stem_now(), ext.to_string()].join(".");
    } else {
        t
    };
    let want = want
        .rsplit_once('.')
        .map(|(s, _)| s)
        .filter(|s| !s.is_empty())
        .unwrap_or(want);
    let clean: String = want
        .chars()
        .filter(|c| *c != '/' && (*c as u32) != 92)
        .collect();
    let clean = clean.trim();
    let stem = if clean.is_empty() {
        return [default_stem_now(), ext.to_string()].join(".");
    } else {
        clean
    };
    if ext.is_empty() {
        stem.to_string()
    } else {
        [stem, ext].join(".")
    }
}

/// 转码源白名单（mp4 三轨 + 静帧 + 1080p 裸帧；其余格式后继再接）。
pub(crate) fn source_accepted(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".mp4")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".bmp")
        || (lower.starts_with("frame") && lower.ends_with(".raw"))
        || lower.ends_with(".raws")
}

/// 进度消息（worker → UI）。
pub(crate) enum JobMsg {
    Progress { done: usize, total: usize },
    Stage(JobStage),
    Finished(Result<JobOut, String>),
}

/// 转码阶段（进度行前缀，UI 侧配文案）。
#[derive(Debug, Clone, Copy)]
pub(crate) enum JobStage {
    Decode,
    Encode,
    Mux,
}

pub(crate) struct JobOut {
    pub(crate) path: String,
    pub(crate) bytes: usize,
    /// 本 job 内是否发生过硬解回退 CPU（worker 线程标记，UI 侧弹 toast）。
    pub(crate) hw_fallback: bool,
}

pub(crate) struct TranscodeJob {
    pub(crate) rx: Receiver<JobMsg>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) handle: Option<std::thread::JoinHandle<()>>,
}

/// 转码状态（`App` 持有；整文件转码，无时间窗）。
pub(crate) struct TranscodeState {
    pub(crate) source: String,
    pub(crate) target: TranscodeTarget,
    pub(crate) info: String,
    pub(crate) status: String,
    pub(crate) job: Option<TranscodeJob>,
    pub(crate) progress: Option<(usize, usize)>,
    /// 硬解选卡偏好（会话级，不落盘；Win/Linux Vulkan 用，mac 恒 `Auto`）。
    pub(crate) vk_gpu: crate::vulkan_gpu::VulkanGpuPref,
    /// 适配器快照（首帧懒枚举一次缓存；mac 恒空对应 UI 行隐藏）。
    pub(crate) vk_adapters: Option<Vec<crate::vulkan_gpu::VkAdapterView>>,
    /// 输出几何文本（默认 1920x1080；空串=跟源；H263 无视）。
    pub(crate) res_text: String,
    /// 输出帧率滑杆镜像：0 = 跟源，1-240 = 固定帧率（库内滑杆只吃 f32）。
    pub(crate) fps_f: f32,
    /// 编码分段文本（默认10，范围 1..=60）。
    pub(crate) seg_text: String,
    /// 输出重命名（仅 stem，扩展名与子目录固定跟目标走；空串 = 默认名）。
    pub(crate) out_stem: String,
    /// 选源对话框会话（winit macOS 禁止事件处理内再进事件循环，对话框必须
    /// 后台线程跑阻塞版，UI 侧每帧收结果，见 `file_pick` 头注释血泪）。
    pub(crate) pick: crate::file_pick::FilePick,
}

impl TranscodeState {
    pub(crate) fn new() -> Self {
        Self {
            source: String::new(),
            target: TranscodeTarget::Avc,
            info: String::new(),
            status: String::new(),
            job: None,
            progress: None,
            vk_gpu: crate::vulkan_gpu::VulkanGpuPref::Auto,
            vk_adapters: None,
            res_text: String::from("1920x1080"),
            fps_f: 0.0,
            seg_text: String::from("10"),
            out_stem: String::new(),
            pick: crate::file_pick::FilePick::new(),
        }
    }

    pub(crate) fn running(&self) -> bool {
        self.job.is_some()
    }
}

/// 解码组封顶（关键帧自然分组 + 硬切；120 帧 720p ≈ 170MB 瞬时上限，
/// 超了硬切 + 状态栏不断转码）。
pub(crate) const GROUP_CAP: usize = 120;
/// 编码分段上下限：用户可设 1..=60，默认10。段越长文件越小、压缩率越高、
/// seek 越慢，但单段编码时间与内存占用越大（取消等待上限也跟着变长）。
pub(crate) const SEG_MAX: usize = 60;
/// 编码分段上下限：用户可设 1..=60，默认10。
/// 连续解码错误上限（坏流保护，避免无限循环）。
pub(crate) const MAX_DECODE_ERRORS: usize = 16;
/// 输出几何（固件定死 1920x1080）。
pub(crate) const OUT_W: u32 = 1920;
pub(crate) const OUT_H: u32 = 1080;

mod demux;
#[allow(unused_imports)]
pub(crate) use demux::*;
mod decode;
#[allow(unused_imports)]
pub(crate) use decode::*;
mod ui;
#[allow(unused_imports)]
pub(crate) use ui::*;
mod job;
#[allow(unused_imports)]
pub(crate) use job::*;
mod probe;
#[allow(unused_imports)]
pub(crate) use probe::*;
mod mux;
#[allow(unused_imports)]
pub(crate) use mux::*;

#[cfg(test)]
mod transcode_tests {
    use super::*;

    #[test]
    fn default_stem_is_timestamp_shaped() {
        let s = default_stem_now();
        assert_eq!(s.len(), 12, "yymmddhhmmss, got {s}");
        assert!(s.bytes().all(|b| b.is_ascii_digit()), "all digits, got {s}");
    }

    #[test]
    fn out_path_lands_in_gallery_root() {
        // 空 stem 即当前时间戳：12 位数字 + 扩展名，无子目录。
        let p = out_path_for(TranscodeTarget::Avc, "");
        assert!(p.ends_with(".mp4") && !p.contains('/'), "{p}");
        assert_eq!(p.len(), 12 + 4);
        // 重命名 stem：扩展名跟目标走，分隔符被清。
        assert_eq!(out_path_for(TranscodeTarget::Avc, "mymovie"), "mymovie.mp4");
        let w = out_path_for(TranscodeTarget::Avc, "  ");
        assert!(
            w.ends_with(".mp4") && !w.contains('/') && w.len() == 16,
            "{w}"
        );
        assert_eq!(out_path_for(TranscodeTarget::H263, "a/b"), "ab.h263");
        assert_eq!(
            out_path_for(TranscodeTarget::Theora, "mymovie.ogg"),
            "mymovie.ogv"
        );
        // `Raw` 为图库根单文件（与 qois 同口径）。
        assert_eq!(
            out_path_for(TranscodeTarget::Raw, "mymovie"),
            "mymovie.raws"
        );
        let r = out_path_for(TranscodeTarget::Raw, "");
        assert!(r.ends_with(".raws") && !r.contains('/'), "{r}");
    }

    #[test]
    fn parse_res_wh_matrix() {
        assert_eq!(parse_res_wh(""), Ok(None));
        assert_eq!(parse_res_wh("source"), Ok(None));
        assert_eq!(parse_res_wh("1920x1080"), Ok(Some((1920, 1080))));
        assert_eq!(parse_res_wh("1280*720"), Ok(Some((1280, 720))));
        assert_eq!(parse_res_wh("853X480"), Ok(Some((852, 480))));
        // QHD 上限 4096：1440p/4K 宽高放行，超 4096 仍拒。
        assert_eq!(parse_res_wh("2560x1440"), Ok(Some((2560, 1440))));
        assert_eq!(parse_res_wh("4096x2160"), Ok(Some((4096, 2160))));
        assert!(parse_res_wh("abc").is_err());
        assert!(parse_res_wh("10x10").is_err());
        assert!(parse_res_wh("4097x1080").is_err());
        assert!(parse_res_wh("9999x1080").is_err());
    }

    #[test]
    fn mp_level_tier_matrix() {
        // L4.x 上限 2228224 像素（1080p 在内），L5.x 上限 8912896（1440p 在内）。
        assert_eq!(mp_level_tier(1920, 1080), 0);
        assert_eq!(mp_level_tier(2048, 1080), 0);
        assert_eq!(mp_level_tier(2560, 1440), 1);
        assert_eq!(mp_level_tier(4096, 2160), 1);
        assert_eq!(mp_level_tier(4096, 4096), 2);
    }

    #[test]
    fn parse_seg_matrix() {
        assert_eq!(parse_seg("10"), Ok(10));
        assert_eq!(parse_seg(" 60 "), Ok(60));
        assert_eq!(parse_seg("1"), Ok(1));
        assert!(parse_seg("0").is_err());
        assert!(parse_seg("61").is_err());
        assert!(parse_seg("abc").is_err());
        assert!(parse_seg("").is_err());
    }

    #[test]
    fn demux_honours_cancel_flag() {
        // 40 样片进门槛：置位后立即 Err，不拆完整轨。
        let mut frames = Vec::new();
        for i in 0..64u8 {
            frames.push(Yuv420 {
                w: 64,
                h: 48,
                y: vec![i; 64 * 48],
                u: vec![0u8; 32 * 24],
                v: vec![0u8; 32 * 24],
            });
        }
        let st = encode_avc(&frames).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let live = AtomicBool::new(false);
        assert!(demux_groups(&mp4, MediaFormat::Avc, Some(&live)).is_ok());
        let dead = AtomicBool::new(true);
        match demux_groups(&mp4, MediaFormat::Avc, Some(&dead)) {
            Err(e) => assert_eq!(e, String::from("cancelled")),
            Ok(_) => panic!("demux should stop on cancelled flag"),
        }
    }

    #[test]
    fn fps_pick_down_and_up_samples() {
        // 30→15：每 2 输入出 1（4 输入共 2 输出）。
        let mut p = FpsPick::new(30, 15);
        let got: Vec<usize> = (0..4).map(|_| p.copies_for_one_input()).collect();
        assert_eq!(got, vec![0, 1, 0, 1]);
        // 30→60：每输入出 2。
        let mut p = FpsPick::new(30, 60);
        let got: Vec<usize> = (0..3).map(|_| p.copies_for_one_input()).collect();
        assert_eq!(got, vec![2, 2, 2]);
        // 等帧率恒 1。
        let mut p = FpsPick::new(30, 30);
        let got: Vec<usize> = (0..3).map(|_| p.copies_for_one_input()).collect();
        assert_eq!(got, vec![1, 1, 1]);
    }

    #[test]
    fn scale_for_source_passes_through() {
        let f = Yuv420 {
            w: 64,
            h: 48,
            y: vec![0u8; 64 * 48],
            u: vec![0u8; 32 * 24],
            v: vec![0u8; 32 * 24],
        };
        let s = scale_for(&f, TranscodeTarget::Avc, None);
        assert_eq!((s.w, s.h), (64, 48));
        let s = scale_for(&f, TranscodeTarget::Avc, Some((1280, 720)));
        assert_eq!((s.w, s.h), (1280, 720));
        // `Raw` 定死 1920x1080，无视分辨率选项。
        let s = scale_for(&f, TranscodeTarget::Raw, None);
        assert_eq!((s.w, s.h), (OUT_W, OUT_H));
        let s = scale_for(&f, TranscodeTarget::Raw, Some((1280, 720)));
        assert_eq!((s.w, s.h), (OUT_W, OUT_H));
    }

    #[test]
    fn target_output_paths_match_firmware_layout() {
        // avc/hevc/vp9 同名 video.mp4（播哪个由 [play] format 开关决定）。
        for (t, path) in [
            (TranscodeTarget::Avc, "1080p/video.mp4"),
            (TranscodeTarget::Hevc, "1080p/video.mp4"),
            (TranscodeTarget::Vp9, "1080p/video.mp4"),
            (TranscodeTarget::H263, "1080p/video.h263"),
            (TranscodeTarget::Mpeg2v, "1080p/video.m2v"),
            (TranscodeTarget::Theora, "1080p/video.ogv"),
            (TranscodeTarget::Qois, "1080p/video.qois"),
            (TranscodeTarget::Raw, "1080p/video.raws"),
        ] {
            assert_eq!(t.out_path(), path);
        }
        assert_eq!(TranscodeTarget::ALL.len(), 8);
    }

    #[test]
    fn raw_bgra_roundtrip_and_single_file_layout() {
        // RGBA→BGRA 逐字节对调，A 不变；单文件即帧首尾相接（固件整切口径）。
        let px = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
        assert_eq!(rgba_to_bgra(&px), vec![30u8, 20, 10, 40, 70, 60, 50, 80]);
        let dir = std::env::temp_dir().join(format!("uefi-player-raws-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("video.raws");
        let mut f = std::fs::File::create(&path).expect("create raws");
        let n = append_raw_frame(&mut f, &[1u8, 2, 3, 4]).expect("append")
            + append_raw_frame(&mut f, &[5u8, 6, 7, 8]).expect("append");
        drop(f);
        assert_eq!(n, 8);
        assert_eq!(
            std::fs::read(&path).expect("read raws"),
            vec![1u8, 2, 3, 4, 5, 6, 7, 8]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn two_frames() -> Vec<Yuv420> {
        vec![
            Yuv420 {
                w: 64,
                h: 48,
                y: vec![16u8; 64 * 48],
                u: vec![128u8; 32 * 24],
                v: vec![128u8; 32 * 24],
            },
            Yuv420 {
                w: 64,
                h: 48,
                y: vec![235u8; 64 * 48],
                u: vec![128u8; 32 * 24],
                v: vec![128u8; 32 * 24],
            },
        ]
    }

    /// 解回 MP4：样片数 + 关键帧数 + 首样片 entry 種別（固件 demux 同 parser）。
    fn demux_check(mp4: &[u8], want_entry: &str) -> (usize, usize) {
        use shiguredo_mp4::boxes::SampleEntry;
        use shiguredo_mp4::demux::{Input, Mp4FileDemuxer};
        assert_eq!(&mp4[4..8], b"ftyp");
        let mut dx = Mp4FileDemuxer::new();
        dx.handle_input(Input {
            position: 0,
            data: mp4,
        });
        let mut n = 0;
        let mut keys = 0;
        let mut entry = String::new();
        loop {
            let s = dx.next_sample().expect("demux").clone();
            let Some(s) = s else { break };
            if !matches!(s.track.kind, shiguredo_mp4::TrackKind::Video) {
                continue;
            }
            n += 1;
            if s.keyframe {
                keys += 1;
            }
            if entry.is_empty() {
                entry = match s.sample_entry {
                    Some(SampleEntry::Avc1(_)) => "avc1",
                    Some(SampleEntry::Hvc1(_)) => "hvc1",
                    Some(SampleEntry::Hev1(_)) => "hev1",
                    Some(SampleEntry::Vp09(_)) => "vp09",
                    _ => "other",
                }
                .to_string();
            }
        }
        assert_eq!(entry, want_entry);
        (n, keys)
    }

    #[test]
    fn mp4_avc_roundtrip() {
        // less-avc 编 → avc1/avcC 打包 → 同轮子解回。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, keys) = demux_check(&mp4, "avc1");
        assert_eq!(n, 2);
        assert!(keys >= 1);
    }

    #[test]
    fn mp4_hevc_roundtrip() {
        // oxideav-h265 编 → hvc1/hvcC 打包 → 同轮子解回。
        let st = encode_hevc(&two_frames()).expect("hevc");
        let entry = hevc_entry(&st.vps, &st.sps, &st.pps, 64, 48, 30);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, keys) = demux_check(&mp4, "hvc1");
        assert_eq!(n, st.samples.len());
        assert!(keys >= 1);
    }

    #[test]
    fn mp4_vp9_roundtrip() {
        // rusty_vp9 编 → vp09/vpcC 打包（裸帧样片）→ 同轮子解回。
        let pkts = encode_vp9(&two_frames()).expect("vp9");
        let entry = vp9_entry(64, 48);
        let samples: Vec<Mp4OutSample> = pkts
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: x.data.clone(),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let (n, _keys) = demux_check(&mp4, "vp09");
        assert_eq!(n, pkts.len());
    }

    #[test]
    fn probe_reports_mp4_track() {
        // 上面 avc 产出的 mp4 再探：轨种/几何/样片数对得上。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        let info = probe_mp4(&mp4).expect("probe");
        assert_eq!(info.codec, "avc");
        assert_eq!((info.w, info.h), (64, 48));
        assert_eq!(info.n, 2);
    }

    #[test]
    fn res_autofill_follows_probe_geometry() {
        // 选片回填口径：`"{w}x{h}"` 必须能被 `parse_res_wh` 原样吃回。
        for (w, h) in [(1280, 720), (1920, 1080), (64, 48)] {
            let s = format!("{w}x{h}");
            assert_eq!(parse_res_wh(&s), Ok(Some((w, h))), "{s}");
        }
    }

    /// 端到端冒烟：合成小 mp4 落盘 → 走完整 `run_transcode_job`（解复用→
    /// 流式解码→缩放→编码→mux→落盘）→ 产物可被同轮子 demux 回读。
    /// 覆盖生产路径的编排逻辑（进度/阶段/收尾），编解码正确性由各单项
    /// roundtrip 锁死。
    #[test]
    fn run_job_end_to_end_avc() {
        use std::sync::atomic::AtomicBool;
        let dir = std::env::temp_dir().join(format!("uefi-player-tc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.mp4");
        let out = dir.join("out").join("video.mp4");
        // 源：2 帧 avc → mp4（与固件 demux 同 parser 可读）。
        let st = encode_avc(&two_frames()).expect("avc");
        let entry = avc_entry(&st.sps, &st.pps, 64, 48);
        let samples: Vec<Mp4OutSample> = st
            .samples
            .iter()
            .map(|x| Mp4OutSample {
                keyframe: x.keyframe,
                payload: length_prefixed_nals(&x.nalus),
            })
            .collect();
        let mp4 = mux_mp4_video(entry, &samples, 64, 48, 30).expect("mux");
        std::fs::write(&src, &mp4).expect("write src");
        // 整管线（阶段/进度回调只计数，不卡线程）。
        let mut stages = 0;
        let mut last = (0usize, 0usize);
        let cancel = AtomicBool::new(false);
        let r = run_transcode_job(
            &src.display().to_string(),
            TranscodeTarget::Avc,
            &out.display().to_string(),
            &cancel,
            |_| {
                stages += 1;
            },
            |d, t| {
                last = (d, t);
            },
            Some((1920, 1080)),
            None,
            10,
        );
        let out_info = r.expect("run_transcode_job");
        // mac 上 VideoToolbox 常驻，硬解必成：无回退＝编解码硬解链真跑了。
        // 非 mac（无 Vulkan 驱动即回退 CPU）不断言，人肉看日志。
        #[cfg(target_os = "macos")]
        assert!(!out_info.hw_fallback, "expected HW encode+decode on mac");
        assert!(stages >= 2, "want decode+encode/mux stages");
        assert_eq!(last.0, last.1);
        assert!(last.0 >= 2, "want frame-level progress, got {last:?}");
        let produced = std::fs::read(&out_info.path).expect("read output");
        assert!(produced.len() > 1024, "output too small");
        // 产物回读：2 样片 avc1。
        let (n, keys) = demux_check(&produced, "avc1");
        assert_eq!(n, 2);
        assert!(keys >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
