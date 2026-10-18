# 播放器（P3）

## `video.qois` 格式

`1080p/video.qois`：`u32le 帧长 + QOI 帧` 串接，无帧率元数据。
生产假设 120Hz 视频（`VIDEO_NATIVE_FPS = 120`，`auto` 上限按它封顶）。

## 管线（`VideoMemoryRaw`）

全量预解码 → RGBA→BGRA → 按生效路径上屏：

- `RenderMode::Blt` → `blt_loop`（单线程 GOP Blt）。
- `direct + bsp_only` → `direct_loop`（BSP 单核直写）。
- `direct + all` → `mp_draw`（`copy_block` 位宽选路，OSD 镂空）。
  分工由 `[render] dist` 定：`static` 按核固定行区（同构核最优）；
  `tile` 按 64 行切块、BSP+AP 从共享原子游标领活（异构核/负载不均时赢，
  OSD 行划出 tile，AP 不再盖字——旧 static 路 AP 会盖掉 OSD，属已知瑕疵，
  保留作扩展比对照）。

## 格式校验 + 掉帧（P3 闭环项）

- 预解码：坏帧计数跳过；空集直接报错（曾无限 rewind 打空转）；
  几何不一致/超大帧拒绝（混入异形帧会撕裂后续定位）。
- `mp_draw` 同：坏帧计数 + 空集报错（曾 AP 读空指针 OOB + 无限循环）。
- `VideoMemory::next_frame` 越界守卫（尾部不足直接结束，曾直切 panic）。
- 掉帧：单线程 `pace_or_drop`（迟超一整帧丢帧追赶，最多连丢 2 帧，串口记数）；
  多核确定性掉帧（BSP 判迟独写发布、全核执行同一结论，索引永不发散；
  OSD 追加 `Drop:` 计数）。
- 首启实测 `measure_max_fps`：单线程 `upload_frame` 整轮推送，是保守下限
  （workers=all 真实更快）；`auto` 取 `min(视频帧率, 实测*3/4)` 并回写。

## 帧率开关（`[play]`）

`max` 不限速；`capped(1-240)` 直接用；`auto` 无实测现测并回写
（`measured_max_fps`，GUI 只读 + 重测清零）。`vsync=on` 再叠 60fps 帽。

## 真机错位问题（曾 TODO，现状态）

- 现象：真机概率性 QOI 解码失败/数据错位，QEMU 不现。
- 现措施：失败计数 + 阈值报错（单文件全坏/连续坏不再静默），串口留数；
  播放循环不再因坏帧卡死或空转。
- 未根治：错位根因（固件 DMA/缓存一致性？文件系统短读？）仍待真机抓包；
  复现时取 `HWBENCH` + 串口 `skipped` 计数一起归档。
