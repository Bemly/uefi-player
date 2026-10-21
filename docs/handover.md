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
   - QEMU 侧 `tools/qemu-matrix/`（纯 Rust runner + `matrix.toml`，42 case（video-dynamic 宿主吃紧时例外，见 §4））：
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
- **video-dynamic 对宿主内存敏感**：QEMU 矩阵偶发 guest 内 OOM（`memory allocation of 56 bytes failed`，
  紧随 `play: path=dynamic_draw`）或 hang 无 `alive round`。宿主内存吃紧（同时跑别的项目）时复现，
  与固件改动无关（干净树同 FAIL）。红了先重跑该 case，宿主空闲重跑即过再定性。
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
cargo fmt -p uefi-player --check   # 不是 --all：path 依赖的 vendor 树豁免（见 §7）
T="$(rustc -vV | sed -n 's/^host: //p')"
cargo test --manifest-path tools/config-gui/Cargo.toml --target "$T"
cargo run  --manifest-path tools/qemu-matrix/Cargo.toml --target "$T" -- [--case X] [--include-slow]
cargo run  --manifest-path tools/config-gui/Cargo.toml  --target "$T"   # GUI 本体
```

QEMU 手动看屏（矩阵默认无头）：去 `tools/qemu-matrix/runs/<case>/usb.img`，
把 `-- -display none` 换掉直接起，键盘直敲；或 monitor 里 `screendump a.ppm`。

## 7. HEVC 适配心得（2026-10-21，rusty_h265 vendor + no_std 标量移植）

背景：播放器/壁纸要加 mp4 格式，先验 H.265 轮子（`rusty_h265`），
结论可用后 vendor 进 `third_party/` 并自 port 标量 no_std，
最后落诊断项 `hevc_probe`（键 `d`）+ QEMU `hevc-probe` case。
下次再接同类纯 Rust 编解码轮子，直接照这套流程走。

### 7.1 选型（一句话：h265 就是年轻版的 h264，早 4 个月）

- 采用 `rusty_h265 0.6.2`：同门作者，Apache-2.0，rust 1.85（< 本仓 1.92），
  无 C/FFI，JCT-VC 一致性套件门控。缺点：下载量 1.3k、上游自己还没 no_std。
- `less-avc`（树里已有的）是**编码器**，不能解码，别被 `Cargo.toml` 注释里的
  “视频解码类”骗了（它名下只有这一项时更要核实）。
- `openh264` 系是 C 绑定：UEFI 无 C 工具链 + 非 no_std 友好，出局。
- h264 兄弟（`rusty_h264-decoder`）已有 `std/global-alloc/libm` 分层，
  h265 还没长出来——结构更干净（纯整数，连 `libm` 都不要；也没有全局
  分配器坑），port 反而比 h264 省事。

### 7.2 移植方法（审计 → 分类 → 编译器驱动，三板斧）

1. `grep -rn "std::" src/` 列全量（本轮 h265 共 67 处），先剔 test/bin/
   `#[cfg(test)]`/`#[cfg(feature)]` 门控内的——真生产依赖只剩约 60 处。
2. 分类只有 7 种，无一例外都有机械解（详见 `VENDOR.md`）：
   env 开关 → 抄 h264-common 的 knob 主义（无 std 恒读作未设置）；
   `eprintln!` → 同文件 shim 成 no-op；`Arc/VecDeque/原子量` →
   `alloc`/`core` 同类型路径替换；`Display/Error` → `core::{fmt,error}`；
   `Vec/String/vec!/format!` → 按文件补 `alloc` 前奏；
   `OnceLock` 缓存 → std 侧原样保留、no_std 侧折成常量（perf 与行为双保）。
3. 剩下的交给编译器：`--no-default-features` host 先过，
   再过 `--target x86_64-unknown-uefi`，报错全是缺 import，一轮补齐。
   纪律：`VENDOR(uefi-player):` 逐处标改动点，合上游时按注释重放；
   只改必要行，不顺手“美化”上游代码。

### 7.3 三个坑（都是实锤，不是推测）

