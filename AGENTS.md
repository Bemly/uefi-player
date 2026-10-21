# uefi-player Agent 约束

本仓是 x86 UEFI 应用（`no_std`），目标只有 `x86_64-unknown-uefi`。
`third_party/sprout` 是 submodule（pin 原 commit，不 fork 上游仓库），
通过 `patches/` 叠加后取 5 个 lib 用，
本仓的约束与它的 `AGENTS.md` 无关，不要混用。

## 定位

x86 引导加载器（基于 sprout 扩展）+ SIMD 加速矩阵 + UEFI 播放器 + 引导动态壁纸。
详见 `docs/roadmap.md`，`docs/` 下的设计文档是唯一口径。

### sprout 全功能对齐（用户拍板，长期方向）

目标是**引导能力**与 sprout 持平，不是搬它的 UI/配置体系：

- 对齐清单（`src/boot/` 薄适配直调上游 `boot` lib，不自研重复逻辑；
  `third_party/sprout` pin 不动、不 fork，只用 `patches/` 叠 `[lib]`）：
  多卷自动发现（BLS/UKI/Linux 内核对/Windows/UEFI Shell）+ PE chainload +
  Linux 直启（kernel + initrd 拼接 + options + devicetree）+ 跨盘 chainload +
  BLS 全字段透传（options/多 initrd/devicetree/arch 过滤）+ boot counting 消费
  （首个直调落地：`consume_try` 调 `BootCounterTarget::consume` 真改名）。
- 不搬：sprout 的菜单/phase/extractor/driver/SBAT/交互配置语言。
  本仓交互层保持自己的（`player.toml` + config-gui + 我们的菜单），
  sprout 只当"上游实现参考"读（读它学思路，同 chainload 惯例），
  需要上游能力先看 5 个 lib 有没有，没有才在 `src/` 自研。
- 其余 6 家（`filmcraft/h263-rs/oxideav-theora/rusty_h264(+accel)/rusty_h265(+accel)/vp9dec`）
  2026-10-21 起同样 submodule + patch pin（用户拍板），冻结不跟上游新版：
  pin = submodule gitlink + `patches/*.patch`（功能 delta）+ `patches/*.VENDOR.md`（移植说明）。
- macOS 原生引导不做（要 bless/APFS，超出 UEFI 范围；sprout 也没有），
  只做 OpenCore 链式接力（`\EFI\OC\OpenCore.efi` 存在即链，`[boot] macos` 开关；
  不扫通用 `BOOTX64.EFI`，那可能是我们自己）。
- UKI 读全套 profile（`.osrel/.cmdline/.uname/.profile`，一 profile 一条目，
  标题/版本/sort-key 走上游同款，读不到用文件名）。

## 目录

- `src/`：UEFI 固件侧，`no_std`，`main/menu/graphics/video/fs/config/error` + `diag/*`。
  已有 `src/boot/`（sprout 扩展的引导项，薄适配直调）；不要动 `third_party/`
 （除 `patches/` 叠加后 apply）。
- `third_party/` 下 7 个 submodule 全部 pin 死不 fork，只叠 `patches/` 用：
  `sprout`（`edera-sprout-config/eficore/bls/parsing/boot`；`boot` 上游是纯 bin，
  无 lib target，path 依赖会被 cargo 静默忽略；用 `patches/` 叠 `[lib]` 后直调）+
  `oxideav-theora/vp9dec/rusty_h265/rusty_h264/filmcraft/h263-rs`
 （各家 no_std 移植，`patches/*.patch` 即功能 delta）。
  新 clone 跑一次 `git submodule update --init` + `patches/README.md` 里的 apply 全套。
  需要上游能力先看 sprout 5 个 lib 有没有，没有才在 `src/` 自研。
- `tools/config-gui`：host 侧独立 crate（`std`），跨平台 `player.toml` 编辑器。
  与 UEFI 主包零依赖关系，见下文“双构建”。
- `docs/`：给人看的设计文档 + 真机报告（`FWINFO*/HWBENCH*.TXT`）。
  `usb_reports/` 只留本地二进制（测试片源/EFI，不入库）。

## 双构建（UEFI 与 host 严格隔离）

