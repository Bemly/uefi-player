# rusty_h265-accel —— submodule + patch pin（2026-10-21 起冻结，不跟上游新版）

- 上游：https://github.com/Remade-With-Rust/rusty_h265 @ tag rusty_h265-v0.6.2（873db87，与 decoder 同 commit）
- 本仓形态：与 decoder 共用 submodule（`third_party/rusty_h265`，frozen）+ `patches/rusty_h265-accel.patch`
- 应用：`git -C third_party/rusty_h265 apply ../../patches/rusty_h265-accel.patch`
- 移植说明见 `patches/rusty_h265.VENDOR.md`（两箱同一份 VENDOR，标题即“(+ rusty_h265-accel)”）。
