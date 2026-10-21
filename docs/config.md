# player.toml 完整 reference

> 位置：启动卷根目录（与 `HWBENCH*.TXT` 同层）。解析：`toml` 1.x（no_std）+ serde。
> 缺失/非法 → 全默认 + 屏显一行警告，不 halt。`[wallpaper]` 缺表同样按默认处理
> （GUI 回归测试覆盖旧文件照读）。与 `tools/config-gui` 同 schema（双边同步改）。

```toml
version = 1

[accel]
level = "auto"          # 性能序（滑杆档位）：auto | scalar | sse | sse2 | sse3 | ssse3
                        # | sse41 | sse42 | popcnt | lzcnt | movbe | aes | pclmulqdq
                        # | rdrand | rdseed | sha | gfni | tbm | sse4a（AMD 专有垫底，
                        #   扩展叶 8000_0001H ECX bit 21/6；qemu64 无此叶，
                        #   显式配了即按 X128 钳住用，不断 boot）
                        # | avx | f16c | fma | avx2 | bmi1 | bmi2 | avxvnni
                        # | avx512f | avx512cd | avx512dq | avx512bw | avx512vl
                        # auto = 开机探最高可用；显式超硬件警告+钳住
                        # 解码 SIMD 无独立开关：六家（hevc/vp9/h263/h264/mpeg2v/theora）单比特跟随全局
                        #（level=scalar 则关，其余开），见 docs/simd.md

[render]
mode    = "auto"        # auto | direct | blt
vsync   = "auto"        # auto | static | half | off（软件帧预算）
blt_op  = "buffer_to_video"  # buffer_to_video | video_to_video | video_fill | video_to_blt_buffer
# 只有 buffer_to_video 能上屏播视频（单发整推 + 混合整推都用它；GUI 只放行这一项，
# 其余置灰展示不删）；其余三项只在矩阵里单发出数（引擎能力对照，不参选），Blt 不在测时跳过。
# workers/dist 已删除 `[render]` 旧键（正典只剩 `[schedule]`；旧文件残留键按 serde 默认忽略）。
# 首启校准记忆（固件回写，GUI 只读展示；"auto"/0 = 未测，下次启动必测）：
measured_render  = "auto"  # "direct" | "blt"
measured_workers = "auto"  # "bsp_only" | "all"
measured_dist    = "auto"  # "static" | "tile" | "dynamic"
measured_vsync   = "auto"  # "static" | "off"（auto 探测结论；static/half 不用它）
measured_vsync_fps = 0    # half 实测预算（fps；0 = 未测，下次启动现测取半并回写）
measured_cores   = 0       # 校准时机型 AP+BSP 总数

[render.tune]
frames    = 30          # 每次矩阵/微基准的帧数（>=1，GUI 1-120）
tile_rows = 64          # tile 高（行），MP 自调度切块粒度（8-512）
vsync_fps = 60          # vsync 预算帧率：static 的封顶 + auto 探测达标线（15-240）
buffers   = 0           # 后缓冲帧数（0-16；0 = 直写前台，>0 = 影子环 + 整推；Blt 单发路不用）
window    = 4           # 流式供片窗口（`[play] feed = "stream"` 的解码帧缓存数，1-32；
                        # 只对 stream 生效，其余供片忽略；缺键老文件按 4 照读）

[play]
fps = "auto"            # auto | max | 1..240
                        # auto: 首启实测最大帧率并回写，按 min(视频帧率, 实测*3/4) 跑
format = "qois"         # 片源格式：qois（1080p/video.qois）| raw（1080p/frameNNNN.raw）
                        # | hevc（video.mp4 hev 轨优先，无则 video.hevc Annex-B）
                        # | avc（video.mp4 avc 轨，石雨堂拆 avcC → Annex-B 软解）
                        # | vp9（video.mp4 vp09 轨优先，无则 video.ivf）
                        # | h263（video.h263 裸流）| mpeg2v（video.m2v 裸基本流，只收 4:2:0）
                        # | theora（video.ogv，OggS 磁盘原格式，只收 4:2:0）；
                        #   旧文件缺键按 qois 照读
feed = "clip"           # 供片模式：clip（开机全量预解码，默认）| staged（qois 显式三段，
                        # 其余七格式结构等价 clip）| stream（window 窗口边播边解，
                        # 游标之后淘汰）| zero_copy（单复用缓冲即时解码）
                        # | single（每帧重读文件+重解码，零缓存下限对照，NEW）；
                        # 只切 Clip 内部拿帧方式，不管上传路（blt/direct/mp/nano/dynamic 零改）；
                        # 多核（mp_draw）/动态调度/壁纸恒 clip/eager；旧文件缺键按 clip 照读；
                        # 生效值可被 `[decode.<fmt>] fetch` 按格式覆盖（串口 `video: feed=` 取覆盖值）
measured_max_fps = 0    # 固件实测回写（0 = 未测）；GUI 只读展示

# 解码表（统一控制平面，八格式各独立五键）：
# threads bsp_only/all（有状态格式 all 解析但警告并驻 bsp_only；qois/raw 即时兑现）；
# zero_copy/streaming 布尔（全格式可行）；buffers 0=跟随 tune.window/1-32 显式；
# fetch 缺省继承全局 [play] feed，显式 clip/staged/stream/zero_copy/single 只覆盖本格式
# （壁纸恒 eager 无视覆盖；串口生效 `video: feed=` + ` fetchovr=<fmt>` 令牌）。
# 缺表缺键按默认照读（threads=bsp_only，布尔关，buffers=0，fetch 继承全局）。
[decode.qois]
threads = "bsp_only"    # bsp_only | all（无状态，all 即时兑现 honored 行）
zero_copy = false       # 单缓冲即时解码开关
streaming = false       # 边播边解开关
buffers = 0             # 0=跟随 tune.window，1-32 显式覆盖
# fetch 缺省不写 = 继承全局 feed；显式如 fetch = "stream" 只覆盖本格式
[decode.raw]
threads = "bsp_only"    # bsp_only | all（零解码，all 即时兑现 honored 行）
zero_copy = false
streaming = false
buffers = 0
[decode.hevc]
threads = "bsp_only"    # bsp_only | all（有状态，all 解析但警告并驻 bsp_only）
zero_copy = false       # 单缓冲即时解码开关
streaming = false       # 边播边解开关
buffers = 0             # 0=跟随 tune.window，1-32 显式覆盖
# fetch = "stream"      # 显式覆盖示例：本格式生效 feed 取 stream（全局 clip 不变）
[decode.avc]
threads = "bsp_only"
zero_copy = false
streaming = false
buffers = 0
[decode.vp9]
threads = "bsp_only"
zero_copy = false
streaming = false
buffers = 0
[decode.h263]
threads = "bsp_only"
zero_copy = false
streaming = false
buffers = 0
[decode.mpeg2v]
threads = "bsp_only"
zero_copy = false
streaming = false
buffers = 0
[decode.theora]
threads = "bsp_only"
zero_copy = false
streaming = false
buffers = 0

[schedule]
backend = "manual"      # 调度后端：manual（手写 MP 三路，workers/dist 生效）
                        # | nano（单核执行器泵走渲染路径，workers/dist 忽略）
                        # | dynamic（多核 steal 泵，只直写前台；见下回退）
workers = "auto"        # manual 后端多核开关：auto | bsp_only | all（只对 direct 有效）
dist = "auto"           # manual 后端分工：auto | static | tile | dynamic（只对 direct+all 有效）

[wallpaper]
enable = false          # 菜单背景 + 引导过渡总开关
format = "raw"          # 片源格式（与 [play] 同一矩阵，八格式全通）：
                          # raw（wallpaper/frameNNNN.raw，默认）| qois（wallpaper/wall.qois）
                          # | hevc（wall.mp4 hev 轨优先，无则 wall.hevc）
                          # | avc（wall.mp4 avc 轨）| vp9（wall.mp4 vp09 轨优先，无则 wall.ivf）
                          # | h263（wall.h263）| mpeg2v（wall.m2v）| theora（wall.ogv）
popup_freeze = true     # 选项弹窗时冻结壁纸（false 则行区继续播；GUI 有开关）
fps = "15"               # 背景帧率三态（与 [play] fps 同构）：auto（最左，跟 vsync；
                          # vsync 关则不限速）| max（最右，不限速）| 1-240（中间显式封顶）
                          # 默认 "15"
frames = 0              # B 过渡封顶帧数（0 = 全播预载集）
preload = 8             # 预载帧数上限（1-32；0 = 不封顶，和播片一样拉满；
                          # 1080p BGRA 单帧约 8MB，长动画注意内存）

[boot]
auto = true             # 多卷自动发现总开关（关 = 只列本卷 BLS，老行为）
windows = true          # 自动发现 Windows Boot Manager（bootmgfw.efi 存在即链）
linux = true            # 自动发现 Linux 内核对（\boot + \ 扫 vmlinuz*/image* + initramfs 配对）
shell = true            # 自动发现 UEFI Shell（shellx64.efi 三处候选）
uki = true              # 自动发现 EFI/Linux/*.efi（读 .uname/.profile 建条目，读不到用文件名）
macos = true            # OpenCore 链式接力（EFI/OC/OpenCore.efi 存在即链；不扫通用 BOOTX64.EFI）
strict = false          # 严格模式：BLS 只认本卷 + XBOOTLDR 卷（默认全卷都认）

[async]
executor = "all"        # 诊断项 a 跑哪条执行器线：all（默认，都跑）| nano（单核线）| dynamic（多核线）
                        # 只管诊断覆盖，不切生产调度（播片/壁纸仍是手写 MP，迁移另立项）
```