```bash
# 固件侧（根目录，`.cargo/config` + rust-toolchain 已配好 target）
cargo build --target x86_64-unknown-uefi
cargo clippy --target x86_64-unknown-uefi
# `-p` 只查主包：`--all` 会连带 path 依赖箱一起查，vendored 上游箱
# （third_party/rusty_h265*）为保合流能力豁免 stock rustfmt（见其 VENDOR.md）
cargo fmt -p uefi-player --check

# host GUI（独立包，不进 UEFI 构建；必须显式 host target，
# 因为根 .cargo/config 把默认 target 钉成了 UEFI）
cargo run --manifest-path tools/config-gui/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')"
```

- `tools/config-gui` 保持独立包：不要把它加进根 `[workspace] members`，
  不要让根 UEFI 包依赖任何 `std` / GUI 轮子。
- `no_std` 侧新增依赖必须支持 `default-features = false` + `alloc`。

## 轮子优先（强制流程）

每次写新能力前必须按此顺序，结论写进 PR/文档，不许跳步：

1. **先搜轮子**：`crates.io` + `docs.rs` 按确切需求搜（如 “toml 编辑”、
   “egui 文件对话框”），看下载量、维护状态、MSRV（本仓线 1.92）、license、
   固件侧是否 `no_std` 可用。
2. **有就用**：`Cargo.toml` 始终不 pin 版本（禁止 `=x.y.z` 精确锁定），
   用 caret 兼容范围（如 `eframe 0.35` 即 `^0.35`）。记录当时测过的组合（如 `eframe 0.35 / egui_glass 0.1.5 / wgpu 29`，
   见 `tools/config-gui/Cargo.toml`），优先选与现有依赖同大版本的。
3. **没有才造**：自研放 `src/`（固件）或对应工具包内，文件头注释写清
   “搜过什么、为什么不用”（如 “`movntdqa` 要求 WC 源，对普通 RAM 会 #GP，
   故不用 XX crate，见 `gop_simd.rs`”这种粒度）。
4. **UEFI 特殊**：`asm!` 显式向量寄存器优先于内在函数（防 LLVM 标量化，
   血泪见 `hw_bench.rs` 注释）；浮点向量在软浮点历史下不执行，只报 CPUID。

已验证组合（改任一先重验 `cargo check` 双端）：

| 侧 | 组合 |
|---|---|
| host GUI | `eframe 0.35`（`wgpu` 特性做驱动）+ `egui 0.35` + `egui-wgpu 0.35` + `egui_glass 0.1.5` + `wgpu 29` + `rfd 0.17` + `toml 1` + `rust-i18n 4` |
| 固件 | `uefi 0.41 / uefi-raw 0.17` + `toml 1.x no_std` + `serde alloc` |

`egui_glass` 硬性要求：`eframe::Renderer::Wgpu`，glow 不可用；
`egui_glass::init` 每 renderer 调一次，MSAA 数与 `NativeOptions` 一致。

## host GUI 件库锁死（只管 `tools/config-gui`，固件侧 `no_std` 不可能用，无需遵守）

- 可见控件只用库内件：`Glass` / `GlassButton` / `GlassSlider` /
  `GlassToolbar` / `GlassStyle` / `LiveBackdrop` / `paint_glass`。
  动作按钮、导航、单选开关全是 `GlassButton`（选中态描蓝 + ●，`● ○` 全量字体自带）；
  档位这类多值开关用 `GlassSlider` 拨下标（库内滑杆只吃 `f32`，镜像字段写法见
  `tools/config-gui/src/main.rs` 的 `*_f` 字段）。
- 禁止一切有玻璃替代品的普通控件：`Button` / `ComboBox` / `TextEdit` /
  `Radio` / `Checkbox` / 普通 `Slider` / `DragValue` / `Separator` 等。
  分隔一律用留白（`add_space`），不画线；顶栏胶囊/中央大薄片可用
  （圆角无直边；曾因 Panel 边界直线误伤过它们，真凶是下条，已恢复）。
- 允许：`Panel` / `Area` / `ScrollArea` 等无玻璃等价物的布局原语；
  `label` / `heading` / `small` / `monospace` 等纯文本（库内无文本件）。
  但 `Panel` 默认带描边分隔线（`show_separator_line` 默认为 true，
  实机截屏确认），一律显式 `.show_separator_line(false).resizable(false)`。
