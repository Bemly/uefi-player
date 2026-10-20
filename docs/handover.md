# 交接：uefi-player 现状、经验与后续计划

> 给新 Agent 看的。先读 `AGENTS.md`（约束）+ 本文件（上下文），再动手。
> 口径文档是 `docs/`（唯一），测试必须 100% Rust（见 AGENTS）。

## 1. 项目一张图

x86_64-unknown-uefi 单目标 `no_std` 应用：基于 sprout 扩展的引导器 + SIMD
加速矩阵 + UEFI 视频播放器 + 引导动态壁纸 + host 跨平台配置 GUI。

```
src/main.rs        入口：关看门狗 → 采 XSAVE → config::load → resolve_accel
                   → Screen::new(1080p 首选）→ ensure_render_calibrated → run_menu（永不返回）
src/menu.rs        开机菜单（按键分发；P4-A idle 壁纸；Exclusive 子菜单用 drop/重建 GOP）
src/graphics.rs    Screen（GOP 独占封装）：upload_frame/region（影子缓冲分支）+
                   render 校准矩阵（direct-bsp/mp-static/tile/dynamic/blt/op 数）
src/video.rs       播放器：blt_loop / direct_loop / mp_draw（static/tile/dynamic +
                   影子整推：Blt 整推或 CPU 整推）+ pace_or_drop + OSD + half 取半
src/video/{decoder,buffer}.rs  VideoMemoryRaw 预解码（u32le len + QOI 串接）
src/boot/          entries.rs（BLS 解析/排序/boot counting）+ detect.rs（多卷自动发现）
                   + chainload.rs（PE/Linux 直启，跨盘）+ mod.rs（Exclusive 子菜单）
src/config.rs      player.toml schema + 原子生效值 + measured 记忆回写
src/wallpaper.rs   序列位图壁纸（frameNNNN.raw BGRA 直存，A 菜单背景 / B 引导过渡）
src/fs.rs          卷 IO（Fs::new 本卷 / Fs::open(Option<Handle>) 任意卷）+ delete
src/error.rs       NyaStatus + handle_fatal（只生产路径走致命）
src/diag/*         真机诊断项（registry 注册；emit! 双写屏显+串口）
tools/config-gui   host 独立 crate（eframe 0.35 + egui_glass 液态玻璃），与固件零依赖
tools/qemu-matrix  host 独立 crate（纯 Rust QEMU 黑盒矩阵，matrix.toml 定义用例）
third_party/sprout 只读 submodule，只依赖 5 个 lib
                    （edera-sprout-config/eficore/bls/parsing/boot；`boot` 上游纯 bin，本仓用 `patches/` 叠 `[lib]` 后直调）
```

交互铁律（坏了会出大事的）：

- GOP 独占：`Shared` 借 `Screen`；`Exclusive` 先 `drop` 再跑完 `Screen::reacquire()`。
  AP 永不碰 GOP（Blt 调用只许 BSP 发；固件 Blt 非线程安全）。
- AP↔BSP 同步只用原子世代（`drop_gen`/`push_gen`/`bgen` + `sync_counter` 栅栏）；
  掉帧判定只有 BSP 说了算（逐核按自家时钟判会发散）。
- 固件输出统一走 `Screen::draw_str`；串口要看必须同步打 `log::`（屏显-only
  的界面 QEMU 黑盒测不到，见下）。

## 2. 本轮已完成（按时间顺序，都是已提交状态）

1. **多核分工 dynamic 档**：按各核实测耗时加权分行（`graphics::dynamic_row_bounds`，
   最大余数 + 每核至少 1 行），校准 probe 定权计时三选一，播放逐轮 EWMA 重算。
2. **vsync half 档 + `on` 改名 `static`**：`static` 按预算滑杆硬封顶；
   `half` 复用冠军 cell 计时算 max/2，落盘 `measured_vsync_fps` 复用。
   旧文件 `"on"` 用 serde alias 照读，不炸旧配置。
   所有 60fps 硬编码描述已清（预算一律走 `effective_vsync_fps()`）。
3. **后缓冲 `tune.buffers`（0-16，默认 0）**：直写路先画影子再整推（撕裂窗口压成
   一次 push）；mp 路 BSP 整推（Blt 引擎，失败回退 CPU 整推）；
   `push_gen` 世代同步（N==1 也无竞态）；OSD 先画进影子再随推上屏。
   Blt 单发路不用（已是单 push）。
4. **blt_op 进矩阵**：`upload_blt` 原来写死 B2V（`config::blt_op()` 零调用，死配置）。
   现在矩阵单发所选 op 出引擎数；只有 B2V 能上屏参选（GUI 置灰其余三项，不删；
   读盘钳回 B2V）。混合整推固定 B2V。