## 生效规则速查

- `render.mode=auto`：有记忆复用（快启），无记忆全矩阵校准（direct/blt ×
  BSP/全核 × vsync 探测）并回写 `measured_*`；全显式跳过矩阵。
- `workers=auto`：直写路 BSP 单核 vs 全核取快；Blt 路径忽略本项。
- `dist=auto`：全核路静态分区 vs tile 自调度 vs dynamic 加权三选一（tile 按 64 行切块，
  BSP+AP 从共享游标领活；异构大小核/负载不均时赢；OSD 行划出 tile，
  AP 不再盖 OSD 字；dynamic 按各核实测耗时加权分行，快核多拿，播放中逐轮重算）。
  单核/Blt 下无意义，不记。
- `vsync=auto`：冠军路径按 `tune.vsync_fps` 预算零超期则开，否则关；
  开 = 播放叠预算帽（封顶数即滑杆值，默认 60）。
- `vsync=static`：不探测，直接按 `tune.vsync_fps` 滑杆值封顶。
- `vsync=half`：开机实测冠军路最高帧率，取半稳定跑并回写 `measured_vsync_fps`；
  下次启动有记忆直接复用。
- `boot.auto=false`：关闭多卷自动发现，只列本卷 BLS（老行为）。
  打开时：逐卷 BLS（本卷除外，已列）+ 全卷 UKI/Linux/Windows/Shell；
  同卷 BLS/UKI 按 `sort_key`、新版优先排序；`strict=true` 时 BLS 只认
  本卷 + XBOOTLDR 卷。
