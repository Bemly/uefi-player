//! 黑盒矩阵可测的纯规则（无 UI 依赖；`App` 方法只是薄委托，UI 照此摆）。
//! 新增 GUI 规则时先加这里 + 矩阵表，再接 UI（见 AGENTS「黑盒矩阵测试」）。
//!
//! P5 触达拆分（本文件已超 1000 行红线）：纯规则按域下沉到
//! `rules_bench`（待测/显隐）与 `rules_coerce`（读盘/落盘钳制，
//! 含 `[play] media_dir`）；本文件只剩重导出 + `blackbox_matrix`
//! 表格测试，行为零改（对外 `crate::rules::*` 路径不变）。

pub(crate) use crate::rules_bench::*;
pub(crate) use crate::rules_coerce::*;

/// 配置黑盒矩阵：GUI 改配置 → 落盘 → 固件生效，全链路只走 TOML 文本 + 纯规则，
/// 无 display 下 `cargo test` 可跑。新增 GUI 规则/档位/字段时先在这里加行
/// （表格即口径），再接 UI 与固件（见 AGENTS「黑盒矩阵测试」；QEMU 侧见
/// `tools/qemu-matrix/matrix.toml`，两边一起更新）。
#[cfg(test)]
mod blackbox_matrix {
    use super::*;
    use crate::schema::*;
    // RAW-GEOM：`DecodeConfig` 已拆到 `decode_cfg`（`schema` 只重导出 `DecodeTable`）。
    use crate::decode_cfg::DecodeConfig;

    fn rc(mode: RenderMode, workers: WorkersMode, dist: DistMode, vsync: VsyncMode) -> FileConfig {
        let mut cfg = FileConfig::default();
        cfg.render.mode = mode;
        cfg.render.vsync = vsync;
        cfg.schedule.workers = workers;
        cfg.schedule.dist = dist;
        cfg
    }

