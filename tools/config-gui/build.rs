//! 构建脚本只做一件事：告诉 cargo 文案改了也要重编。
//!
//! 背景（血泪）：`rust-i18n` 编译期把 `locales/*.yml` 烘进二进制，但它的构建
//! 脚本没声明对该目录的跟踪，只改 yml 不碰 rs 时 `cargo run` 会直接起旧二进制
//! （`Finished in Xs`，一行 Compiling 都没有），UI 看起来"改了没生效"。
fn main() {
    println!("cargo:rerun-if-changed=locales/");
    println!("cargo:rerun-if-changed=assets/");
}
