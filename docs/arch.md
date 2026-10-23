# uefi-player 架构

> 口径：`src/` 固件侧（`no_std`）+ `tools/config-gui` host 工具（`std`，零依赖关系）。
> 构建见根 `AGENTS.md`「双构建」。

## 模块地图

```text
src/main.rs      入口：关看门狗 → 采固件 OSXSAVE → config::load → seed_runtime + resolve_accel
                 → Screen::new（首选 1080p）→ ensure_render_calibrated → idle::run（永不返回）
src/idle.rs      顶层交互：壁纸待机（全屏循环，任意键弹窗；p 直进 TUI）+ 启动选项弹窗
                 （条目直选交权 / p 进 TUI / f 固件设置 / s 关机 / r 重启 / q 回待机）
src/ptui/        TUI 外壳（中文页栈，p 进 q 逐层回）：mod（run 值进值出 Screen）+
                 state（Next 导航/文件串小写表/调档 helpers）+ media（media_dir 单层扫描 +
                 tagged 探针只读识别）+ pages/（home 九宫 / play 扫描播片+调档 /
                 accel 31 档 / render 路径+微调 / schedule 后端+核 / decode 八格式 /
                 display 适配+黑边 / tests 透传诊断页 / wallpaper 四键+显式落盘）
src/menu.rs      TUI 测试页（诊断透传；播片入口已搬 play 页经 play_path；行区壁纸背景，q 回 TUI 上级）
src/font/        TUI 点阵字体：latin（英文 8x16）+ cjk（中文 16x16 子集 31 字 + 豆腐块）+
                 layout（度量 8/16/行高 18/换行，WP_TOP 布局依赖）
src/graphics.rs  Screen（GOP 独占封装）：draw_str/upload_frame(+region)/bench 矩阵/asm 拷贝族
                 + present（display.fit 五档 + align 五档 + bar 留边几何，全由 present::geometry 定）
src/video.rs     播放器：video_run → blt_loop（单线程 Blt）/ direct_loop（BSP 直写）
                 / mp_draw（全核并行）+ pace_or_drop/确定性掉帧 + OSD
src/video/exec.rs  执行器后端（`[schedule] backend`）：nano_pump（单核泵）/
                 dynamic_draw（分带 spawn + steal，BSP 独判）+ 回退门
src/video/{decoder,buffer}.rs  VideoMemoryRaw 预解码（校验 + 计数 + 空集报错）
src/boot/        P1：entries（BLS 枚举）+ chainload（eficore）+ mod（枚举 + 交权；UI 在 idle 弹窗）
src/wallpaper.rs P4：画廊拼接（[playlist] 逐项检测 + preload 按项封顶）+ 行区/全屏上屏
src/config.rs    player.toml schema + 运行时快照（PlayerRuntime 脱钩内存）+ 壁纸/画廊文件侧 +
                 measured 记忆回写
src/fs.rs        卷 IO：open/read/write（删后建防残留）+ open_dir/list_dir
src/error.rs     NyaStatus + handle_fatal（仅生产路径）
src/diag/        真机诊断项（非 cargo test）：registry 注册 + common（emit!/按键/报告/XSAVE）
                 + async_exec（uefi-async 自举：nano/dynamic 两线 smoke）
                 + input2_probe（uefi-input2 自举：Ex 键 + shift 状态）
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

## 交互路由

- 待机（`idle::run`）：壁纸全屏循环；按键路由按 `p` 还是其余键分流——
  `p` 进 TUI 外壳（`ptui::run`，`q` 逐层回到此）；其余任意键走原引导弹窗。
- 弹窗内同样保留 `p` 入口（与待机直达对称）；`q` 回壁纸待机，
  条目数字键直选交权（回来继续待机），`f/s/r` 固定项不变。
- TUI 内：首页九宫数字进子页，子页 `q` 回首页，首页 `q` 回调用方（待机/弹窗页栈继续）；
  播片中按任意键停播回播放页（`play_path` 内按键退出，见 `docs/player.md`）。
- 菜单（`menu::run_test_page`）只剩诊断透传：播片入口已搬 TUI 播放页，
  `q` 回 TUI 上级（不再直回壁纸待机）。

## 数据流

- 播放列表/画廊：`[playlist] items` → 逐项自动检测 → `Clip`
  （播片 `video_run` 首个可播项起播；壁纸 `wallpaper::load` 全拼一个 Clip）。
- TUI 播放页：`media_dir`（`""`=根，否则单层子目录）→ `media::scan` 枚举直属项 +
  tagged 探针只读识别 → 按字节序排 → 数字键选进 `play_path`（与列表同管线）。
- 运行时脱钩：开机拷入 `PlayerRuntime` 后只读内存（TUI 经 `runtime_mut` 改内存；
  落盘只有壁纸页 `s`，只写 `[wallpaper]` 表；`measured_*` 留文件由固件回写）。
- 壁纸：画廊 Clip（`preload` 按项封顶）→ `upload_frame`/`upload_frame_region`
 （源几何取 Clip 真实 `w/h`，`display.fit` 生效）→ A 行区循环 / B 全屏过渡。
- 文本：统一 `Screen::draw_str`（`src/font/` 点阵，英文 8x16 + 中文 16x16 子集）；
  串口保留 `log::*`（`emit!` 双写）。
