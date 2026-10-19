# uefi-player 架构

> 口径：`src/` 固件侧（`no_std`）+ `tools/config-gui` host 工具（`std`，零依赖关系）。
> 构建见根 `AGENTS.md`「双构建」。

## 模块地图

```text
src/main.rs      入口：关看门狗 → 采固件 OSXSAVE → config::load → resolve_accel
                 → Screen::new（首选 1080p）→ ensure_render_calibrated → idle::run（永不返回）
src/idle.rs      顶层交互：壁纸待机（全屏循环，任意键停）+ 启动选项弹窗
                 （条目直选交权 / p 纯播放器 / f 固件设置 / s 关机 / r 重启 / q 回待机）
src/menu.rs      纯播放器菜单（诊断 + 播片；`q` 回壁纸待机）
src/graphics.rs  Screen（GOP 独占封装）：draw_str/upload_frame(+region)/bench 矩阵/asm 拷贝族
src/video.rs     播放器：video_run → blt_loop（单线程 Blt）/ direct_loop（BSP 直写）
                 / mp_draw（全核并行）+ pace_or_drop/确定性掉帧 + OSD
src/video/{decoder,buffer}.rs  VideoMemoryRaw 预解码（校验 + 计数 + 空集报错）
src/boot/        P1：entries（BLS 枚举）+ chainload（eficore）+ mod（枚举 + 交权；UI 在 idle 弹窗）
src/wallpaper.rs P4：序列位图预载 + 行区/全屏上屏
src/config.rs    player.toml schema + 原子生效值 + measured 记忆回写
src/fs.rs        卷 IO：open/read/write（删后建防残留）+ open_dir/list_dir
src/error.rs     NyaStatus + handle_fatal（仅生产路径）
src/diag/        真机诊断项（非 cargo test）：registry 注册 + common（emit!/按键/报告/XSAVE）
                 + async_exec（uefi-async 自举：nano/dynamic 两线 smoke）
```

## GOP 独占模型

- GOP 同一时间只能被一个持有者以 Exclusive 打开。
- `registry::Entry::Shared`：借用菜单的 `Screen`（默认形态）。
- `registry::Entry::Exclusive`：菜单先 `drop(Screen)`，跑完 `Screen::reacquire()` 重建。
  P1 chainload 必须 Exclusive：GOP 独占不能带过 handoff，否则下个镜像打不开 GOP。
- 菜单结束后不清屏（demo 画面保留，提示打底部，见 `cursor_to_bottom`）。

## 看门狗 / MP 模型

- 入口立即 `set_watchdog_timer(0, ...)` 禁固件 5 分钟看门狗（长 bench/播放会触发复位）。
- MP：BSP 驱动，AP 常驻循环 + 轮次号/原子计数器同步（`gop_bench` fill、`mp_draw` play、
  `graphics` 校准、`boot` 无（单线程））。单发 `startup_all_aps` 只测唤醒开销，
  稳态吞吐必须常驻 + 热身后再计时。
- 播放掉帧确定性：逐核按自家时钟判迟会发散，BSP 判、全核执行同一发布值（`video.rs`）。

## 数据流

- 视频：`1080p/video.qois`（`u32le len + qoi` 串接）→ 全量预解码（RGBA→BGRA）
  → `upload_frame`（render 矩阵选 Blt/直写）/ `mp_draw`（分核直写）。
- 壁纸：`wallpaper/frameNNNN.raw`（BGRA 直存）→ 预载（`preload` 上限）
  → A 行区循环 / B 全屏过渡。
- 文本：统一 `Screen::draw_str`；串口保留 `log::*`（`emit!` 双写）。