- `buffers>0` 时 direct 全核 cell 跑混合形态（多核画影子 + BSP 单发 Blt 整推，
  AP 永不碰 GOP；单核路走影子 + CPU 整推；Blt 单发路不变）。
  所选 `blt_op` 若非 `buffer_to_video`，矩阵里单发出数（引擎能力对照，不参选）。
- `accel.level=auto`：探最高可用位宽；显式档位只管上限语义，实际搬运走位宽。
  档位→位宽映射见 `docs/simd.md`（标量/密码类档不加宽，按基线下限同宽处理）。
- 解码 SIMD（hevc/vp9/h263/h264/mpeg2v/theora）无独立开关：单比特跟随全局搬运位宽
  （`level=scalar` 则六家全关，其余开；串口 `accel: decode simd hevc=…` 行
  + `accel: h264 decode isa = …` 行值全跟随全局，供矩阵断言）。ee2ccad 窗口期的 `[accel.decode]` 表直接忽略，
  不断旧文件解析。
- `schedule.backend=manual`：旧三路（blt 单线程引擎 / direct BSP 单核直写 /
  direct 全核 `mp_draw` 常驻 AP + 世代同步），`workers`/`dist` 按直写/全核门控生效。
- `schedule.backend=nano`：单核执行器泵（取帧→上屏→限速/掉帧一帧一调度），
  `render`/影子/vsync 全 honoring，`workers`/`dist` 忽略。
