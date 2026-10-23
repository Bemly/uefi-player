//! 应用状态同步与持久化（同 `impl App`，`app.rs` 触达即拆：跨文件只为守 1000 行上限，行为零改）。
//!
//! - `sync_widgets`：读盘/重置后滑杆镜像同步（否则旧镜像下一帧盖掉新载入值）。
//! - `load_path`/`save_to`：读盘解析 + 落盘钳制（P5 `[play] media_dir` 薄代理：
//!   直读直写 `cfg.play.media_dir`，读盘警告复算 `rules::clamp_media_dir` flag，落盘原样）。

use rust_i18n::t;

use crate::app::App;
use crate::rules::*;
use crate::schema::*;

impl App {
    pub(crate) fn sync_widgets(&mut self) {
        self.accel_f = self.cfg.accel.level.index() as f32;
        self.feed_f = self.cfg.play.feed.index() as f32;
        self.playlist_loop_f = self.cfg.playlist.loop_mode.index() as f32;
        // P5：`[wallpaper] loop` 已删，`wp_loop_f` 同删（画廊循环只剩 `[playlist]`）。
        // 切表不跟读盘走：tab 只切展示，读盘保持当前 tab（见解码页）。
        for (i, fmt) in DecodeTable::ORDER.iter().enumerate() {
            if let Some(d) = self.cfg.decode.get(*fmt) {
                self.dec_threads_f[i] = d.threads.index() as f32;
                self.dec_buffers_f[i] = d.buffers.min(32) as f32;
                self.dec_fetch_f[i] = match d.fetch {
                    None => 0.0,
                    Some(f) => f.index() as f32 + 1.0,
                };
            }
        }
        self.play_f = match parse_fps(&self.cfg.play.fps) {
            FpsPolicy::Auto => 0.0,
            FpsPolicy::Max => 241.0,
            FpsPolicy::Capped(n) => n as f32,
        };
        self.wp_fps_f = match parse_fps(&self.cfg.wallpaper.fps) {
            FpsPolicy::Auto => 0.0,
            FpsPolicy::Max => 241.0,
            FpsPolicy::Capped(n) => n.clamp(1, 240) as f32,
        };
        self.wp_preload_f = self.cfg.wallpaper.preload.min(32) as f32;
        self.tune_frames_f = self.cfg.render.tune.frames.clamp(1, 120) as f32;
        self.tune_tile_f = self.cfg.render.tune.tile_rows.clamp(8, 512) as f32;
        self.tune_vsync_fps_f = self.cfg.render.tune.vsync_fps.clamp(15, 240) as f32;
        self.tune_buffers_f = self.cfg.render.tune.buffers.min(16) as f32;
        self.sched_cores_f = self.cfg.schedule.cores.min(64) as f32;
        self.bar_r_f = self.cfg.display.bar.r as f32;
        self.bar_g_f = self.cfg.display.bar.g as f32;
        self.bar_b_f = self.cfg.display.bar.b as f32;
        // 越界单选回到列表内（删行后旧下标悬空，下一帧点行即覆盖）。
        // P5：`[[wallpaper.items]]` 已删，`wallpaper_sel` 同删；只剩画廊单选。
        if let Some(s) = self.playlist_sel {
            if s >= self.cfg.playlist.items.len() {
                self.playlist_sel = None;
            }
        }
        // `media_dir` 不在此同步：直读直写 `cfg.play.media_dir`（读盘由 `coerce_loaded`
        // 装填，重置由 `cfg` 回默认覆盖）；滑杆镜像才需要随读盘回拨。
    }

