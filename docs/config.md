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
vsync   = "auto"        # auto | on | off（软件 60fps 帧预算）
blt_op  = "buffer_to_video"  # buffer_to_video | video_to_video | video_fill | video_to_blt_buffer
# 首启校准记忆（固件回写，GUI 只读 + 重校准清零；"auto"/0 = 未测，下次启动必测）：
measured_render  = "auto"  # direct | blt
measured_workers = "auto"  # bsp_only | all
measured_vsync   = "auto"  # on | off
measured_cores   = 0       # 校准时机型 AP+BSP 总数

[render.tune]
frames = 30             # 每次矩阵/微基准的帧数（>=1）

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
- `workers=auto`：直写路 BSP vs 全核取快；Blt 路径忽略本项。
- `vsync=auto`：冠军路径 60fps 零超期则开，否则关；开 = 播放叠 60fps 帽。
- `accel.level=auto`：探最高可用位宽；显式档位只管上限语义，实际搬运走位宽。