- `schedule.backend=dynamic`：多核 steal 泵（按核分带，BSP 每帧 spawn，AP steal，
  BSP 独判掉帧/OSD，确定性与手写一致）。只直写前台：`render=blt`（Blt 引擎
  单线程）/ 无 MP / 单核回 nano 泵并警告；`tune.buffers>0`（影子环 v1 不支持）
  回 manual 并警告。`[render] workers/dist` 旧键已删除（残留按未知键忽略）。
- `[play] feed=clip`（默认）：开机全量预解码现路（零行为变化）。
  `staged`：qois 显式三段逐帧解码（其余七格式软解码 planar→BGRA 固有分段，
  结构等价 clip）；`stream`：`tune.window`（1-32，默认 4）窗口边播边解，
  游标之后淘汰（`rewind` 重建解码器状态重播）；`zero_copy`：单复用帧缓冲
  逐显示帧即时解码（调用方须立即上传，`blt`/`direct_loop` 与 `nano_pump`
  满足借用契约）；`single`：每帧重读文件 + 重解码，零缓存下限对照（NEW）。
  只切 `Clip` 内部拿帧方式，不管上传路
  （blt/direct/mp/nano/dynamic 零改，只消费 `next_frame`/`rewind`/`len`/`frame`）。
  多核（`mp_draw`）/动态调度/壁纸恒 `clip`/`eager`（壁纸随机访问循环，按 tag 裁决）。
  缺键老文件按 `clip` 照读，现路行为不变。
- `[decode.<fmt>]`（八格式 qois/raw/hevc/avc/vp9/h263/mpeg2v/theora，每表五键，缺表缺键按默认照读）：
  `threads=all` 在 qois/raw 上即时兑现（`honored (frame-parallel safe)` 行），
  在有状态软解码上解析但警告并驻 `bsp_only`
  （`decode: <Fmt> threads all deferred (staying bsp_only)` 行，矩阵断言用）；
  `zero_copy`/`streaming` 布尔全格式可行；`buffers=0` 跟随 `tune.window`（1-32），
  显式 1-32 按格式覆盖，`tune.window` 仍是全局默认；`fetch` 缺省（None）继承全局
  `[play] feed`，显式值只覆盖本格式生效供片（串口 `video: feed=` 取覆盖值 +
  ` fetchovr=<fmt>` 令牌行，矩阵断言子串；壁纸恒 eager 无视覆盖）。
  串口另有 `decode: fmt=<Dbg> threads=...` 解析行（矩阵断言用）。
- 旧键/旧值：`[play] pipeline`、`[render] workers`、`[render] dist` 已删除
  （残留键按 serde 默认忽略；`pipeline` 旧文件按 `clip` 跑）；
  值别名 `avx512`（→`avx512f`）、`on`（→`static`）、整数壁纸 `fps` 已摘除，
  命中即整文件回默认 + 警告（`cfg: parse error` 行），不再静默照读。
- `async.executor`：只决定诊断项 `a` 跑 `nano`/`dynamic` 哪条线（`all` 都跑）；
  缺表缺键按 `all` 照读；生产调度不受影响。