5. **GUI 整顿**：标题栏中英切换（`UEFI Player`/`UEFI播放器`，`send_viewport_cmd`
   跟语言走）；中央摘要纸只留一句提示，摘要全收进左侧导航；无关控件整组隐藏
   （Blt 藏 workers/dist/tune-tile/off 藏 vsync 预算/关壁纸藏三滑杆）；
   `locales/` 改动不触发重编的坑已用 `tools/config-gui/build.rs`
   （`rerun-if-changed`）根治。
6. **双黑盒矩阵（AGENTS 硬性要求，改功能必同步更新）**：
   - GUI 侧 `tools/config-gui/src/main.rs::blackbox_matrix`：全档位笛卡尔回写、
     旧文件兼容、读盘/落盘钳制、`bench_pending`/`tune_visible` 表格断言。
     纯规则已抽成无 display 依赖的自由函数（`coerce_loaded/clamp_tune/
     bench_pending/tune_visible`），UI 只是薄调用——新规则先加表再接 UI。
   - QEMU 侧 `tools/qemu-matrix/`（纯 Rust runner + `matrix.toml`，33 case 全绿）：
     每个 case 独立 FAT 镜像 + 独立 QEMU（pflash 挂 OVMF code，`-bios` 拒载
     3.5MB 非对齐文件），串口断言 + `sendkey` 按键（`wait_for` 事件驱动，
     `push_gen` 同款思想），`needs_video/wallpaper/extra_volumes/artifact/soak`齐全。
7. **sprout 全功能对齐（用户拍板，见 AGENTS「sprout 全功能对齐」）**：
   - `src/boot/detect.rs`：多卷自动发现（逐卷 BLS/UKI/Linux 内核对/Windows/Shell/OpenCore）。
   - Linux 直启（kernel + initrd 拼接 + options + `LINUX_EFI_INITRD_MEDIA_GUID`
     交接 + devicetree；stubble 占位 `placeholder` 照抄上游）。
   - 跨盘 chainload（目标卷设备句柄拼路径 + 跨卷直读）。
   - BLS 全字段透传（options/多 initrd/devicetree/arch 过滤/uki 键/版本排序）。
    - boot counting 消费（启动尝试即递减，直调上游 `BootCounterTarget::consume` 真改名）。
   - UKI 多 profile 展开（`.osrel/.cmdline/.uname/.profile`，一 profile 一条目）。
   - strict 模式（`[boot] strict`：BLS 只认本卷 + XBOOTLDR）。
    - `[boot]` 配置（auto/windows/linux/shell/uki/macos/strict）+ GUI 引导页 + 矩阵覆盖。
   - QEMU 验证：Windows/UKI/多 profile 跨盘 chainload 全通，`locate ok` → 1338880 字节
     → `starting image` → 第二遍 cfg + 菜单（计数断言 `x2`，不是误判）。

## 3. 踩坑历史（血泪，新人先读这节）

1. **OVMF 调试版 `DevicePathFromText` 触发 `MemoryAllocationLib.c:821` ASSERT**
   （`CpuDeadLoop`，串口停在 `boot: resolving`）。结论：全程只用二进制
   DevicePath 操作，零文本解析（见 `chainload.rs` 文件头）。