1. **`cargo check` 能过 ≠ 能用**：本机新工具链自带 UEFI 的 `libstd`，
   纯 `std` 代码 check 照过。真门槛在线下三处——链接（lang 件冲突）、
   运行（std 运行时在 UEFI 上不可用）、仓规（固件禁 `std` 依赖）。
   以后验轮子 no_std，先看源码有没有 `#![no_std]` + 特性门，再谈编译。
2. **`cargo fmt --all` 会扫 path 依赖**：vendor 树（上游风格，300+ 处
   fmt 差异）直接炸门禁。解法不是格式化 vendor 树（毁掉合流能力），
   而是门禁收窄为 `cargo fmt -p uefi-player --check`
   （AGENTS/roadmap/本文件 §6 已同步改；`rustfmt.toml` 的 `ignore`
   是 nightly-only，stable 上用了只会告警，无用，别加）。
3. **校验和口径要在设计时定死**：首版 `hevc_probe` 期望值把 host 拼帧
   输出里的色度也加了进去，固件侧只加 Y——数字对不上，第一反应别怀疑
   轮子，先对口径（本轮：`aa91d…` 整文件 md5 对上、Y 和一致才是真一致）。

另：`accel` 箱的 `OnceLock` planner 在标量构建下全是死代码
（调用点被 `simd` 门控），shim 写成什么都不影响行为——放心写最简单的。
`video-dynamic` 的 QEMU 红与本次无关（宿主压力 flake，见 §4），
重跑策略不变：宿主空闲单跑该 case。

### 7.4 验证链（下次照单全收）

host 像素级回归（ffmpeg md5 + 标量/SIMD 双路一致）→
4 配置 check（默认/无默认/无默认+std/UEFI）→ 箱内单测
（decoder 39 + accel 30 通过）→ QEMU 矩阵（`hevc-probe` 首跑即绿）。
`simd`+无 `std` 组合明确不支持（ twins 没 port，唯一非机械点是
运行时 ISA 检测对接固件 CPUID 门禁），留待上游或另立项。

### 7.5 解码 SIMD 开关回滚 + 加速全量 29 档（2026-10-21，用户拍板）

背景：ee2ccad 加了 `[accel.decode]` 三键新区（hevc/vp9/h263 各一键，
`auto`/`scalar`/`max`）；vp9 的 `set_simd_allowed`、h263 yuv/deblock 的
`set_simd_enabled` 在固件侧此前零调用（vp9 缺省开、h263 缺省关），本轮才接上线。

- 回滚（用户拍板①）：ee2ccad 的三键新区全摘——固件 `DecodeSimd`/`AccelDecode`/
  `DECODE_*` 原子存储/`decode_*()` 取值器、`load` 落盘三行、`cfg:` 日志三键段；
  GUI `schema.rs` 同构对、`pages.rs` 加速页三行 + 摘要 `sum.decode` 行、
  `text.rs::decode_desc`、`app.rs::decode_row`、中英 locales 各 6 键、
  `rules decode_roundtrip_matrix`（主笛卡尔从未进三键，无 27 倍膨胀之忧）、
  `docs/config.md`/`simd.md` 的 decode 小节、`matrix.toml accel-decode-switches`
  case。保留：`resolve_accel` 末尾三家单比特跟随全局（`scalar`=关/其余=开，
  按 `copy_width()` 推，`V128` 取 hevc 下最高档 `Sse41`）+ 串口
  `accel: decode simd hevc=on/off vp9=… h263=…` 行（值全跟随全局，
  `hevc-probe` boot_expect 断言保留，语义已是全局）；`load` 日志去三键段。
  窗口期写出的 `[accel.decode]` 表双边直接忽略（serde 默认跳未知键），不断解析。
