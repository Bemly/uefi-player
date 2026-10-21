# player.toml 完整 reference

> 位置：启动卷根目录（与 `HWBENCH*.TXT` 同层）。解析：`toml` 1.x（no_std）+ serde。
> 缺失/非法 → 全默认 + 屏显一行警告，不 halt。`[wallpaper]` 缺表同样按默认处理
> （GUI 回归测试覆盖旧文件照读）。与 `tools/config-gui` 同 schema（双边同步改）。

```toml
version = 1

[accel]
level = "auto"          # auto | scalar | sse | sse2 | sse3 | ssse3 | sse41 | sse42
                        # | popcnt | aes | pclmulqdq | avx | f16c | fma | avx2
                        # | bmi1 | bmi2 | lzcnt | movbe | rdrand | rdseed | sha | gfni
                        # | avx512f | avx512cd | avx512dq | avx512bw | avx512vl | avxvnni
                        # | tbm | sse4a（AMD 专有，扩展叶 8000_0001H ECX bit 21/6；
                        #   qemu64 无此叶，显式配了即按 X128 钳住用，不断 boot）
                        # auto = 开机探最高可用；显式超硬件警告+钳住；
                        # 老 "avx512" 按 avx512f 照读
                        # 解码 SIMD 无独立开关：四家（hevc/vp9/h263/h264）单比特跟随全局
                        #（level=scalar 则关，其余开），见 docs/simd.md

[render]
mode    = "auto"        # auto | direct | blt
vsync   = "auto"        # auto | static | half | off（软件帧预算；旧 "on" 按 static 照读）
blt_op  = "buffer_to_video"  # buffer_to_video | video_to_video | video_fill | video_to_blt_buffer
# 只有 buffer_to_video 能上屏播视频（单发整推 + 混合整推都用它；GUI 只放行这一项，
# 其余置灰展示不删）；其余三项只在矩阵里单发出数（引擎能力对照，不参选），Blt 不在测时跳过。
# workers/dist 已搬家到 [schedule]（下）；残留旧键永久兼容（缺 [schedule] 表时照读，
# [schedule] 在场则忽略；GUI 存盘即新格式）。
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

[play]
fps = "auto"            # auto | max | 1..240
                        # auto: 首启实测最大帧率并回写，按 min(视频帧率, 实测*3/4) 跑
format = "qois"         # 片源格式：qois（1080p/video.qois）| raw（1080p/frameNNNN.raw）
pipeline = "clip"       # 播放管线：clip（预解码现路，默认）| staged（流式三段）
                        # | zero_copy（流式零拷贝）| stream（真文件流式）| single（单帧反复）
                        # | legacy_blt / legacy_direct / legacy_u64（Clip + 老上传循环）
                        # 非 clip 只读 qois（raw 时 warn 回落 clip）；老路无 OSD/多核/影子。
measured_max_fps = 0    # 固件实测回写（0 = 未测）；GUI 只读展示

[schedule]
backend = "manual"      # 调度后端：manual（手写 MP 三路，workers/dist 生效）
                        # | nano（单核执行器泵走渲染路径，workers/dist 忽略）
                        # | dynamic（多核 steal 泵，只直写前台；见下回退）
workers = "auto"        # manual 后端多核开关：auto | bsp_only | all（只对 direct 有效）
dist = "auto"           # manual 后端分工：auto | static | tile | dynamic（只对 direct+all 有效）

[wallpaper]
enable = false          # 菜单背景 + 引导过渡总开关
format = "raw"          # 片源格式：raw（wallpaper/frameNNNN.raw，默认）| qois（wallpaper/wall.qois，开机预解码）
popup_freeze = true     # 选项弹窗时冻结壁纸（false 则行区继续播；GUI 有开关）
fps = "15"               # 背景帧率三态（与 [play] fps 同构）：auto（最左，跟 vsync；
                          # vsync 关则不限速）| max（最右，不限速）| 1-240（中间显式封顶）
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
- 解码 SIMD（hevc/vp9/h263/h264）无独立开关：单比特跟随全局搬运位宽
  （`level=scalar` 则四家全关，其余开；串口 `accel: decode simd hevc=…` 行
  + `accel: h264 decode isa = …` 行值全跟随全局，供矩阵断言）。ee2ccad 窗口期的 `[accel.decode]` 表直接忽略，
  不断旧文件解析。
- `schedule.backend=manual`：旧三路（blt 单线程引擎 / direct BSP 单核直写 /
  direct 全核 `mp_draw` 常驻 AP + 世代同步），`workers`/`dist` 按直写/全核门控生效。
- `schedule.backend=nano`：单核执行器泵（取帧→上屏→限速/掉帧一帧一调度），
  `render`/影子/vsync 全 honoring，`workers`/`dist` 忽略。
- `schedule.backend=dynamic`：多核 steal 泵（按核分带，BSP 每帧 spawn，AP steal，
  BSP 独判掉帧/OSD，确定性与手写一致）。只直写前台：`render=blt`（Blt 引擎
  单线程）/ 无 MP / 单核回 nano 泵并警告；`tune.buffers>0`（影子环 v1 不支持）
  回 manual 并警告。缺 `[schedule]` 表的老文件继续认 `[render]` 旧键。
- `play.pipeline=clip`（默认）：现行管线（`schedule.backend` 三后端生效）。
  非 clip 走修复后的老实现循环（`render`/`schedule`/`workers`/`dist` 全忽略，
  无 OSD/多核/影子，只读 qois，raw 回落 clip 并警告）：
  `staged`（VideoMemory 整文件 + 三段解码 + Blt + 限速掉帧）/
  `zero_copy`（直解进 Blt 缓冲 + R/B 交换 + 限速掉帧）/
  `stream`（真文件流式边读边解 + 限速不限丢）/
  `single`（每帧重读文件 + 单帧解码，仅对照）/
  `legacy_blt`/`legacy_direct`/`legacy_u64`（Clip 预解码 + 老上传循环）。
  缺键老文件按 `clip` 照读，现路行为不变。
- `async.executor`：只决定诊断项 `a` 跑 `nano`/`dynamic` 哪条线（`all` 都跑）；
  缺表缺键按 `all` 照读；生产调度不受影响。
