# oxideav-theora —— submodule + patch pin（2026-10-21 起冻结，不跟上游新版）

- 上游：https://github.com/OxideAV/oxideav-theora @ tag v0.0.12 (fb46b47)
- 本仓形态：submodule（`third_party/oxideav-theora`，frozen）+ `patches/oxideav-theora.patch`（功能 delta）
- 应用：`git -C third_party/oxideav-theora apply ../../patches/oxideav-theora.patch`
- 本文件内 'VENDOR.md' 指代即本文件；'README.upstream.md' 在 submodule 内已有原版。

---

# Vendored `oxideav-theora` — no_std port (decoder-only scalar)

- 上游：https://github.com/OxideAV/oxideav-theora ，版本 **0.0.12**
  （crates.io 2026-09-01，MIT，`LICENSE` 见本目录；`README.upstream.md`
  为上游 README 原样保留）。
- 本目录是“上游 0.0.12 全树（去 tests/examples/fuzz，见下）+ no_std 移植层”，
  不是 fork 上游仓库：上游目前**不支持 no_std**（`std::collections`/`std::fmt`/
  `std::error` 无特性门；`cargo check --target x86_64-unknown-uefi` 能过只是因为
  本机新工具链自带 UEFI 的 `libstd`，链接/运行仍不可用，且本仓禁固件侧 `std`）。
  等上游自己长出 no_std 分层就切回 crates.io，把这份 vendor 删掉。
- `third_party/sprout` 是只读 submodule（见根 AGENTS.md），**这份不是**：
  用户拍板 vendor 进来改（2026-10-21），与 sprout 的 pin 政策无关。

## 改了什么（相对上游 0.0.12）

1. 三箱特性 `default = ["std", "framework", "encoder"]`（默认开，行为与上游完全一致）；
   关掉即 firmware 形：`#![no_std]` + `alloc`（`extern crate alloc` + 无条件 alloc
   前奏：与 std 前奏同类型，`std` 构建不受影响）+ 无 framework + 无 encoder。
2. 口径：`std` 关时无 env/eprintln（本箱生产代码本来就零 env、零 eprintln，
   只有 tests 用）；诊断 Display 沿用上游的 `core::fmt` 实现。
3. `framework`（默认开）是 oxideav-core 胶水（TheoraDecoder/TheoraEncoder/
   register/make_*/cropped_to_video_frame/HeaderState/plane_to_top_down +
   `use oxideav_core::RuntimeContext`），以及 `oxideav-core` 可选依赖。
   `framework` 隐含 `std`：上游 oxideav-core 是 std-only
  （thiserror/serde_json/`std::io`，无特性可关），bad 组合用特性蕴含直接堵死。
   这是比 rusty_h265 稍大的上游 delta：h265 保留了兄弟箱依赖，本箱把 20k 行的
   框架箱依赖改成了可选——固件侧永远关掉，只留 `FrameDecoder` 裸解码路径。
4. `encoder`（默认开）是 FrameEncoder + 全部前向/RD/码率控制机器
  （含 `vp31_huffman_data` 整模块、`BitWriter`、Token 规划器、Rate* 族、
   `SourceFrame`、`with_tuned/with_gop_tuned` 等方法级门），同样隐含 `std`：
   唯一的 std-only 调用 `RateModel::x` 的 f64 `.ln()` 在 no_std 下无对应方法，
   首轮不碰（libm 跟进是另立项；h264 有 libm 先例）。
   固件 `default-features = false` = 纯标量解码器，无 SIMD 概念可留
   （本箱零 intrinsic/`target_feature`，关掉特性即全部）。
5. `register_codecs` 的 `.encoder(make_encoder)` 一行拆成 encoder 门控
   （decoder 注册恒在；见该处 `VENDOR(uefi-player):` 注释）。