    pub(crate) fn load_path(&mut self, path: String) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(cfg) => {
                    // 未知键 serde 默认忽略：老文件残留（`[play] pipeline` /
                    // `[render] workers/dist` / 别名值）不进解析，整文件失败才报错。
                    // P5 `media_dir` 读盘警告：`coerce_loaded` 内 trim/截断不返回值，
                    // 故在原始值上复算一次 `clamp_media_dir` 取 flag（同函数，确定性一致；
                    // trim 合法不警告，截断才警告，不静默吃掉）。
                    let mut md_probe = cfg.play.media_dir.clone();
                    let md_fixed = clamp_media_dir(&mut md_probe);
                    let cfg = coerce_loaded(cfg);
                    let msg = t!(
                        "status.loaded",
                        path = path.clone(),
                        accel = cfg.accel.level.file_str(),
                        render = cfg.render.mode.file_str()
                    )
                    .into_owned();
                    self.cfg = cfg;
                    self.sync_widgets();
                    self.tune_at_load = self.cfg.render.tune;
                    self.status = msg;
                    if md_fixed {
                        self.status = format!("{} {}", self.status, t!("play.media_dir_warn"));
                    }
                    self.path = path;
                    // 换文件即换图库：默认背景跟随新目录首个 avc/hevc。
                    let p = self.path.clone();
                    self.gallery.refresh(&p);
                }
                Err(e) => {
                    self.status =
                        t!("status.parse_error", path = path, err = e.to_string()).into_owned();
                }
            },
            Err(e) => {
                self.status =
                    t!("status.read_error", path = path, err = e.to_string()).into_owned();
            }
        }
    }

    pub(crate) fn save_to(&mut self, path: &str) {
        clamp_tune(&mut self.cfg.render.tune);
        clamp_decode(&mut self.cfg.decode);
        clamp_schedule(&mut self.cfg.schedule);
        // P5 统一口径（parent 裁决）：读盘落盘调同一 `clamp_media_dir`，
        // 文件里永远是干净值；修正即警告（`play.media_dir_warn`），不静默吃掉。
        // 文本框无 live 钳制（打字中截断会抢光标），存盘时统一钳。
        let md_fixed = clamp_media_dir(&mut self.cfg.play.media_dir);
        // tune 变了 = bench 条件变了：旧记忆作废，下次启动重测。
        // 不做这个，改 tune 会被“有记忆快启”吞掉，永远不生效。
        if self.cfg.render.tune != self.tune_at_load {
            self.cfg.render.measured_render = RenderMode::Auto;
            self.cfg.render.measured_workers = WorkersMode::Auto;
            self.cfg.render.measured_dist = DistMode::Auto;
            self.cfg.render.measured_vsync = VsyncMode::Auto;
            self.cfg.render.measured_vsync_fps = 0;
            self.cfg.render.measured_cores = 0;
            self.cfg.schedule.measured_workers_n = 0;
            self.cfg.play.measured_max_fps = 0;
        }
        self.tune_at_load = self.cfg.render.tune;
        match toml::to_string(&self.cfg) {
            // `media_dir` 已钳制，随 schema 序列化直写（薄代理，无字符串手术）。
            Ok(text) => match std::fs::write(path, text) {
                Ok(()) => {
                    self.status = t!("status.saved", path = path).into_owned();
                    if md_fixed {
                        self.status = format!("{} {}", self.status, t!("play.media_dir_warn"));
                    }
                }
                Err(e) => {
                    self.status =
                        t!("status.write_error", path = path, err = e.to_string()).into_owned();
                    if md_fixed {
                        self.status = format!("{} {}", self.status, t!("play.media_dir_warn"));
                    }
                }
            },
            Err(e) => {
                self.status = t!("status.serialize_error", err = e.to_string()).into_owned();
            }
        }
    }
}

/// P5 `[play] media_dir` 薄代理说明：直读直写 `cfg.play.media_dir`
/// （schema 落键 `PlayConfig.media_dir` + `rules::clamp_media_dir` + locale 三键；
/// 本文件旧的 App 侧缓冲 + 原始 TOML 字符串手术已删除）。
/// 统一口径（parent 裁决，黑盒矩阵覆盖）：trim + 非法回 `""` + 超 64 字符截断，
/// 修正即警告（`load_path` 复算 flag、`save_to` 同函数复算，文件里永远是干净值）。
/// 零新依赖（`Cargo.toml` 不动）。

#[cfg(test)]
mod media_dir_tests {
    use super::*;

    /// 钳制口径（`load_path` probe / `save_to` 同一函数）：trim 不警告，
    /// 非法回空警告，截断警告。
    #[test]
    fn proxy_clamp_unified() {
        let mut v = String::from("  1080p  ");
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, "1080p");
        let ok64: String = std::iter::repeat_n('a', 64).collect();
        let mut v = ok64.clone();
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, ok64);
        let bad65: String = std::iter::repeat_n('a', 65).collect();
        let mut v = bad65.clone();
        assert!(clamp_media_dir(&mut v));
        assert_eq!(v, ok64);
        // 中文按字符数截断（非字节口径，不切断 UTF-8）。
        let mut v = std::iter::repeat_n('片', 65).collect::<String>();
        assert!(clamp_media_dir(&mut v));
        assert_eq!(v, std::iter::repeat_n('片', 64).collect::<String>());
        // 非法相对路径回空 + 警告（绝对/父目录/反斜杠/冒号/控制字符）。
        for s in [
            "/abs", "\\abs", "C:\\x", "d:x", "..", "../up", "a/../b", "a\\b", "a\nb",
        ] {
            let mut v = String::from(s);
            assert!(clamp_media_dir(&mut v), "{s:?}");
            assert_eq!(v, "", "{s:?}");
        }
        // `""`（根目录）恒合法，不警告。
        let mut v = String::new();
        assert!(!clamp_media_dir(&mut v));
        assert_eq!(v, "");
    }

    /// 存盘同样钳制（`save_to` 同一函数）：超长/非法在序列化前已干净，文件无脏值。
    #[test]
    fn proxy_save_clamps_same_fn() {
        let long: String = std::iter::repeat_n('a', 65).collect();
        let mut cfg = FileConfig::default();
        cfg.play.media_dir = long.clone();
        assert!(clamp_media_dir(&mut cfg.play.media_dir));
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains(&long), "{text}");
        assert!(
            text.contains(&format!("media_dir = \"{}\"", &long[..64])),
            "{text}"
        );
        let mut cfg = FileConfig::default();
        cfg.play.media_dir = String::from("/abs");
        assert!(clamp_media_dir(&mut cfg.play.media_dir));
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("media_dir = \"\""), "{text}");
    }

    /// 缺键默认 `""`（根目录），无警告。
    #[test]
    fn proxy_missing_key_defaults() {
        let cfg: FileConfig = toml::from_str("version = 1\n[play]\nfps = \"auto\"\n").unwrap();
        assert_eq!(cfg.play.media_dir, "");
        let mut v = cfg.play.media_dir.clone();
        assert!(!clamp_media_dir(&mut v));
    }
}