    /// 全档位笛卡尔回写：9 代表档 × 3 render × 3 workers × 4 dist × 4 vsync × 4 blt × 3 async × 3 sched，
    /// 序列化再解析必须逐字节等价（顺序/缺键都由 serde 保证，不许丢字段）。
    /// 代表集见 `AccelLevel::REPRESENTATIVE`（性能序时代仍占 9 席，不膨胀；
    /// 全量 31 档另见 `accel_levels_roundtrip` 表，不进主笛卡尔，避 3 倍膨胀）。
    #[test]
    fn schema_roundtrip_matrix() {
        let mut n = 0;
        for a in AccelLevel::REPRESENTATIVE {
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
    /// `workers`/`dist` 只认新位置 `[schedule]`（`render` 下同名键已删）。
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
            accel: AccelConfig { level: a },
            render: RenderConfig {
                mode: m,
                vsync: v,
                blt_op: b,
                tune: RenderTune {
                    frames: 7,
                    tile_rows: 100,
                    vsync_fps: 77,
                    buffers: 3,
                    window: 4,
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
                feed: FeedMode::Clip,
                media_dir: String::from("1080p"),
            },
            playlist: PlaylistConfig {
                loop_mode: LoopMode::LoopList,
                items: vec![
                    PlaylistItem {
                        path: String::from("1080p/video.mp4"),
                    },
                    PlaylistItem {
                        path: String::from("1080p/video.hevc"),
                    },
                    PlaylistItem {
                        path: String::from("1080p/video.qois"),
                    },
                ],
            },
            decode: DecodeTable::default(),
            wallpaper: WallpaperListConfig {
                enable: true,
                fps: String::from("20"),
                preload: 4,
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
                cores: 8,
                roles: RolesMode::Split,
                measured_workers_n: 6,
            },
            display: Default::default(),
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

    /// 远古文件（v0.1 时代：无 wallpaper/playlist/tune/decode 表）照读，新键全按默认。
    #[test]
    fn compat_legacy_matrix() {
        let text = "version = 1\n[accel]\nlevel = \"auto\"\n[render]\nmode = \"auto\"\n[play]\nfps = \"auto\"\n";
        let cfg: FileConfig = toml::from_str(text).unwrap();
        assert!(!cfg.wallpaper.enable);
        assert!(cfg.wallpaper.popup_freeze);
        assert!(cfg.playlist.items.is_empty());
        assert_eq!(cfg.playlist.loop_mode, LoopMode::LoopList);
        assert_eq!(cfg.play.feed, FeedMode::Clip);
        // P5：`[play] media_dir` 缺键按 `""`（根目录）照读。
        assert_eq!(cfg.play.media_dir, "");
        assert_eq!(cfg.schedule.dist, DistMode::Auto);
        assert_eq!(cfg.schedule.workers, WorkersMode::Auto);
        assert_eq!(cfg.schedule.cores, 0);
        assert_eq!(cfg.schedule.roles, RolesMode::Auto);
        assert_eq!(cfg.schedule.measured_workers_n, 0);
        assert_eq!(cfg.render.vsync, VsyncMode::Auto);
        assert_eq!(cfg.render.blt_op, BltOpKind::BufferToVideo);
        assert_eq!(cfg.render.tune.buffers, default_buffers());
        assert_eq!(cfg.render.measured_vsync_fps, 0);
        assert_eq!(cfg.r#async.executor, AsyncExecutor::All);
        // 无 `[decode]` 表：8 格式全默认（bsp_only/关/关/跟随窗口/继承全局供片）。
        for fmt in DecodeTable::ORDER {
            let d = cfg.decode.get(fmt).unwrap();
            assert_eq!(d.threads, DecodeThreads::BspOnly);
            assert!(!d.zero_copy);
            assert!(!d.streaming);
            assert_eq!(d.buffers, 0);
            assert_eq!(d.fetch, None);
        }
    }

    /// 画廊循环三档回写（`loop_single`/`loop_list`/`once` × 冻结开关，
    /// 3×2 = 6 组合不断字段；壁纸画廊语义：显示哪几个/多少个/什么顺序/是否循环，
    /// 不断言播放器供片语义——供片改走 `[play] media_dir` 整目录）。
    /// （旧 `format_roundtrip_matrix` 已删：`[play] format`/
    /// `[wallpaper] format,frames` 键随播放页一并删除，解码 tabs 见下。）
    #[test]
    fn playlist_loop_modes_matrix() {
        let mut n = 0;
        for m in LoopMode::ALL {
            for froze in [false, true] {
                let mut cfg = FileConfig::default();
                cfg.playlist.loop_mode = m;
                cfg.wallpaper.popup_freeze = froze;
                let text = toml::to_string(&cfg).unwrap();
                assert!(
                    text.contains(&format!("loop = \"{}\"", m.file_str())),
                    "{m:?} missing in {text}"
                );
                let back: FileConfig = toml::from_str(&text).unwrap();
                assert_eq!(back.playlist.loop_mode, m);
                assert_eq!(back.wallpaper.popup_freeze, froze);
                assert_eq!(back, cfg);
                n += 1;
            }
        }
        assert_eq!(n, 3 * 2);
        // 老文件缺键：画廊 `loop` 按 `loop_list` + 空列表照读。
        let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
        assert_eq!(legacy.playlist.loop_mode, LoopMode::LoopList);
        assert!(legacy.playlist.items.is_empty());
        assert!(legacy.wallpaper.popup_freeze);
        // 旧 `[play] format` / `[wallpaper] format,frames,loop,items` 残留直接忽略，不断解析。
        let old: FileConfig = toml::from_str(
            "version = 1\n[play]\nfps = \"max\"\nformat = \"raw\"\n[wallpaper]\nformat = \"qois\"\nframes = 30\nloop = \"once\"\n[[wallpaper.items]]\npath = \"wallpaper/wall.mp4\"\n",
        )
        .unwrap();
        assert_eq!(old.play.fps, "max");
        assert!(old.playlist.items.is_empty());
        assert_eq!(old.playlist.loop_mode, LoopMode::LoopList);
    }

    /// 画廊多条目回写（`playlist-list-loop`）、单条循环
    /// （`playlist-loop-single`）、播完即停（`playlist-once`）与画廊归属
    /// （P5 起 `[playlist]` 即壁纸画廊：显示哪几个/多少个/什么顺序/是否循环；
    /// 供片改走 `[play] media_dir`，不断言播放器语义）见 `playlist.rs`
    /// 同名测试（列表归属页，黑盒仍纯 TOML 无 display，`cargo test` 同跑）。

    /// `[play] media_dir` 回写矩阵（缺省/显式/去空格/超长截断；序列化原样，App 落盘前调同一钳制）。
    /// 纯规则函数（`clamp_media_dir`/`coerce_loaded` 先行），无 display 依赖。
    #[test]
    fn media_dir_roundtrip_matrix() {
        // 缺省：缺键老文件按 `""`（根目录）照读。
        let legacy: FileConfig = toml::from_str("version = 1\n[play]\nfps = \"max\"\n").unwrap();
        assert_eq!(legacy.play.media_dir, "");
        // 显式值照读照写。
        for s in ["", "1080p", "wallpaper/hd", "a/b/c"] {
            let mut cfg = FileConfig::default();
            cfg.play.media_dir = String::from(s);
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.play.media_dir, s, "{s:?}");
            assert_eq!(back, cfg);
        }
        // 读盘钳制：去首尾空格（合法值不警告）。
        let spaced: FileConfig = toml::from_str("[play]\nmedia_dir = \"  1080p  \"\n").unwrap();
        assert_eq!(coerce_loaded(spaced).play.media_dir, "1080p");
        let mut v = String::from("1080p");
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, "1080p");
        // 超长截断：65 字符 → 前 64 + 警告；64 字符合法不警告。
        let ok64: String = std::iter::repeat_n('a', 64).collect();
        let bad65: String = std::iter::repeat_n('a', 65).collect();
        let mut v = ok64.clone();
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, ok64);
        let mut v = bad65.clone();
        assert!(clamp_media_dir(&mut v));
        assert_eq!(v, ok64);
        // 中文按字符数（64 汉字合法，65 截断；非字节口径，不切断 UTF-8）。
        let ok_zh: String = std::iter::repeat_n('片', 64).collect();
        let mut v = ok_zh.clone();
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, ok_zh);
        let mut v = std::iter::repeat_n('片', 65).collect::<String>();
        assert!(clamp_media_dir(&mut v));
        assert_eq!(v, ok_zh);
        // 读盘路径同样截断（`coerce_loaded`）。
        let loaded: FileConfig =
            toml::from_str(&format!("[play]\nmedia_dir = \"{bad65}\"\n")).unwrap();
        assert_eq!(coerce_loaded(loaded).play.media_dir, ok64);
        // 序列化原样：超长值 `to_string` 不截断（钳制在读盘 coerce + App 落盘前，两处调同一函数）。
        let mut cfg = FileConfig::default();
        cfg.play.media_dir = bad65.clone();
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains(&bad65), "write as-is in {text}");
    }

    /// 解码 tabs 回写（8 格式 × 线程 2 档采样：tab 只切展示，落盘八表齐全；
    /// 全量组合见 `decode_roundtrip_matrix`，此处断 tab 序与落盘键）。
    #[test]
    fn decode_tabs_matrix() {
        assert_eq!(DecodeTable::ORDER.len(), 8);
        assert_eq!(DecodeTable::ORDER, MediaFormat::ALL);
        // 顶栏 tab 序与 ORDER 同步（首位全局 + 八格式；漂移了顶栏就错位）。
        let tabs = crate::app::DecodeTab::ALL;
        assert_eq!(tabs.len(), 9);
        assert!(matches!(tabs[0], crate::app::DecodeTab::Global));
        for (tab, fmt) in tabs[1..].iter().zip(DecodeTable::ORDER.iter()) {
            assert!(matches!(tab, crate::app::DecodeTab::Format(f) if f == fmt));
        }
        let mut cfg = FileConfig::default();
        for (i, fmt) in DecodeTable::ORDER.iter().copied().enumerate() {
            let d = cfg.decode.get_mut(fmt).unwrap();
            d.threads = if i % 2 == 0 {
                DecodeThreads::BspOnly
            } else {
                DecodeThreads::All
            };
            d.buffers = i as u32;
        }
        let text = toml::to_string(&cfg).unwrap();
        // 八表节名齐全（`[decode.<fmt>]`），逐表线程/缓冲不断字段。
        for fmt in DecodeTable::ORDER {
            assert!(
                text.contains(&format!("[decode.{}]", fmt.file_str())),
                "{fmt:?} missing in {text}"
            );
        }
        let back: FileConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
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

    /// 壁纸帧率纯字符串三态照读照写（与播放 `fps_string_matrix` 同构）。
    #[test]
    fn wallpaper_fps_string_matrix() {
        for s in ["auto", "max", "15", "60", "240", "999", "abc", ""] {
            let text = format!("[wallpaper]\nfps = \"{s}\"\n");
            let cfg: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(cfg.wallpaper.fps, s);
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back.wallpaper.fps, s);
        }
    }

    #[test]
    fn wallpaper_fps_int_rejected() {
        // 整数老文件（`fps = 15`）不再兼容：整文件解析失败 → 调用方走全默认 + 警告。
        assert!(toml::from_str::<FileConfig>("[wallpaper]\nfps = 15\n").is_err());
    }

    /// 供片五档 × 窗口代表值回写（clip/staged/stream/zero_copy/single × 1/4/16/32，
    /// 不断字段；老文件缺键按 clip/4 照读，现路行为不变）。
    /// 主笛卡尔不加 feed 维（×5 即 23 万组合，全量 5 档在此全盖，不存在代表集缺口）。
    #[test]
    fn feed_roundtrip_matrix() {
        let mut n = 0;
        for f in FeedMode::ALL {
            for w in [1, 4, 16, 32] {
                let mut cfg = FileConfig::default();
                cfg.play.feed = f;
                cfg.render.tune.window = w;
                let text = toml::to_string(&cfg).unwrap();
                assert!(text.contains(f.file_str()), "{f:?} missing in {text}");
                let back: FileConfig = toml::from_str(&text).unwrap();
                assert_eq!(back.play.feed, f);
                assert_eq!(back.render.tune.window, w);
                assert_eq!(back, cfg);
                n += 1;
            }
        }
        assert_eq!(n, 5 * 4);
        // 老文件缺键：供片按 clip、窗口按 4 照读（现行为不变）。
        let legacy: FileConfig = toml::from_str("version = 1\n[play]\nfps = \"max\"\n").unwrap();
        assert_eq!(legacy.play.feed, FeedMode::Clip);
        let legacy_tune: FileConfig =
            toml::from_str("version = 1\n[render.tune]\nframes = 30\n").unwrap();
        assert_eq!(legacy_tune.render.tune.window, default_window());
        // 老 `[play] pipeline` 残留键直接忽略（未知键 serde 默认忽略，不断解析）。
        let pipelined: FileConfig =
            toml::from_str("version = 1\n[play]\nfps = \"max\"\npipeline = \"single\"\n").unwrap();
        assert_eq!(pipelined.play.feed, FeedMode::Clip);
    }

    /// 解码表回写（8 格式 × threads 2 档 × 零拷/流式开关 × 缓冲代表值 × 供片采样，
    /// 8×2×2×2×4×4 = 1024 组合不断字段；`fetch = None`（继承）落盘不写键，
    /// `Some` 显式五档照读照写；老文件缺表/缺键全按默认照读）。
    /// 主笛卡尔不加全 fetch 维（×6 即 1536 组合仍可跑，但全量 5 档在此采样 + 下方
    /// `decode_fetch_all_modes` 全盖，不存在代表集缺口；避主笛卡尔 23 万级膨胀同理）。
    #[test]
    fn decode_roundtrip_matrix() {
        const FETCH_SAMPLES: [Option<FeedMode>; 4] = [
            None,
            Some(FeedMode::Clip),
            Some(FeedMode::Stream),
            Some(FeedMode::Single),
        ];
        let mut n = 0;
        for fmt in DecodeTable::ORDER {
            for t in DecodeThreads::ALL {
                for zc in [false, true] {
                    for st in [false, true] {
                        for b in [0, 1, 4, 32] {
                            for f in FETCH_SAMPLES {
                                let mut cfg = FileConfig::default();
                                {
                                    let d = cfg.decode.get_mut(fmt).unwrap();
                                    d.threads = t;
                                    d.zero_copy = zc;
                                    d.streaming = st;
                                    d.buffers = b;
                                    d.fetch = f;
                                }
                                let text = toml::to_string(&cfg).unwrap();
                                assert!(text.contains(fmt.file_str()), "{fmt:?} missing in {text}");
                                // 继承必须写 NO 键（TOML 干净），显式必须写键。
                                if f.is_none() {
                                    assert!(
                                        !text.contains("fetch"),
                                        "{fmt:?} inherit must not write fetch key in {text}"
                                    );
                                } else {
                                    assert!(
                                        text.contains(&format!(
                                            "fetch = \"{}\"",
                                            f.unwrap().file_str()
                                        )),
                                        "{fmt:?}/{f:?} missing fetch key in {text}"
                                    );
                                }
                                let back: FileConfig = toml::from_str(&text).unwrap();
                                let d = back.decode.get(fmt).unwrap();
                                assert_eq!(d.threads, t);
                                assert_eq!(d.zero_copy, zc);
                                assert_eq!(d.streaming, st);
                                assert_eq!(d.buffers, b);
                                assert_eq!(d.fetch, f);
                                assert_eq!(back, cfg);
                                n += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(n, 8 * 2 * 2 * 2 * 4 * 4);
        // 五档全量：单格式（hevc）逐档照读照写（主循环采样之外的 staged/zero_copy 全盖）。
        for f in FeedMode::ALL {
            let mut cfg = FileConfig::default();
            cfg.decode.get_mut(MediaFormat::Hevc).unwrap().fetch = Some(f);
            let text = toml::to_string(&cfg).unwrap();
            assert!(
                text.contains(&format!("fetch = \"{}\"", f.file_str())),
                "{f:?}"
            );
            let back: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(back.decode.get(MediaFormat::Hevc).unwrap().fetch, Some(f));
            assert_eq!(back, cfg);
        }
        // qois/raw 同表：显式全键照读照写（前者帧级并行可兑现，后者零解码占位）。
        for fmt in [MediaFormat::Qois, MediaFormat::Raw] {
            let mut cfg = FileConfig::default();
            {
                let d = cfg.decode.get_mut(fmt).unwrap();
                d.threads = DecodeThreads::All;
                d.zero_copy = true;
                d.streaming = true;
                d.buffers = 16;
                d.fetch = Some(FeedMode::ZeroCopy);
            }
            let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
            let d = back.decode.get(fmt).unwrap();
            assert_eq!(d.threads, DecodeThreads::All);
            assert!(d.zero_copy);
            assert!(d.streaming);
            assert_eq!(d.buffers, 16);
            assert_eq!(d.fetch, Some(FeedMode::ZeroCopy));
            assert_eq!(back, cfg);
        }
        // 老文件缺表：8 格式全默认（bsp_only/关/关/跟随窗口/继承全局供片）。
        let legacy: FileConfig = toml::from_str("version = 1\n[play]\nfps = \"max\"\n").unwrap();
        for fmt in DecodeTable::ORDER {
            let d = legacy.decode.get(fmt).unwrap();
            assert_eq!(d.threads, DecodeThreads::BspOnly);
            assert!(!d.zero_copy);
            assert!(!d.streaming);
            assert_eq!(d.buffers, 0);
            assert_eq!(d.fetch, None);
            // RAW-GEOM：老文件无两键，全格式回 1920x1080（仅 raw 生效）。
            assert_eq!(d.width, 1920);
            assert_eq!(d.height, 1080);
        }
        // 缺键部分表：只写 threads，其余按默认照读（含 fetch 继承）。
        let partial: FileConfig =
            toml::from_str("version = 1\n[decode.hevc]\nthreads = \"all\"\n").unwrap();
        let h = partial.decode.get(MediaFormat::Hevc).unwrap();
        assert_eq!(h.threads, DecodeThreads::All);
        assert!(!h.zero_copy);
        assert!(!h.streaming);
        assert_eq!(h.buffers, 0);
        assert_eq!(h.fetch, None);
        // RAW-GEOM：显式几何照读照写（1280x720 小分辨率 raw 用）。
        let rawgeom: FileConfig =
            toml::from_str("version = 1\n[decode.raw]\nwidth = 1280\nheight = 720\n").unwrap();
        let r = rawgeom.decode.get(MediaFormat::Raw).unwrap();
        assert_eq!(r.width, 1280);
        assert_eq!(r.height, 720);
        let back: FileConfig = toml::from_str(&toml::to_string(&rawgeom).unwrap()).unwrap();
        assert_eq!(back.decode.get(MediaFormat::Raw).unwrap().width, 1280);
        assert_eq!(back.decode.get(MediaFormat::Raw).unwrap().height, 720);
        assert_eq!(back, rawgeom);
        let a = partial.decode.get(MediaFormat::Avc).unwrap();
        assert_eq!(*a, DecodeConfig::default());
        // 部分表显式 fetch：照读，其余表仍继承。
        let explicit: FileConfig =
            toml::from_str("version = 1\n[decode.vp9]\nfetch = \"stream\"\n").unwrap();
        assert_eq!(
            explicit.decode.get(MediaFormat::Vp9).unwrap().fetch,
            Some(FeedMode::Stream)
        );
        assert_eq!(explicit.decode.get(MediaFormat::Hevc).unwrap().fetch, None);
    }

    /// 解码缓冲钳制：超限手写值压到 32，0（跟随窗口）保留。
    #[test]
    fn decode_clamp_matrix() {
        let mut d = DecodeTable::default();
        d.hevc.buffers = 99;
        d.avc.buffers = 0;
        // RAW-GEOM：raw 几何越界钳住（0 → 1，超大 → 4096）。
        d.raw.width = 0;
        d.raw.height = 99999;
        clamp_decode(&mut d);
        assert_eq!(d.hevc.buffers, 32);
        assert_eq!(d.avc.buffers, 0);
        assert_eq!(d.raw.width, 1);
        assert_eq!(d.raw.height, 4096);
        // `coerce_loaded` 同样钳住（读盘路径）。
        let cfg: FileConfig = toml::from_str("[decode.vp9]\nbuffers = 64\n").unwrap();
        assert_eq!(coerce_loaded(cfg).decode.vp9.buffers, 32);
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
            assert_eq!(coerce_loaded(cfg).render.blt_op, expected, "{input}");
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
            window: 0,
        };
        clamp_tune(&mut t);
        assert_eq!(
            t,
            RenderTune {
                frames: 1,
                tile_rows: 8,
                vsync_fps: 240,
                buffers: 16,
                window: 1
            }
        );
        let mut ok = RenderTune {
            frames: 30,
            tile_rows: 64,
            vsync_fps: 60,
            buffers: 2,
            window: 4,
        };
        clamp_tune(&mut ok);
        assert_eq!(
            ok,
            RenderTune {
                frames: 30,
                tile_rows: 64,
                vsync_fps: 60,
                buffers: 2,
                window: 4
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

    /// `[schedule]` 回写：3×3×4×3 = 108 组合 + 钳制/继承/measured（cores 全量不进笛卡尔）。
    #[test]
    fn schedule_roundtrip_matrix() {
        for b in ScheduleBackend::ALL {
            for w in WorkersMode::ALL {
                for d in DistMode::ALL {
                    for r in RolesMode::ALL {
                        let mut cfg = FileConfig::default();
                        cfg.schedule.backend = b;
                        cfg.schedule.workers = w;
                        cfg.schedule.dist = d;
                        cfg.schedule.roles = r;
                        cfg.schedule.cores = 8;
                        cfg.schedule.measured_workers_n = 6;
                        let back: FileConfig =
                            toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
                        assert_eq!(RolesMode::from_u8(r.as_u8()).file_str(), r.file_str());
                        assert_eq!(back, cfg);
                    }
                }
            }
        }
        for (input, expected) in [(0u32, 0u32), (1, 1), (64, 64), (99, 64)] {
            let loaded: FileConfig =
                toml::from_str(&format!("[schedule]\ncores = {input}\n")).unwrap();
            assert_eq!(coerce_loaded(loaded).schedule.cores, expected, "{input}");
        }
        let legacy: FileConfig = toml::from_str("version = 1\n").unwrap();
        assert_eq!(legacy.schedule.cores, 0);
        assert_eq!(legacy.schedule.roles, RolesMode::Auto);
        assert_eq!(legacy.schedule.measured_workers_n, 0);
        let explicit: FileConfig =
            toml::from_str("version = 1\n[schedule]\ncores = 4\nroles = \"split\"\n").unwrap();
        assert_eq!(explicit.schedule.cores, 4);
        assert_eq!(explicit.schedule.roles, RolesMode::Split);
        assert!(toml::from_str::<FileConfig>("[schedule]\nroles = \"bogus\"\n").is_err());
        let mut cfg = FileConfig::default();
        cfg.schedule.measured_workers_n = 5;
        let back: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.schedule.measured_workers_n, 5);
        cfg.schedule.measured_workers_n = 0;
        let cleared: FileConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cleared, cfg);
        // 老文件：`[render]` 旧键残留 → 直接忽略，`[schedule]` 全默认。
        let old: FileConfig = toml::from_str(
            "version = 1\n[render]\nmode = \"direct\"\nworkers = \"all\"\ndist = \"tile\"\n",
        )
        .unwrap();
        let moved = coerce_loaded(old);
        assert_eq!(moved.schedule.workers, WorkersMode::Auto);
        assert_eq!(moved.schedule.dist, DistMode::Auto);
        assert_eq!(moved.render.mode, RenderMode::Direct);
        // 新文件：`[render]` 残留 + `[schedule]` 同在 → 残留同样忽略。
        let new: FileConfig = toml::from_str(
            "version = 1\n[render]\nworkers = \"all\"\n[schedule]\nbackend = \"nano\"\n",
        )
        .unwrap();
        let kept = coerce_loaded(new);
        assert_eq!(kept.schedule.backend, ScheduleBackend::Nano);
        assert_eq!(kept.schedule.workers, WorkersMode::Auto);
    }

    /// 加速全量 31 档回写（`ALL` 逐档 `file_str` 照读照写，不丢字段）。
    #[test]
    fn accel_levels_roundtrip() {
        let mut n = 0;
        for a in AccelLevel::ALL {
            let mut cfg = FileConfig::default();
            cfg.accel.level = a;
            let text = toml::to_string(&cfg).unwrap();
            assert!(text.contains(a.file_str()), "{a:?} missing in {text}");
            let back: FileConfig = toml::from_str(&text).unwrap();
            assert_eq!(back.accel.level, a);
            assert_eq!(back, cfg);
            n += 1;
        }
        assert_eq!(n, 31);
        // 老 `"avx512"` 别名已删：整文件解析失败 → 调用方走全默认 + 警告。
        assert!(
            toml::from_str::<FileConfig>("version = 1\n[accel]\nlevel = \"avx512\"\n").is_err()
        );
        // 缺键老文件按 auto 照读（现行为不变）。
        let missing: FileConfig = toml::from_str("version = 1\n[accel]\n").unwrap();
        assert_eq!(missing.accel.level, AccelLevel::Auto);
        // 回滚兼容：ee2ccad 窗口期写出的 `[accel.decode]` 表直接忽略，不断解析。
        let rolled: FileConfig = toml::from_str(
            "version = 1\n[accel]\nlevel = \"avx2\"\n[accel.decode]\nhevc = \"scalar\"\n",
        )
        .unwrap();
        assert_eq!(rolled.accel.level, AccelLevel::Avx2);
    }
}