6. `[[test]]`/examples/fuzz 未 vendor：examples（rd_ladder 丈量台）、fuzz 子箱是
   host-only 台架；tests 目录未 vendor（嵌入式单测在 `src/lib.rs` 的 `mod tests`
   里，随树保留，host 可跑；语料库不在 git 树，无外部向量可带）。

## 同步上游

`src/` 内 `// VENDOR(uefi-player):` 注释标出每一处移植点（本轮约 113 处）；
合上游新版时按注释逐处重放。`src/bin` 本箱没有（上游台架在 examples/，未 vendor）；
`tests/` 未 vendor（见上）。

## 本仓约定豁免

- 这是第三方上游代码（MIT，Mark Karpeles），**不受**“单文件 ≤1000 行”约束
  （`src/lib.rs` 单文件 44k 行，上游即如此）——只要求新增的移植层自身保持
  小而集中（本轮实际只有 crate 根前奏 + 逐处 2 行门）。
- 上游树本身不是 stock-rustfmt 干净的，**禁止对 vendor 树跑 `rustfmt`**
  （会产生巨大无关 diff，毁掉合 upstream 的能力）；移植行沿用文件局部风格。
  根门禁因此用 `cargo fmt -p uefi-player --check`
  （`--all` 会连带 path 依赖一起查；见根 AGENTS.md「双构建」注释）。

## SIMD 卷土重来（2026-10-21）：用户拍板必须上，e5c2ddf  verdict 作废

- 热路（`src/lib.rs` 全整数；`f64` 仅 `granule_position_seconds` 一处除法，
  无向量化对象；encoder 区前向/RD/码率机器属 `encoder` 门之后，
  decoder-only 构建已排除）：`inverse_dct_1d/2d`（§7.9.3.1 55 步定点
  butterfly，`wrapping` + `>>16`）→ `dequantize_block`（64 乘，zig-zag 查表
  串行）→ `reconstruct_block`（8×8 加钳，`p = (pred+res).clamp(0,255)`）→
  `lflim` + `horizontal/vertical_loop_filter_edge`（4-tap + 分段钳制，每边
  8 行/列分支）。熵解码/MV/帧内预测串行不动（Amdahl，本轮不碰）。

### 轮子实搜结论（2026-10-21 crates.io + docs.rs，不许跳步，结论在此）

- `wide 1.7.1`（84.0M 下载，MSRV 1.89，Zlib/Apache-2.0/MIT，
  `default-features = false` 可 no_std）——**用**。`i16x8::mul_keep_high`
  就是 butterfly 要的 `(a*b)>>16`（`pmulhw`，SSE2）；另有 wrapping 加减、
  `saturating_add`（`paddsw`，重建钳制）、`min`/`max`（`pminsw/pmaxsw`）、
  `simd_gt`+`select`（`pcmpgtw`+与/或，`lflim` 去分支）、`unbounded_shr_scalar`
  （`psraw`）、`narrow_i16x8`（`packuswb`，饱和钳制）。其 `std` 特性只加速
  f32/f64 `sqrt`，整数核零触及。与在树 h263-rs 同版本（`1.7.1`），共享
  lockfile 条目，零新增依赖（AGENTS 同大版本优先）。
- `safe_arch 1.2.0`（82.7M，同 license，no_std 原生）——不用：`core::arch`
  薄包装 = 内在函数，无 XSAVE 集成，相对 wide 零附加价值（AGENTS 要
  `asm!` 优先，h263 前例已在同机上选 wide 弃 safe_arch）。
- `pulp 0.22.3`（31.9M，MIT，默认 `x86-v3`）——不用：AVX2 基线撞 UEFI
  SSE2 基线 + XSAVE 门控，且自带分发（禁第二套 CPUID）。
