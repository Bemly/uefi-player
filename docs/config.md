# player.toml 完整 reference

> 位置：启动卷根目录（与 `HWBENCH*.TXT` 同层）。解析：`toml` 1.x（no_std）+ serde。
> 缺失/非法 → 全默认 + 屏显一行警告，不 halt。`[wallpaper]` 缺表同样按默认处理
> （GUI 回归测试覆盖旧文件照读）。与 `tools/config-gui` 同 schema（双边同步改）。

```toml
version = 1

[accel]
level = "auto"          # auto | scalar | sse2 | ssse3 | sse41 | sse42 | avx | avx2 | avx512
                        # auto = 开机探最高可用；显式超硬件警告+钳住

[render]
mode    = "auto"        # auto | direct | blt
workers = "auto"        # auto | bsp_only | all（只对 direct 有效；Blt 恒单线程）
dist    = "auto"        # auto | static | tile | dynamic（多核分工；只对 direct+all 有效）
vsync   = "auto"        # auto | static | half | off（软件帧预算；旧 "on" 按 static 照读）
blt_op  = "buffer_to_video"  # buffer_to_video | video_to_video | video_fill | video_to_blt_buffer
# 只有 buffer_to_video 能上屏播视频（单发整推 + 混合整推都用它；GUI 只放行这一项，
# 其余置灰展示不删）；其余三项只在矩阵里单发出数（引擎能力对照，不参选），Blt 不在测时跳过。
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
measured_max_fps = 0    # 固件实测回写（0 = 未测）；GUI 只读展示

[wallpaper]
enable = false          # 菜单背景 + 引导过渡总开关
fps = 15                # 背景循环帧率（1-60，已钳制）
frames = 0              # B 过渡封顶帧数（0 = 全播预载集）
preload = 8             # 预载帧数上限（1-32；1080p BGRA 单帧约 8MB）
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
- `buffers>0` 时 direct 全核 cell 跑混合形态（多核画影子 + BSP 单发 Blt 整推，
  AP 永不碰 GOP；单核路走影子 + CPU 整推；Blt 单发路不变）。
  所选 `blt_op` 若非 `buffer_to_video`，矩阵里单发出数（引擎能力对照，不参选）。
- `accel.level=auto`：探最高可用位宽；显式档位只管上限语义，实际搬运走位宽。