2. **BLS 路径是 `/` 分隔，UEFI 要 `\`**：不转直接 `INVALID_PARAMETER`。
   当时串口停在 `boot: reading` 之后就哑了——因为报错只画了屏。
   教训：**所有屏显-only 的失败路径必须同步打串口**（`boot failed` 已补
   `log::warn!`），否则无头 QEMU 抓瞎只能靠截屏。
3. **QEMU 按键被 `flush_keys` 吃掉**：菜单每次重画都排空陈旧输入，测试键
   发早了就没了。对策：发键一律等串口标记（`menu: ready` / `menu: back from` /
   `boot: N entries` / `boot: [k] title <kind>`），`wait_for` 卡点，不靠裸睡；
   Exclusive 子菜单返回后还有一次 `pause_return`，键序列要多补一个 dismiss 键。
4. **QEMU 跑陈旧 EFI**：runner 默认跳过构建导致全员回退默认配置，查了半天。
   现在 runner 默认必编（`--no-build` 才复用）。
5. **`-bios` 拒载 brew 的 x64 OVMF**（3.5MB 非对齐）：改 pflash 只读挂 code。
   另：本仓没有 x86_64-vars.fd，用 stateless code 跑（已验证进 shell）。
6. **FAT 镜像做法**：`newfs_msdos` 只认设备节点，走
   `mkfile → hdiutil attach -nomount → newfs_msdos → 拷文件 → detach`。
   Linux 走 `mtools`（分支已写，没在本机验证过）。
7. **PE 段名必须恰 8 字节**：测试资产生成脚本曾把 9 字节的 `.osrel\0\0`
   塞进 8 字节槽，后面全错位一位。教训：合成固件 fixture 后先用独立解析验一遍。
8. **UKI 期望写反**：上游 version-sort 是新版本在前（`third-kernel` >
   `second-kernel`），矩阵按上游语义是 `[0] testos (Second)`。矩阵抓到了，
   是我手写的期望错了——矩阵对了，不要"修"矩阵去迎合直觉。
9. **`else unsafe` 不是合法 Rust**（`else` 后只能跟块或 `if`），包一层 `{}`。
10. **串口是串口，屏显是屏显**：`draw_str` 串口看不见；所有黑盒要断言的界面
    切换，必须补 `log::` 标记（README 里有规范）。
11. **`sendkey` 的 telnet 实现必须按字节读**：banner 含非 UTF-8，直接 `.read()`
    解码会炸；`screendump` 排查时用 `sips` 转 png 看（QEMU 黑盒跑时 `-display none`）。

## 4. 已知未做 / 有意不做（别重复造轮子查一遍）

- **Linux 真内核启动**：只验证到"列出条目"（假内核文件），没真起过 Linux。
  要验需要真内核 + initrd + 根文件系统，QEMU 里另起一套盘。
- **devicetree 安装路径**：代码已通（Secure Boot 开时跳过，上游同款），x86 无消费者，
  没实测过。
- **boot counting 递减**：逻辑已实现（启动尝试即真改名），`boot-counting`
  矩阵 case 已覆盖单次递减断言；"计数耗尽自动跳过"的端到端（两次启动）还没做，
  待排。
- **并行多核 tile 路读源问题（疑似）**：`video.rs` tile 分支用本核切片按全局行号
  取数，疑似错位/OOB——bench 路（共用源）是对的所以 HWBENCH 数字不受影响；
  没动手，等用户拍板再修（动了会改变现有 tile 行为）。
- **macOS 原生引导**：用户明确不要（bless/APFS 超出 UEFI 范围；sprout 自己也没有）。
  只做 OpenCore 链式接力（已实现：`[boot] macos` + detect + `boot-macos` case）。
- **sprout 的菜单/phase/extractor/driver/SBAT/交互配置语言**：有意不搬
  （AGENTS 已定：UI/配置保持本仓，sprout 只当实现参考）。
- **OpenCore 链式接力**：已实现（`[boot] macos` 开关 + detect + `boot-macos`
  矩阵 case，Windows 同款形状；不扫通用 `BOOTX64.EFI`）。

## 5. 接下来怎么做的计划（按顺序）

1. **补 boot counting 耗尽跳过端到端 case**：第二卷放 `test+2-0.conf` + efi，起两次，
   断言文件名变成 `test+1-0.conf`（重挂载读目录）。预计半天。
3. **tile 读源疑似错位**：先写个最小复现（QEMU 截屏对比 static/tile 同一帧，
   或串口 dump 像素校验和），确认后再动手修。预计 1 天（含验证）。
4. **真 Linux 启动（可选，另立项）**：需要真内核 + initrd + rootfs 三件套，
   QEMU 另起数据盘；i386/x86_64 内核都行。这是大项，先出方案再动手。
5. 日常：改功能先加两套矩阵的表/case（GUI `blackbox_matrix` + QEMU `matrix.toml`），
   按 README 规范走；提交前 `cargo check/clippy/fmt` 双端 + 两套矩阵全绿
   （AGENTS 有完整门禁命令，`docs/roadmap.md`「验证门禁」）。

## 6. 常用命令（都在仓库根，host target 显式指定是因为根 `.cargo/config` 钉死了 UEFI）

```bash
cargo build --target x86_64-unknown-uefi
cargo clippy --target x86_64-unknown-uefi
cargo fmt --all --check
T="$(rustc -vV | sed -n 's/^host: //p')"
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$T"
cargo run  --manifest-path tools/qemu-matrix/Cargo.toml --target "$T" -- [--case X] [--include-slow]
cargo run  --manifest-path tools/config-gui/Cargo.toml  --target "$T"   # GUI 本体
```

QEMU 手动看屏（矩阵默认无头）：去 `tools/qemu-matrix/runs/<case>/usb.img`，
把 `-- -display none` 换掉直接起，键盘直敲；或 monitor 里 `screendump a.ppm`。