- 自研部分：wide 的 runtime-index `shuffle` 在纯 SSE2 下标量化
  （`u16x8_.rs` 的 `pick!` 在无 SSSE3 时走 `to_array` 标量回退），而 55 步
  butterfly 的配对每级都变，跨车道重排不可避免——故 butterfly 不按 8 车道
  做 1 个变换（零 lane 内并行度），而是 8 行变换一次跑完（SoA，见下）；
  唯一的 8×8 转置用 `core::arch` 的 `punpcklwd/hwd` 手排（SSE2 基线 1:1
  指令，`#[allow(unsafe_code)]` 仅限转置函数 + SAFETY，其余全文件 safe，
  crate 级 `#![deny(unsafe_code)]` 保持）。

### 去分支方案（A/B/C 逐处落地，全部 `VENDOR(uefi-player):` 标点）

- A) 55 步 butterfly：**无分支可消**（直本身是 55 步直线码）——关键是截断
  语义即车道语义，零掩码开销。8 个 `i16x8` 寄存器 `V0..V7`（`Vk` = 8 个行
  变换的第 `k` 个元，列优先 SoA），55 步逐字映射（`t[i]` → `Vi`）：
  加减 wrapping（`paddw/psubw`）、乘高半部（`pmulhw`）。Table 7.65 中
  `> i16::MAX` 的常数（C3/C4/S3/S6/S7）以 `C−65536` 进入 `pmulhw` 再 `+y`
  补回——恒等式 `(y*C)>>16 = ((y*(C−65536))>>16) + y`（`y*65536` 是 `2^16`
  倍数，算术右移对其精确分配），`mul_bigc` 注释有逐值验算。
  逐位一致证明：车道全程跟踪标量值 mod 2^16（wrapping 加减/mulh 保持同余；
  标量每次 `as i16` 截断点车道恒等于已截断值；其余下游全线性），截断点与
  输出 `as i16` 逐位一致。行列两遍共用同一 `idct_8wide`，中间一次转置。
  常数 splat 按使用点现播（不 hoist：8 数据 + 1 常数 + 临时恰好塞下 16 xmm，
  hoist 7 个 splat 会 spill）。`(X+8)>>4` 用 i16 分解
  `(X>>4) + (((X&15)+8)>>4)`（`X&15` 即数学 mod 16，负数亦然；省掉 i32
  拓宽的 2×unpack+shift+pack）。
- B) dequant+reconstruct 融合：**dequant 保持标量**（实测：8-wide `pmullw`
  gather 版比标量 imul 环慢 2.4×——SSE2 无 gather，`pinsrw` 串行链与
  store-forwarding 碎片双输；`dequantize_block` 维持上游原样，零分发）。
  融合落在其余两处：① 2D 管线融合（行遍→转置→列遍→偏置全寄存器传递，
  `dqc` 仅 L1 中转，转置-in 实测 0.8ns）；② 重建 2(f) 融成单遍向量核
  （`paddsw` + `packuswb`，`add_pred_res_clamp`，三路 RES——DC 填充/全
  IDCT/零残差——共用）。
- C) loop_filter 去分支化：`lflim` 五段折线 = 无分支 tent
  `clamp(r,±2l)` → `select` 折叠 → 越界选零（`pminsw/pmaxsw/pcmpgtw`+与/或，
  SSE2 mask-select 形状，无 `blendv`）；`clamp_to_u8` 三分支 =
  `packuswb`。横边 8 行 gather + 纵边 4 行读（列即车道，打包即连续两行，
  纵边零 scatter）。车道填充一律经 `load_i16x8`（`black_box` 挡住 LLVM 的
  `pinsrw` SLP——无 fence 时反汇编见 300+ `pinsrw` 串行链；有 fence 后
  `movdqu` 单发，边缘核翻正为胜）。
- NT/sfence/对齐：本核只有栈/小缓冲 `loadu/storeu`，无流式存储——nt 不适用
  （`movntdqa` 要求 WC 源，对普通 RAM 会 #GP，见 `gop_simd.rs` 同款粒度）；
  无 AVX/AVX2（SSE2 基线即可，无需 `check_avx_support`，也无第二套 CPUID）。

