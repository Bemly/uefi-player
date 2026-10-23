//! 读盘/落盘钳制规则（`rules` 触达拆分：`rules.rs` 已超 1000 行红线，
//! 纯规则按域下沉，本文件只收 coerce/clamp 域；行为零改，
//! 对外路径经 `rules` 重导出不变）。
//!
//! P5 新增 `[play] media_dir` 钳制（`clamp_media_dir`）：读盘落盘统一口径
//! （parent 裁决 P5-GUI 收尾）：trim 首尾空格，非法相对路径回 `""` + 警告，
//! 超 64 字符按字符截断 + 警告（返回 `true` 即需警告）；读盘经 `coerce_loaded`，
//! 落盘经 `App::save_to`，文件里永远是干净值。

use crate::schema::*;

/// `[play] media_dir` 字符上限（字符数口径，中文 1 字计 1；与固件缺省 `""`=根目录同义）。
pub(crate) const MEDIA_DIR_MAX_CHARS: usize = 64;

/// 读盘钳制：Blt 三个非上屏操作 GUI 置灰不可选，残留值钳回 B2V
/// （播放行为无变化——固件上屏路本来就只认 B2V；手写值想出引擎数
/// 的改完别点保存，或固件侧照读，见 player.md）。
/// 解码缓冲逐格式钳 0-32 + `[schedule] cores` 钳 0..=64（0 保留，超限压界，不断解析）。
/// `[play] pipeline` / `[render] workers/dist` 已删：老文件残留键 serde 默认忽略，
/// 无需迁移（`[schedule]` 为准）。
/// P5：`[play] media_dir` 同样读盘钳制（警告由 `clamp_media_dir` 返回值展示；
/// 落盘由 `App::save_to` 调同一函数，文件里永远是干净值）。
pub(crate) fn coerce_loaded(mut cfg: FileConfig) -> FileConfig {
    if cfg.render.blt_op != BltOpKind::BufferToVideo {
        cfg.render.blt_op = BltOpKind::BufferToVideo;
    }
    clamp_decode(&mut cfg.decode);
    clamp_schedule(&mut cfg.schedule);
    clamp_media_dir(&mut cfg.play.media_dir);
    cfg
}

/// `[play] media_dir` 钳制（纯规则，可单测）：去首尾空格，
/// 非法相对路径（绝对路径/父目录 `..`/反斜杠/盘符冒号/控制字符）回 `""`，
/// 超 64 字符按字符截断（非字节，避免切断 UTF-8）。
/// 返回是否发生修正（调用方展示警告；读盘落盘调同一函数）。
/// `""`（根目录）恒合法，不警告。
pub(crate) fn clamp_media_dir(s: &mut String) -> bool {
    *s = s.trim().to_owned();
    if s.is_empty() {
        return false;
    }
    // 非法先判（固件按相对 join 读盘，此类值无意义；截断可能恰好切掉非法段，
    // 故先回空，不给截断留歧义）。
    if s.starts_with('/') || s.starts_with('\\') || s.contains('\\') || s.contains(':') {
        s.clear();
        return true;
    }
    if s.split('/').any(|seg| seg == "..") {
        s.clear();
        return true;
    }
    if s.chars().any(|c| c.is_control()) {
        s.clear();
        return true;
    }
    if s.chars().count() > MEDIA_DIR_MAX_CHARS {
        *s = s.chars().take(MEDIA_DIR_MAX_CHARS).collect();
        return true;
    }
    false
}

pub(crate) fn clamp_schedule(s: &mut ScheduleConfig) {
    s.cores = s.cores.min(64);
}

/// 落盘前钳制解码缓冲（滑杆已钳 0-32，这里防手改文件后的脏值回写；
/// 0 保留 = 跟随 `tune.window`，只压上界）。
/// RAW-GEOM：raw 几何钳 1..=4096（手改文件脏值回写；与固件 `store_all` 同口径；
/// 其余格式键固件侧忽略，不钳）。
pub(crate) fn clamp_decode(d: &mut DecodeTable) {
    for fmt in DecodeTable::ORDER {
        if let Some(c) = d.get_mut(fmt) {
            c.buffers = c.buffers.min(32);
            if fmt == MediaFormat::Raw {
                c.width = c.width.clamp(1, 4096);
                c.height = c.height.clamp(1, 4096);
            }
        }
    }
}

/// 落盘前钳制 tune（滑杆已钳，这里防手改文件后的脏值回写）。
/// `window` 是流式供片窗口（1-32，默认 4；只对 staged/stream 生效，
/// 其余供片忽略本项，不断解析）。
pub(crate) fn clamp_tune(t: &mut RenderTune) {
    t.frames = t.frames.max(1);
    t.tile_rows = t.tile_rows.clamp(8, 512);
    t.vsync_fps = t.vsync_fps.clamp(15, 240);
    t.buffers = t.buffers.min(16);
    t.window = t.window.clamp(1, 32);
}
