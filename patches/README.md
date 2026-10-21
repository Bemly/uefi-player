# patches/ —— submodule 叠加层（frozen，不跟上游新版）

本仓 `third_party/` 下 7 个 submodule 全部 pin 死（见根 `AGENTS.md`），
逻辑改动以 patch 形式叠在上游 checkout 之上。sprout 那个是样子，其余 6 家同理。

## 新 clone 后跑一次

```bash
git submodule update --init
git -C third_party/sprout apply ../../patches/sprout-boot-lib.patch
git -C third_party/oxideav-theora apply ../../patches/oxideav-theora.patch
git -C third_party/vp9dec apply ../../patches/vp9dec.patch
git -C third_party/rusty_h265 apply ../../patches/rusty_h265.patch
git -C third_party/rusty_h265 apply ../../patches/rusty_h265-accel.patch
git -C third_party/rusty_h264 apply ../../patches/rusty_h264-accel.patch
git -C third_party/filmcraft apply ../../patches/filmcraft-mpeg2v.patch
git -C third_party/h263-rs apply ../../patches/h263-rs.patch
```

## 清单

| submodule | 上游 base（frozen） | patch（功能 delta） | 说明 |
|---|---|---|---|
| `third_party/sprout` | pin commit（见 gitlink） | `sprout-boot-lib.patch`（72 行，加 `[lib]`） | 只取 5 个 lib |
| `third_party/oxideav-theora` | tag `v0.0.12` | `oxideav-theora.patch` + `.VENDOR.md` | theora 软解 no_std port |
| `third_party/vp9dec` | tag `v0.1.1` | `vp9dec.patch` + `.VENDOR.md` | vp9 软解 no_std port |
| `third_party/rusty_h265` | tag `rusty_h265-v0.6.2` | `rusty_h265.patch` / `rusty_h265-accel.patch` + `.VENDOR.md` | h265 双箱，一个 submodule 出两个 patch |
| `third_party/rusty_h264` | tag `v0.16.1` | `rusty_h264-accel.patch` + `.VENDOR.md` | 经 `[patch.crates-io]` 截流 |
| `third_party/filmcraft` | commit `70f0fc7a`（= 上游 HEAD） | `filmcraft-mpeg2v.patch` + `.VENDOR.md` | 只用 `crates/mpeg2v` |
| `third_party/h263-rs` | commit `27ee833`（= 上游 HEAD） | `h263-rs.patch` + `.VENDOR.md` | 3 箱同仓 |

说明：

- patch 只收功能 delta（`src/**` + `Cargo.toml`）；上游自带的文档/用例/fuzz/
  CI 保留不动。apply 后各 submodule 工作树是脏的（同 sprout 现状），
  脏的内容 ≡ patch 内容，不许手改 submodule 里任何东西，要改就改 patch 重打。
- `*.VENDOR.md` 是原 vendor 树的移植说明，原样搬家（头加了 base 认领），
  注释里的 'VENDOR.md' 指的就是它；`README.upstream.md` 不再需要
  （submodule 里有原版）。
- 冻结政策：不跟上游新版。port 语义变了才动 patch，动完重跑双矩阵门禁。

## 改 submodule 里的代码后重打 patch

用 `tools/patch-sync`（纯 Rust，零新依赖），不要手写 `git diff`：

```bash
T="$(rustc -vV | sed -n 's/^host: //p')"
cargo run --manifest-path tools/patch-sync/Cargo.toml --target "$T" -- [--check] [--only <name>]
```

- 默认：按改动重写 `patches/*.patch`（`git diff` 口径，`git apply` 通吃，
  重打是字节稳定的——同样改动反复打，文件逐字节相同）。
- `--check`：只比对不写盘（fresh-clone 模拟：纯净 base + 已提交 patch ⟺
  当前工作树），有漂移即报错退出。
- `--only <name>`：只处理一家（如 `rusty_h265-accel`）。

fail-closed 三条（见 `tools/patch-sync/src/main.rs` 头注释）：

1. `third_party/rusty_h265` 一仓两 patch，按路径自动劈开；
2. 新增文件自动 `git add -N`（`git diff` 默认吞掉它们，不加会静默丢 patch）；
3. 作用域（`src/**` + `Cargo.toml`，h263 是三箱同款）之外的脏文件直接报错，
   想收就改工具里的 scope 表——examples/CI/文档的随手改混不进 firmware patch。
