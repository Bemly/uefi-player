//! 黑盒矩阵可测的纯规则（无 display 依赖；`App` 方法只是薄委托，UI 照此摆）。
//! 新增 GUI 规则时先加这里 + 矩阵表，再接 UI（见 AGENTS「黑盒矩阵测试」）。

use crate::schema::*;

/// 微调区四旋钮显隐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TuneVisible {
    pub(crate) frames: bool,
    pub(crate) tile: bool,
    pub(crate) vfps: bool,
    pub(crate) buf: bool,
}

/// 开机矩阵待测（与固件 `config::needs_render_bench` 同逻辑 + half）：
/// Blt 下 workers/dist 不参测，单核下 dist 不参测。
/// `workers`/`dist` 读 `[schedule]`（搬家后位置；`render` 下是 legacy 别名，
/// `coerce_loaded` 已迁移，此处只认新位置）。
pub(crate) fn bench_pending(cfg: &FileConfig) -> bool {
    let (m, s) = (&cfg.render, &cfg.schedule);
    if m.mode == RenderMode::Auto && m.measured_render == RenderMode::Auto {
        return true;
    }
    let render_is_blt = match m.mode {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => m.measured_render == RenderMode::Blt,
    };
    if !render_is_blt && s.workers == WorkersMode::Auto && m.measured_workers == WorkersMode::Auto {
        return true;
    }
    let dist_in_play = !render_is_blt
        && s.workers != WorkersMode::BspOnly
        && !(s.workers == WorkersMode::Auto && m.measured_workers == WorkersMode::BspOnly);
    if dist_in_play && s.dist == DistMode::Auto && m.measured_dist == DistMode::Auto {
        return true;
    }
    // half 自带 bench（冠军计时取半）：无记忆即待测。
    if m.vsync == VsyncMode::Half && m.measured_vsync_fps == 0 {
        return true;
    }
    // Blt 引擎对照操作：选中非 B2V 且 Blt 在测时，单发出数（不参选，只出数）。
    let blt_in_play = match m.mode {
        RenderMode::Blt => true,
        RenderMode::Direct => false,
        RenderMode::Auto => {
            m.measured_render == RenderMode::Auto || m.measured_render == RenderMode::Blt
        }
    };
    if blt_in_play && m.blt_op != BltOpKind::BufferToVideo {
        return true;
    }
    m.vsync == VsyncMode::Auto && m.measured_vsync == VsyncMode::Auto
}

/// 微调区显隐：frames=矩阵有待测；tile=直写全核且分工沾 tile；
/// vfps=static 封顶/auto 达标线；buf=非 Blt（Blt 已是单 push）。
pub(crate) fn tune_visible(cfg: &FileConfig) -> TuneVisible {
    let (m, s) = (&cfg.render, &cfg.schedule);
    let dist_in_play = m.mode != RenderMode::Blt && s.workers != WorkersMode::BspOnly;
    TuneVisible {
        frames: bench_pending(cfg),
        tile: dist_in_play && (s.dist == DistMode::Auto || s.dist == DistMode::Tile),
        vfps: m.vsync == VsyncMode::Auto || m.vsync == VsyncMode::Static,
        buf: m.mode != RenderMode::Blt,
    }
}

/// 读盘钳制：Blt 三个非上屏操作 GUI 置灰不可选，残留值钳回 B2V
/// （播放行为无变化——固件上屏路本来就只认 B2V；手写值想出引擎数
/// 的改完别点保存，或固件侧照读，见 player.md）。
/// `[schedule]` 缺表迁移：老文件 `[render]` 下的 workers/dist 搬到新位置，
/// 原位置复位 Auto（下次存盘即新格式；固件侧同规则，见 `config::load`）。
pub(crate) fn coerce_loaded(mut cfg: FileConfig, has_schedule: bool) -> FileConfig {
    if cfg.render.blt_op != BltOpKind::BufferToVideo {
        cfg.render.blt_op = BltOpKind::BufferToVideo;
    }
    if !has_schedule {
        cfg.schedule.workers = cfg.render.workers;
        cfg.schedule.dist = cfg.render.dist;
        cfg.render.workers = WorkersMode::Auto;
        cfg.render.dist = DistMode::Auto;
    }
    cfg
}