- 新增 UI 前先查库（`docs.rs/egui_glass` + 本地 registry 源码确认签名），
  没有才用原语，并注释说明。玻璃嵌玻璃（官方不建议）须经用户拍板，
  拍板结论写进文件头注释。
- 无 GPU 兜底分支除外（玻璃管线没注册，只能用普通控件；renderer 已强制
  Wgpu，实际走不到）。
- 字体禁止子集化（血泪：子集漏字只能事后补）：`tools/config-gui/assets/` 下
  直接放上游原包全量文件（当前 `fusion-pixel-10px-proportional-zh_hans.ttf`
  v2026.10.16，约 4MB），缺字加文案即可，不许 `pyftsubset` 裁剪。
  体积换完整（host 工具，不进 UEFI 固件，无体积压力）。

## 测试 100% Rust（硬性）

- 本仓测试代码只用 Rust 写：GUI 黑盒矩阵是 `tools/config-gui` 内 `#[cfg(test)]`
  表格测试（`blackbox_matrix`），QEMU 黑盒矩阵是 `tools/qemu-matrix` 纯 Rust
  runner（`std` + 与 GUI 同版本的 `serde`/`toml`，不许引 `clap`/`regex` 等新轮子，
  手写即可，结论见该包 `Cargo.toml` 头注释）。
- 禁止 Python/shell 测试脚本（发现即删了重写）：系统工具（`qemu`/`hdiutil`/
  `mtools`）只许当子进程调，不许写测试逻辑。
- 黑盒矩阵测试（两套，必同步更新）：
  - GUI 侧：`tools/config-gui/src/main.rs` 的 `blackbox_matrix` 模组——TOML 全档位
    笛卡尔回写、旧文件兼容、读盘/落盘钳制、`bench_pending`、`tune_visible`。
  - QEMU 侧：`tools/qemu-matrix/matrix/`——每个 case 内联 `player.toml` +
    串口断言 + 按键脚本（事件驱动，`wait_for` 卡点，不靠裸睡；屏显-only 界面
    靠 `log::` 标记同步，见该目录 `README.md`）。
- 新增/修改功能必同步更新两套矩阵（GUI 加表行，QEMU 加 case 并写 `covers`），提交前两套全绿；
  固件新串口日志想想矩阵能不能用上，能用就地加断言。
  迭代加速可用 `--changed` 只跑改动波及的 case（`--list --changed` 预览，menu-smoke 必带锚点），
  它不替代提交门禁的全矩阵。

## 代码约定

- 发现疑似笔误/命名问题（拼写、术语、前后不一致）先指出再动手，不将错就错。

- 沿袭周边写法：中文注释允许（存量如此），`emit!` 双写屏显+串口，
  固件输出统一走 `Screen::draw_str`。
- GOP 是独占协议：`Shared` 借 `Screen`，`Exclusive` 先 `drop` 再重建
  （`registry.rs` 的两种 `Entry`，不要第三种）。
- 诊断项可返回式报错，只生产路径（播视频）走 `handle_fatal`。
- 单文件 ≤1000 行（`wc -l` 口径，含注释空行；UEFI 固件/host GUI/工具一视同仁，
  数据文件如 `matrix/` 下分组 `*.toml` 同理）：超了说明职责过载，必须按子分类拆模块，
  不许继续往里堆。新文件落地即守，存量超限（`config-gui main`/`gop_simd`/
  `graphics`/`config`/`video`/`gop_bench`/`matrix/`）按触达顺序拆（2026-10-21 六件套已拆完：`gop_bench`/`graphics`/`video`/`gop_simd`/`qemu main`/`matrix/`，各子文件均 <1000 行）。
- 提交用 Conventional Commits（`feat/fix/docs`），不加 Agent 署名。
- 版本号用 CalVer `YY.MM.DD`（如 `26.10.20`；
  主包与 `uefi-async` 各自独立按日期 bump，`tools/*` 内部工具保持 `0.1.0` 不动）。
- 每次改完自动提交并推送到 GitHub：`check/clippy/fmt` 全绿就 `commit + push`，
  不必等用户开口；以用户最新说明为准。
- 本机 sudo 口令只存登录钥匙串（服务名 `uefi-player-sudo`），取用
  `security find-generic-password -s uefi-player-sudo -w`；绝不写进仓库任何文件
  （本仓自动推送且为 PUBLIC，落盘即公开）。