### 特性门与固件契约（decoder-only 口径不变，framework/encoder 门保持）

- `simd = ["dep:wide"]`（纯自研转置除外，零新依赖）；`std = ["simd",
  "wide/std"]`（std 蕴含 simd，默认形与上游逐字节一致——开关默认 ON 下
  跑 SIMD 路径，但互锁单测逐字节锁定）。`default` 不变
  `["std", "framework", "encoder"]`；`framework`/`encoder` 各自蕴含 `std`
  （从而蕴含 `simd`），bad 组合仍不可能。固件形 `--no-default-features`
  （纯标量）与 `--no-default-features --features simd`（no_std SIMD）并存。
- 运行时单比特：`simd::set_simd_enabled` 公开（无 `simd` 时 no-op），标量
  twin 恒编译。固件接线座（本箱不管 `src/`，由接线方按此契约推）：
  `resolve_accel` 后按 `copy_width()` 单比特推——`scalar`=关/其余=开
  （参考 h263 e5dd289 开关 + h265 `force_*` 推法；本箱永不自研 CPUID）。
  `Relaxed` 足够（双路逐字节一致，撕裂读至多混帧不损坏）。工作站
  `src/config` 已有 `decode_hevc/vp9/h263` 三键（`DecodeSimd` 三态），theora
  键（`decode_theora` + `set_simd_enabled(decode_on(...))` + 根
  `oxideav-theora/features=["simd"]`）待接线方补——本箱只动
  `third_party/oxideav-theora`，不动 `src/`/`Cargo.toml`/`Cargo.lock`。
- `rust-version` 1.85 → 1.89（`wide 1.7.1` MSRV，仍低于仓线 1.92）。

### 正确性证据（双路互锁，规范向量 + 尺寸×强度 sweep 逐字节比对）

- 箱内嵌入式单测（默认形，开关 ON/OFF 双跑同一二进制；save/restore 防
  并行干扰）：butterfly 广播隔离（19 边缘值×8 位置 + 3000 随机）；
  转置已知模式 + 对合；2D sweep（DC 平坦/负 DC/小 DC/行列朝向规范向量 +
  2000 随机密块 + 500 稀疏 + 64 位置×4 极值 impulses + 全 ±32768）；
  2(f) 加钳（6 pred 角 × 8 res 角全积 + 300 随机）；
  `lflim_8wide` 对标量 `lflim` 全断点穷举（`l` 0..=255 × 每 `l` 17 个
  `±2l/±l/0/±1` 邻域 = 4352 车道）；双边 sweep（常数/台阶/渐变/棋盘/
  200 随机平面 × 7 强度 × 4 锚点，全平面比对 + 足迹/长度守卫双路同错）；
  `reconstruct_block` 生产路径（DC-only/全 DCT 随机系数×qi 角/uncoded）。
  677 存量单测在 SIMD-ON 下原样全绿（精确期望即互锁向量）。
- 整帧口径：待 Ogg 拆帧落地后补整帧 ffmpeg 双解（Y 平面 max|Δ|≤4 /
  PSNR≥58dB / 差异≤3% / 帧数精确——沿 mpeg2v 口径，见下“遗留风险”）。
  本轮用合成块级 sweep + 存量端到端 fixture 测试（`decode_frame_*` 全绿，
  含 1080p 双帧 trace）代替。

### 实测性能（Intel Mac，min-of-3；SSE2-only 列 = 固件 codegen 模拟）

| 核 | 标量 | SIMD | 结论 |
|---|---|---|---|
| IDCT 2D（密块）| 119ns | 164ns | 0.72×（延迟界：55 步串行链 ×2，见下）|
| 环路滤波双边（随机纹理）| 506–1114ns（分支预测风暴，方差大）| ~200ns（确定性）| 2.6–5× |
| 重建全路径 intra | — | — | ~1.04×（加钳胜补 IDCT 平）|
| 反量化（gather 版，已删）| 58ns | 139ns | 0.42×（回退标量，见上）|