- 全量 29 档（用户拍板②，“全部都要”）：`accel.level` 从 9 档扩到 28 档 + `auto`
  （`scalar/sse/sse2/sse3/ssse3/sse41/sse42/popcnt/aes/pclmulqdq/avx/f16c/fma/
  avx2/bmi1/bmi2/lzcnt/movbe/rdrand/rdseed/sha/gfni/avx512f/cd/dq/bw/vl/avxvnni`，
  按 CPUID 叶序排，`target_feature` 小写无点形；老 `"avx512"` 按 `avx512f` 照读；
  `tbm`/`sse4a` 为 AMD 专有、QEMU `qemu64` 无，明确不收）。
  固件 `types.rs` ≡ GUI `schema.rs` 同构（`ALL`/`file_str`/`index`/`as_u8`/`from_u8`
  全同步）；GlassSlider 改 `0.0..=28.0`/`step 1.0`，`accel_f` 镜像照旧
  （另补 `sync_widgets` 漏掉的 `accel_f` 同步：读盘后滑杆不动会盖掉已载入档位）。
  位宽映射（`level_width`，见 `docs/simd.md` 档位→位宽表）：sse 系→X128，
  标量/密码类（popcnt/aes/pclmulqdq/lzcnt/movbe/rdrand/rdseed/sha/gfni）按下限
  X128，avx/f16c→V128，avx2/fma/bmi/avxvnni→Y256，avx512 系→Z512。
  `probe_max_width` 按 CPUID/XCR0 逐叶补全（复用 `avx_checker`/`common` 门控，
  缺叶恒钳住，不断 boot）。
- 矩阵：GUI 主笛卡尔保持 9 档代表集（`REPRESENTATIVE`，`avx512`→`avx512f` 改名
  后仍占一位）不膨胀；全量 29 档进新 `accel_levels_roundtrip` 表（含 alias +
  回滚兼容断言）。`matrix.toml` 头注释同步（`[accel] level` 全档口径）。
- 分拆保持：回滚后 `src/config` 合计仍 >1000 行（`mod.rs` ~870 + `types.rs` ~745），
  按行数口径不合回单文件。
- 遗留：固件 `cargo build/clippy/fmt` 被 sibling 未提交的
  `third_party/filmcraft-mpeg2v` 改动挡住（`idct.rs:48` `.round()` 在软浮点
  no_std 下无此方法），本轮只改了注释未碰其代码，待其合入后重验固件侧。

### 7.6 AVC 解码 SIMD 上线（2026-10-21，vendor rusty_h264-accel + 单比特跟随全局）

背景：`simd.md` AVC 节“标量 shipped、SIMD 待上游解禁”已过时——本轮把上游
`rusty_h264-accel 0.16.1` vendor 进 `third_party/rusty_h264-accel` 并自 port
no_std，`decoder`（`features = ["libm", "asm"]`）+ 根尾部 `[patch.crates-io]`
截流；`common` 保持外部箱不动。decoder 本体也不 fork，只加特性。

- 移植（三板斧同 §7.2）：`std::arch`→`core::arch`（25 处）；`OnceLock` 两处
  （x86_asm/hpel 的 `has_avx2`，std 侧原样缓存、no_std 侧恒真）；
  `AtomicU8`+`std::env` 两处（knobs 下 `tq_scalar_forced`/`abl_recon`，
  no_std 侧 env 读作未设置）；检测宏直调点（luma_mc×11、idct4x4×2、
  dequant、satd 宏、neon×8）经 `lib.rs` 的 `have_avx2()/have_sse41()/have_neon()`
  转接（std 原样探、no_std 恒真——`core` 在 no_std 下没有 feature-detect 宏，
  手写 `__cpuid` 就是第二套，故恒真，见下）。
- 总开关（VENDOR 新增缝，上游无）：`set_simd_enabled(bool)` +
  `pub(crate) simd_enabled()`（`core::AtomicBool`，初值恒 `true` ==
  上游行为）；解码可达入口（idct/deblock-dispatch!/mc/chroma/hpel/
  bs_masks/mb_uniform/dequant）关时走标量双胞胎（bool/Option 入口回退到
  `common` 自带标量路，census 记 scalar）；编码侧（satd/sad/dct）固件无调用，
  只保编译。`intra_pred` 纯标量无门可加。
