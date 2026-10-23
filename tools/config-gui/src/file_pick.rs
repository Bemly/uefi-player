//! rfd 文件对话框异步化（winit macOS 不允许在事件处理内再进事件循环：
//! UI 线程内同步 `pick_file` + 模态面板 pumping 会触发
//! `tried to handle event while another event is currently being handled` → abort。
//! 血泪：转码页“选源”一点即崩（backtrace 定点 `rfd ... run_modal`）。
//! 故对话框一律起后台线程跑阻塞版，UI 侧每帧 `try_recv` 收结果。零新依赖
//! （`try_recv` 轮询与转码 job 同风格，见 `transcode.rs`）。
//!
//! 用法：点击时 `open(|| ...pick_file())`（闭包须 `'static + Send`，
//! 参数先搬进 owned 再 move）；每帧 `take()` 一次，`Some(path)` 落袋，
//! `Some(None)` = 用户取消（调用方无视），`None` = 还没选好。

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// 单个文件对话框会话（打开/保存/加列表各持一个，互不抢 channel）。
pub(crate) struct FilePick {
    rx: Option<Receiver<Option<PathBuf>>>,
}

impl FilePick {
    pub(crate) fn new() -> Self {
        Self { rx: None }
    }

    /// 起对话框（后台线程阻塞跑 `make`，结果回 channel；重复点击会替换旧会话，
    /// 旧框选了也落空——调用方可另用 `pending()` 把按钮置灰）。
    pub(crate) fn open(&mut self, make: impl FnOnce() -> Option<PathBuf> + Send + 'static) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(make());
        });
    }

    /// 收结果（每帧调一次）：`Some(path)` 落袋 / `Some(None)` 用户取消 /
    /// `None` 还没选好（继续等）。
    pub(crate) fn take(&mut self) -> Option<Option<PathBuf>> {
        let rx = self.rx.take()?;
        match rx.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty) => {
                self.rx = Some(rx);
                None
            }
            // worker 没了（panic 会被上层接住，此处按取消处理，不断 UI）。
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }

    /// 对话框是否还开着（调用方可据此置灰按钮，防叠框）。
    pub(crate) fn pending(&self) -> bool {
        self.rx.is_some()
    }
}

#[cfg(test)]
mod file_pick_tests {
    use super::*;

    #[test]
    fn empty_take_is_none_and_not_pending() {
        let mut p = FilePick::new();
        assert!(!p.pending());
        assert_eq!(p.take(), None);
        assert!(!p.pending());
    }

    #[test]
    fn open_marks_pending_take_collects_result() {
        let mut p = FilePick::new();
        // 立即返回的闭包：结果迟早进 channel；先断言会话已建立（同步部分），
        // 再轮询收结果（线程调度不定时，最多等 5s，超时即失败）。
        p.open(|| Some(PathBuf::from("/tmp/x.toml")));
        assert!(p.pending());
        let mut got = None;
        for _ in 0..50 {
            if let Some(v) = p.take() {
                got = Some(v);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(got, Some(Some(PathBuf::from("/tmp/x.toml"))));
        assert!(!p.pending());
    }
}