- IDCT 未能更快的原因（已用反汇编确认 codegen 干净：UEFI 物件 40
  pmulhw/pmullw + 105 pack/clamp/min/max + 608 punpck，零 SSE4.1/零 VEX；
  SIMD kernel 内无 `pinsrw` 串行链——`load_i16x8` fence 后车道填充全是
  `movdqu`）：55 步是单依赖链 × 行列两遍串行，
  而标量 16 个独立 1D 变换天然 ILP——这是算法结构上限，不是实现问题；
  多块批处理可破（要改驱动循环，本轮不动）。典型稀疏内容 IDCT 切片本就
  1–2ms，0.72× 绝对值 +0.2–0.5ms，由边缘核的 −0.5–1ms 对冲；净帧级持平偏正。
- UEFI 终链（`.efi`）以标量形编译本箱（根未接线前）；`--features simd`
  的 no_std 终链待根接线后由接线方验证（本箱 `cargo check` 已过）。

### 验证门（本轮实跑）

- host 单测默认形 686 全绿；4 配置 check 零（新）警告：默认 /
  无默认 / 无默认+simd / UEFI（x86_64-unknown-uefi，默认+simd 双形）；
  根三门（`check/clippy/fmt -p uefi-player`）待接线后由接线方复验——本箱
  未动根文件，vendor 树豁免 fmt（合流能力，沿 handover §7.3）。
- QEMU/theora-probe：Ogg 另立项后补（本箱不做拆帧）。

### Ogg carriage（固件侧另立项，不在本箱；沿用 e5c2ddf 口径）

- 本箱只吃已拆帧 packet（§6.1 分类 + §A.2.3 granule 映射是公共 API，
  decoder-only 构建保留），固件侧 OggS 拆帧手写（100% Rust）另立项；
  校验和口径先定死只取 Y 平面（Theora 输出三平面，下原点行序，
  `crop_for_display` 后翻转 + YUV→BGRA 是固件侧集成活）。

### 遗留风险

1. Ogg 拆帧未落地 → 整帧 ffmpeg 双解（Y 口径）未补，帧级证据缺一块；
   块级 sweep + 存量端到端 fixture（1080p trace）暂代。
2. IDCT SIMD 在延迟界结构上追不上标量 ILP（0.72× 密块）；帧级净效果靠
   边缘核对冲——若未来 profile 显示 IDCT 主导，可做跨块批处理（改驱动，
   另立项），或固件侧按 block-type 动态选路（开关已就位）。
3. 微架构差异：以上数字是 Intel Mac 实测；固件跑在未知 x86-64 上，
   SSE2 确定性（无 autovec 赌博）是本轮真正的可移植收益。
4. `cargo test` 在 no_std 形下全箱不可跑（test harness 要 std + 存量测试
   引用 framework/encoder/std——e5c2ddf 前即如此，非本轮引入）：no_std 双形
   只做 `check`，双路行为由默认形开关翻转覆盖。

## 播放器 `format` 接线状态（2026-10-21）：格式可选、解码后继

- `[play] format = "theora"` 已进固件/GUI 同构 schema（小写串 `theora`，旧文件缺键照读），
  GUI 播放页可选；但 `Clip` 分发未接——选中即明确报错
  （“selectable, decoder follows later”），不硬编解码，不断解析/boot。
- 后继项（QEMU 矩阵 `format-theora-parse` 只验解析，不断言解码）：
  样片（Ogg 页拆帧另立项，本箱只吃已拆帧 packet；需先备拆帧后小样片）
  → `Clip::from_theora_*`（IDCT butterfly/重建加钳/环路滤波输出转 BGRA，
  校验和口径只取 Y 平面，见上节 Ogg carriage）→ 矩阵 case。