- **设计偏离（实锤 E0152 逼的）**：原计划 vendor `default = ["std"]` 不可行——
  `common` 以 `optional`（未关 defaults）拉 accel，特性单向累加把 `std` 推进
  固件图，`cargo check` 即撞 `panic_impl`（E0152）。故 vendor `default = []`
 （`--features std` 才回上游行为），开关初值恒开（host 默认构建无人推，
  初值关会把 host 全打成标量）；`resolve_accel` 开机无条件推一次，
  首解只能跑在它后面。根另加 `rusty_h264-accel` 直连边（registry 名，
  patch 同截到本地；透传依赖调不到 crate 名，不加边写不了推）。
- 接线：`resolve_accel` 按 `copy_width()` 单比特推（`scalar`=关/其余=开）+
  串口 `accel: h264 decode isa = rusty_h264-accel: SIMD/scalar` 行
  （hevc 同款，供矩阵断言；另立项的 QEMU avc-probe case 再消费）；
  `avc_probe` 首解前幂等 `ensure_xsave_enabled`（hevc 同款）。
  无独立 `[accel.decode] h264` 键（§7.5 回滚口径：解码开关只跟随全局）。
- 验证链：host Baseline/High 320x240 三方逐字节一致（asm 路/标量路/ffmpeg；
  乱字节·截断双路优雅 0 帧）；720p census 双流全进内核（scalar=0，
  kernel_share=100%）；提速配对 dev baseline~1.3x/main~1.3x
  （口径 1.55x/1.23x，机器负载 16 噪声大、趋势一致，main 贴近）；
  vendor 4 配置 check（默认 no_std/`--features std`/no-default+census+knobs/
  UEFI no-default）+ 箱内单测 46×2（默认/`--features std`，test-only alloc
  前奏让上游 std 测试在 no_std 默认构建下可编）+ 根三门
  （终链 build/clippy 0 error/fmt）全绿。
- 遗留风险：开状态假设 AVX2/SSE4.1 可执行（无 SSE4.1/AVX2 老 CPU 上
  `level != scalar` 会 `#UD`；`deblock` 的“sse2”核本就无条件用 SSSE3/SSE4.1，
  上游原样）；详见 vendor `VENDOR.md` 固件门契约。

### 7.7 mpeg2v/theora 接线收尾（2026-10-21，六家齐，根+文档只动，不碰 vendor 树）

背景：filmcraft-mpeg2v（`simd` 整数 SSE2，`set_simd_enabled` 根导出）与
oxideav-theora（`simd` wide 整数核，`simd::set_simd_enabled`）的 vendor 树
与 VENDOR 固件契约已分别落地（§7.5 之后两笔 vendor 提交），根侧只差最后
一推。本轮一次性收尾，vendor 树零改动。

- 根 `Cargo.toml`：`filmcraft-mpeg2v` + `oxideav-theora` 各加
  `features = ["simd"]`（特性名以各自 vendor manifest 为准，皆为 `simd`）；
  两处“无 SIMD 概念”过期注释改写成清单比特语义（`scalar`=关/其余=开，
  关特性时调用即 no-op）。
- `src/config/mod.rs` `resolve_accel`：h263 双箱同款再加两行
  `filmcraft_mpeg2v::set_simd_enabled(on)` +
  `oxideav_theora::simd::set_simd_enabled(on)`
  （`on = copy_width() != U64` 单比特跟随全局）；串口 decode simd 行扩展到
  六家（`hevc/vp9/h263/h264/mpeg2v/theora`，on/off 值），hevc/h264 两条
  `isa` 行保留；文件头 + 函数注释“四家”改“六家”。
- 文档：`docs/simd.md` 接线表加 mpeg2v/theora 两行 + 标题/语义/悬空历史同步
  六家；`docs/config.md` 两处同步六家；`matrix.toml` 头注释 + `hevc-probe`
  `boot_expect` 同步六家断言（旧三家子串仍能命中，新行是精确全断言）。
- 验证：固件 `build/clippy/fmt -p uefi-player` + GUI host 单测 +
  qemu-matrix 单测 + `menu-smoke`/`hevc-probe` 全绿后 commit+push。