/// 落盘前钳制 tune（滑杆已钳，这里防手改文件后的脏值回写）。
pub(crate) fn clamp_tune(t: &mut RenderTune) {
    t.frames = t.frames.max(1);
    t.tile_rows = t.tile_rows.clamp(8, 512);
    t.vsync_fps = t.vsync_fps.clamp(15, 240);
    t.buffers = t.buffers.min(16);
}

/// 配置黑盒矩阵：GUI 改配置 → 落盘 → 固件生效，全链路只走 TOML 文本 + 纯规则，
/// 无 display 下 `cargo test` 可跑。新增 GUI 规则/档位/字段时先在这里加行
/// （表格即口径），再接 UI 与固件（见 AGENTS「黑盒矩阵测试」；QEMU 侧见
/// `tools/qemu-matrix/matrix.toml`，两边一起更新）。
#[cfg(test)]
mod blackbox_matrix {
    use super::*;

    fn rc(mode: RenderMode, workers: WorkersMode, dist: DistMode, vsync: VsyncMode) -> FileConfig {
        let mut cfg = FileConfig::default();
        cfg.render.mode = mode;
        cfg.render.vsync = vsync;
        cfg.schedule.workers = workers;
        cfg.schedule.dist = dist;
        cfg
    }

    /// 全档位笛卡尔回写：9 accel × 3 render × 3 workers × 4 dist × 4 vsync × 4 blt × 3 async × 3 sched，
    /// 序列化再解析必须逐字节等价（顺序/缺键都由 serde 保证，不许丢字段）。
    #[test]
    fn schema_roundtrip_matrix() {
        let mut n = 0;
        for a in AccelLevel::ALL {
            for m in [RenderMode::Auto, RenderMode::Direct, RenderMode::Blt] {
                for w in WorkersMode::ALL {
                    for d in DistMode::ALL {
                        for v in VsyncMode::ALL {
                            for b in BltOpKind::ALL {
                                for e in AsyncExecutor::ALL {
                                    for s in ScheduleBackend::ALL {
                                        roundtrip_one(a, m, w, d, v, b, e, s);
                                        n += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 9 * 3 * 3 * 4 * 4 * 4 * 3 * 3);
    }

    /// 单组回写断言（从笛卡尔里抽出来：8 层嵌套塞不进 100 列）。
    /// `workers`/`dist` 走新位置 `[schedule]`（`render` 下留 legacy 默认，
    /// 缺表迁移另见 `schedule_roundtrip_matrix`）。
    fn roundtrip_one(
        a: AccelLevel,
        m: RenderMode,
        w: WorkersMode,
        d: DistMode,
        v: VsyncMode,
        b: BltOpKind,
        e: AsyncExecutor,
        s: ScheduleBackend,
    ) {
        let cfg = FileConfig {
            version: 1,
            accel: AccelConfig {
                level: a,
                decode: AccelDecode::default(),
            },
            render: RenderConfig {
                mode: m,
                workers: WorkersMode::Auto,
                dist: DistMode::Auto,
                vsync: v,
                blt_op: b,
                tune: RenderTune {
                    frames: 7,
                    tile_rows: 100,
                    vsync_fps: 77,
                    buffers: 3,
                },
                measured_render: m,
                measured_workers: w,
                measured_dist: d,
                measured_vsync: v,
                measured_vsync_fps: 11,
                measured_cores: 4,
            },
            play: PlayConfig {
                fps: String::from("60"),
                measured_max_fps: 120,
                format: MediaFormat::Raw,
                pipeline: PlayPipeline::Clip,
            },
            wallpaper: WallpaperConfig {
                enable: true,
                fps: String::from("20"),
                frames: 30,
                preload: 4,
                format: MediaFormat::Qois,
                popup_freeze: true,
            },
            boot: BootConfig {
                auto: true,
                windows: true,
                linux: false,
                shell: true,
                uki: false,
                macos: true,
                strict: false,
            },
            r#async: AsyncConfig { executor: e },
            schedule: ScheduleConfig {
                backend: s,
                workers: w,
                dist: d,
            },
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            back, cfg,
            "roundtrip {a:?}/{m:?}/{w:?}/{d:?}/{v:?}/{b:?}/{e:?}/{s:?}"
        );
    }

    /// fps 三态字符串照读照写（非法值不断解析，语义由 `parse_fps` 兜底）。
    #[test]
    fn fps_string_matrix() {
        for s in [
            "auto", "max", "0", "1", "60", "120", "240", "999", "abc", "",
        ] {
            let text = format!("[play]\nfps = \"{s}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.play.fps, s);
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.play.fps, s);
        }
    }

    /// 远古文件（v0.1 时代：无 wallpaper/tune 表）照读，新键全按默认。
    #[test]
    fn compat_legacy_matrix() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(!cfg.wallpaper.enable);
        assert_eq!(cfg.wallpaper.format, MediaFormat::Raw);
        assert!(cfg.wallpaper.popup_freeze);
        assert_eq!(cfg.play.format, MediaFormat::Qois);
        assert_eq!(cfg.render.dist, DistMode::Auto);
        assert_eq!(cfg.render.vsync, VsyncMode::Auto);
        assert_eq!(cfg.render.blt_op, BltOpKind::BufferToVideo);
        assert_eq!(cfg.render.tune.buffers, default_buffers());
        assert_eq!(cfg.render.measured_vsync_fps, 0);
        assert_eq!(cfg.r#async.executor, AsyncExecutor::All);
    }

    /// 片源格式双档位回写（play/wallpaper 各 qois/raw 四组合，不丢字段）。
    /// 弹窗冻结开关同步翻转（8 组合，见 `[wallpaper] popup_freeze`）。
    #[test]
    fn format_roundtrip_matrix() {
        for (pf, wf) in [
            (MediaFormat::Qois, MediaFormat::Qois),
            (MediaFormat::Qois, MediaFormat::Raw),
            (MediaFormat::Raw, MediaFormat::Qois),
            (MediaFormat::Raw, MediaFormat::Raw),
        ] {
            for froze in [false, true] {
                let mut cfg = FileConfig::default();
                cfg.play.format = pf;
                cfg.wallpaper.format = wf;
                cfg.wallpaper.popup_freeze = froze;
                let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
                assert_eq!(back.play.format, pf);
                assert_eq!(back.wallpaper.format, wf);
                assert_eq!(back.wallpaper.popup_freeze, froze);
                assert_eq!(back, cfg);
                // 老文件缺键：play 按 qois、wallpaper 按 raw/冻结照读。
                let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
                assert_eq!(legacy.play.format, MediaFormat::Qois);
                assert_eq!(legacy.wallpaper.format, MediaFormat::Raw);
                assert!(legacy.wallpaper.popup_freeze);
            }
        }
    }

    /// 预载帧数回写（0 = 不封顶，1/8/32 照写；老文件缺键按默认 8 照读）。
    #[test]
    fn preload_roundtrip_matrix() {
        for p in [0, 1, 8, 32] {
            let mut cfg = FileConfig::default();
            cfg.wallpaper.preload = p;
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.wallpaper.preload, p);
            assert_eq!(back, cfg);
        }
        let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
        assert_eq!(legacy.wallpaper.preload, 8);
    }

    /// 壁纸帧率三态字符串照读照写（与播放 `fps_string_matrix` 同构；
    /// 整数老文件（`fps = 15`）照读为 `"15"`，不断旧配置）。
    #[test]
    fn wallpaper_fps_string_matrix() {
        for s in ["auto", "max", "15", "60", "240", "999", "abc", ""] {
            let text = format!("[wallpaper]\nfps = \"{s}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.wallpaper.fps, s);
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.wallpaper.fps, s);
        }
        // 整数即字符串（固件 `de_wallpaper_fps` 同款，host/guest 隔离故各测一遍）。
        let cfg: FileConfig = toml::from_str("[wallpaper]\nfps = 15\n").unwrap();
        assert_eq!(cfg.wallpaper.fps, "15");
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.wallpaper.fps, "15");
    }

    /// 播放管线八档位回写（clip/staged/zero_copy/stream/single/legacy 三档，
    /// 不丢字段；老文件缺键按 clip 照读，现路行为不变）。
    #[test]
    fn pipeline_roundtrip_matrix() {
        for p in PlayPipeline::ALL {
            let mut cfg = FileConfig::default();
            cfg.play.pipeline = p;
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.play.pipeline, p);
            assert_eq!(back, cfg);
        }
        let legacy: FileConfig = toml::from_str("version = 1\n[play]\nfps = \"max\"\n").unwrap();
        assert_eq!(legacy.play.pipeline, PlayPipeline::Clip);
    }

    /// 执行器线三档位回写（all/nano/dynamic，不丢字段；老文件缺键按 all 照读）。
    #[test]
    fn async_roundtrip_matrix() {
        for ex in AsyncExecutor::ALL {
            let mut cfg = FileConfig::default();
            cfg.r#async.executor = ex;
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.r#async.executor, ex);
            assert_eq!(back, cfg);
            // 老文件缺键：执行器线按 all 照读（诊断项双线全跑）。
            let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
            assert_eq!(legacy.r#async.executor, AsyncExecutor::All);
        }
    }

    /// 空文件全默认（缺表缺键一路 Default，不 halt）。
    #[test]
    fn compat_empty_matrix() {
        let cfg: FileConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, FileConfig::default());
    }

    /// `[boot]` 缺表：老文件照读，自动发现全开（默认行为就是发现）。
    #[test]
    fn boot_compat_matrix() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(cfg.boot.auto);
        assert!(cfg.boot.windows);
        assert!(cfg.boot.linux);
        assert!(cfg.boot.shell);
        assert!(cfg.boot.uki);
        assert!(cfg.boot.macos);
        assert!(!cfg.boot.strict);
    }

    /// `[boot]` 6 开关全笛卡尔回写（128 种）。
    #[test]
    fn boot_roundtrip_matrix() {
        let mut n = 0;
        for bits in 0..128u8 {
            let mut cfg = FileConfig::default();
            cfg.boot.auto = bits & 1 != 0;
            cfg.boot.windows = bits & 2 != 0;
            cfg.boot.linux = bits & 4 != 0;
            cfg.boot.shell = bits & 8 != 0;
            cfg.boot.uki = bits & 16 != 0;
            cfg.boot.macos = bits & 32 != 0;
            cfg.boot.strict = bits & 64 != 0;
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back, cfg, "{bits:07b}");
            n += 1;
        }
        assert_eq!(n, 128);
    }

    /// 读盘钳制矩阵：只有 B2V 能留，其余三项一律钳回 B2V（播放行为不变）。
    #[test]
    fn coerce_matrix() {
        for (input, expected) in [
            ("buffer_to_video", BltOpKind::BufferToVideo),
            ("video_to_video", BltOpKind::BufferToVideo),
            ("video_fill", BltOpKind::BufferToVideo),
            ("video_to_blt_buffer", BltOpKind::BufferToVideo),
        ] {
            let text = format!("[render]\nblt_op = \"{input}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            // 该文本无 `[schedule]` 表（has_schedule=false，走缺表迁移，不影响 blt 断言）。
            assert_eq!(coerce_loaded(cfg, false).render.blt_op, expected, "{input}");
        }
    }

    /// 落盘钳制矩阵：越界钳住，正常值不动。
    #[test]
    fn clamp_matrix() {
        let mut t = RenderTune {
            frames: 0,
            tile_rows: 4,
            vsync_fps: 999,
            buffers: 99,
        };
        clamp_tune(&mut t);
        assert_eq!(
            t,
            RenderTune {
                frames: 1,
                tile_rows: 8,
                vsync_fps: 240,
                buffers: 16
            }
        );
        let mut ok = RenderTune {
            frames: 30,
            tile_rows: 64,
            vsync_fps: 60,
            buffers: 2,
        };
        clamp_tune(&mut ok);
        assert_eq!(
            ok,
            RenderTune {
                frames: 30,
                tile_rows: 64,
                vsync_fps: 60,
                buffers: 2
            }
        );
    }

    /// 待测判定矩阵（与固件 `needs_render_bench` 同逻辑 + half）。
    /// 行：(mode, workers, dist, vsync, measured..., cores, half_fps, 期待)。
    #[test]
    fn bench_pending_matrix() {
        // 基线：全 auto 无记忆 → 必测。
        assert!(bench_pending(&FileConfig::default()));
        // 全显式（Direct/BspOnly/Static/Off）→ 跳过。
        let mut m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Off,
        );
        assert!(!bench_pending(&m));
        // Direct + workers auto 无记忆 → 测 workers。
        m = rc(
            RenderMode::Direct,
            WorkersMode::Auto,
            DistMode::Auto,
            VsyncMode::Off,
        );
        assert!(bench_pending(&m));
        // workers 已记忆 BspOnly → dist 不参测 → 跳过。
        m.render.measured_workers = WorkersMode::BspOnly;
        assert!(!bench_pending(&m));
        // Direct + 全核 + dist auto 无记忆 → 测 dist。
        m = rc(
            RenderMode::Direct,
            WorkersMode::All,
            DistMode::Auto,
            VsyncMode::Off,
        );
        assert!(bench_pending(&m));
        // dist 已记忆 → 跳过。
        m.render.measured_dist = DistMode::Static;
        assert!(!bench_pending(&m));
        // Blt 定死：workers/dist 不参测（auto 也白搭）。
        m = rc(
            RenderMode::Blt,
            WorkersMode::Auto,
            DistMode::Auto,
            VsyncMode::Off,
        );
        assert!(!bench_pending(&m));
        // mode auto 无记忆 → 必测（哪怕 Blt 倾向，首轮也得跑）。
        m = rc(
            RenderMode::Auto,
            WorkersMode::Auto,
            DistMode::Auto,
            VsyncMode::Off,
        );
        assert!(bench_pending(&m));
        // mode auto 已记忆 Direct + workers auto 无记忆 → 测 workers。
        m.render.measured_render = RenderMode::Direct;
        assert!(bench_pending(&m));
        // mode auto 已记忆 Blt + workers auto 无记忆 → workers 被忽略 → 跳过。
        m.render.measured_render = RenderMode::Blt;
        assert!(!bench_pending(&m));
        // vsync auto 无记忆 → 测（其余全显式）。
        m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Auto,
        );
        assert!(bench_pending(&m));
        // vsync auto 已记忆 → 跳过。
        m.render.measured_vsync = VsyncMode::Static;
        assert!(!bench_pending(&m));
        // half 无记忆 → 必测（其余全显式）。
        m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Half,
        );
        assert!(bench_pending(&m));
        // half 已记忆 → 跳过。
        m.render.measured_vsync_fps = 45;
        assert!(!bench_pending(&m));
        // 显式 half + dist auto 无记忆（全核）→ dist 照测。
        m = rc(
            RenderMode::Direct,
            WorkersMode::All,
            DistMode::Auto,
            VsyncMode::Half,
        );
        m.render.measured_vsync_fps = 45;
        assert!(bench_pending(&m));
        // Blt 引擎对照：Blt 在测 + 选中非 B2V → 必测（只出数）。
        m = rc(
            RenderMode::Blt,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Off,
        );
        m.render.blt_op = BltOpKind::VideoToBltBuffer;
        assert!(bench_pending(&m));
        // 同上但选中 B2V → B2V 格本就覆盖，无额外待测。
        m.render.blt_op = BltOpKind::BufferToVideo;
        assert!(!bench_pending(&m));
        // Blt 不在测（Direct 定死）→ 选中啥都不测 op 格。
        m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Off,
        );
        m.render.blt_op = BltOpKind::VideoFill;
        assert!(!bench_pending(&m));
        // mode auto 已记忆 Blt + 选中非 B2V → Blt 在测 → 测 op 格。
        m = rc(
            RenderMode::Auto,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Off,
        );
        m.render.measured_render = RenderMode::Blt;
        m.render.blt_op = BltOpKind::VideoFill;
        assert!(bench_pending(&m));
    }

    /// 微调显隐矩阵：行 = (mode, workers, dist, vsync) → (frames, tile, vfps, buf)。
    #[test]
    fn tune_visible_matrix() {
        // 全 auto 无记忆：全摆。
        assert_eq!(
            tune_visible(&FileConfig::default()),
            TuneVisible {
                frames: true,
                tile: true,
                vfps: true,
                buf: true
            }
        );
        // Blt 定死：只剩 vfps（auto）+ frames（render auto 无记忆）。
        let m = rc(
            RenderMode::Blt,
            WorkersMode::Auto,
            DistMode::Auto,
            VsyncMode::Auto,
        );
        assert_eq!(
            tune_visible(&m),
            TuneVisible {
                frames: true,
                tile: false,
                vfps: true,
                buf: false
            }
        );
        // 全显式 Direct/BspOnly/Static/Off：只剩 buf。
        let m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Off,
        );
        assert_eq!(
            tune_visible(&m),
            TuneVisible {
                frames: false,
                tile: false,
                vfps: false,
                buf: true
            }
        );
        // tile 沾边（auto/Tile）→ tile 摆；static/dynamic 藏。
        for (d, tile) in [
            (DistMode::Auto, true),
            (DistMode::Tile, true),
            (DistMode::Static, false),
            (DistMode::Dynamic, false),
        ] {
            let mut m = rc(RenderMode::Direct, WorkersMode::All, d, VsyncMode::Off);
            // dist 无记忆会点亮 frames，先给记忆压住，只看 tile。
            m.render.measured_dist = DistMode::Static;
            assert_eq!(
                tune_visible(&m),
                TuneVisible {
                    frames: false,
                    tile,
                    vfps: false,
                    buf: true
                },
                "{d:?}"
            );
        }
        // vfps 只在 static/auto 摆；half/off 藏（half 自带预算）。
        for (v, vfps) in [
            (VsyncMode::Auto, true),
            (VsyncMode::Static, true),
            (VsyncMode::Half, false),
            (VsyncMode::Off, false),
        ] {
            let mut m = rc(
                RenderMode::Direct,
                WorkersMode::BspOnly,
                DistMode::Static,
                v,
            );
            // 无关记忆先压住（auto 无记忆会点亮 frames，half 无记忆同理），只看 vfps。
            m.render.measured_vsync = VsyncMode::Static;
            m.render.measured_vsync_fps = 45;
            let vis = tune_visible(&m);
            assert_eq!(vis.vfps, vfps, "{v:?}");
            assert!(!vis.frames && !vis.tile && vis.buf, "{v:?}");
        }
        // half 无记忆 → frames 亮（自带 bench）。
        let m = rc(
            RenderMode::Direct,
            WorkersMode::BspOnly,
            DistMode::Static,
            VsyncMode::Half,
        );
        assert!(tune_visible(&m).frames);
    }

    /// `[schedule]` 三档位回写 + 缺表迁移：36 组合不断字段；
    /// 老文件（`[render]` 旧键、无 `[schedule]` 表）迁到新位置，原位置复位。
    #[test]
    fn schedule_roundtrip_matrix() {
        for b in ScheduleBackend::ALL {
            for w in WorkersMode::ALL {
                for d in DistMode::ALL {
                    let mut cfg = FileConfig::default();
                    cfg.schedule.backend = b;
                    cfg.schedule.workers = w;
                    cfg.schedule.dist = d;
                    let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
                    assert_eq!(back.schedule.backend, b);
                    assert_eq!(back.schedule.workers, w);
                    assert_eq!(back.schedule.dist, d);
                    assert_eq!(back, cfg);
                }
            }
        }
        // 老文件：`[render]` 旧键 + 无 `[schedule]` 表 → 迁到新位置。
        let old: FileConfig = toml::from_str(
            "version = 1\n[render]\nmode = \"direct\"\nworkers = \"all\"\ndist = \"tile\"\n",
        )
        .unwrap();
        assert_eq!(old.render.workers, WorkersMode::All);
        let moved = coerce_loaded(old, false);
        assert_eq!(moved.schedule.workers, WorkersMode::All);
        assert_eq!(moved.schedule.dist, DistMode::Tile);
        assert_eq!(moved.render.workers, WorkersMode::Auto);
        assert_eq!(moved.render.dist, DistMode::Auto);
        // 新文件：`[schedule]` 表在场 → `[render]` 残留不动（固件侧同规则）。
        let new: FileConfig = toml::from_str(
            "version = 1\n[render]\nworkers = \"all\"\n[schedule]\nbackend = \"nano\"\n",
        )
        .unwrap();
        let kept = coerce_loaded(new, true);
        assert_eq!(kept.schedule.backend, ScheduleBackend::Nano);
        assert_eq!(kept.render.workers, WorkersMode::All);
    }

    /// `[accel.decode]` 三键回写（3³ = 27 组合；不进主笛卡尔，避免 27 倍膨胀）+
    /// 老文件缺表缺键按 auto 照读（= 跟随全局，不炸旧配置）。
    #[test]
    fn decode_roundtrip_matrix() {
        let mut n = 0;
        for h in DecodeSimd::ALL {
            for v in DecodeSimd::ALL {
                for g in DecodeSimd::ALL {
                    let mut cfg = FileConfig::default();
                    cfg.accel.decode.hevc = h;
                    cfg.accel.decode.vp9 = v;
                    cfg.accel.decode.h263 = g;
                    let back: FileConfig =
                        toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
                    assert_eq!(back.accel.decode.hevc, h);
                    assert_eq!(back.accel.decode.vp9, v);
                    assert_eq!(back.accel.decode.h263, g);
                    assert_eq!(back, cfg);
                    n += 1;
                }
            }
        }
        assert_eq!(n, 27);
        // 老文件：无 `[accel.decode]` 表 → 三键全 auto（跟随全局）。
        let legacy: FileConfig =
            toml::from_str("version = 1\n[accel]\nlevel = \"scalar\"\n").unwrap();
        assert_eq!(legacy.accel.decode.hevc, DecodeSimd::Auto);
        assert_eq!(legacy.accel.decode.vp9, DecodeSimd::Auto);
        assert_eq!(legacy.accel.decode.h263, DecodeSimd::Auto);
        // 半表：只写一键，其余 auto。
        let partial: FileConfig = toml::from_str("[accel.decode]\nvp9 = \"max\"\n").unwrap();
        assert_eq!(partial.accel.decode.vp9, DecodeSimd::Max);
        assert_eq!(partial.accel.decode.hevc, DecodeSimd::Auto);
        assert_eq!(partial.accel.decode.h263, DecodeSimd::Auto);
    }
}
