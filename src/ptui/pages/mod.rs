//! ptui 子页聚合（外壳 `mod.rs` 只需调 `home::run` 一处；各页值进值出 `Screen`）。
//!
//! - `home`：九宫导航（播放/加速/渲染/调度/解码/显示/测试/壁纸 + 返回），
//!   数字键嵌套进子页，`q` 回外壳。
//! - 其余各页：自带按键循环，`q` 回首页调用方；调档只改 runtime 内存
//!   （壁纸页显式保存除外），每次调档双写 `ptui: set <key>=<value>`。

pub mod accel;
pub mod decode;
pub mod display;
pub mod home;
pub mod play;
pub mod render;
pub mod schedule;
pub mod tests;
pub mod wallpaper;
