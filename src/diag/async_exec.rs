//! uefi-async 自举验证：nano + dynamic 两条堆线在真机/UEFI 下跑通。
//!
//! 不碰播放/bench 热路径，只证明轮子在固件里能转：`join!`/`Timeout`/
//! `WaitTimer`/`Yield`/`Skip`/`Pacer` 同词汇 + `spawn/cancel/set_freq`
//! 跨核管控。串口断言行供 QEMU 矩阵 `async-exec` case 用。

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};

use uefi_async::Pacer;
use uefi_async::Skip;
use uefi_async::Yield;
use uefi_async::common::control::single::join::join;
use uefi_async::common::time::{_Timeout, Timeout, WaitTimer};
use uefi_async::dynamic::Executor as DynExec;
use uefi_async::nano_alloc::Executor as NanoExec;
use uefi_async::nano_alloc::TaskNode as NanoNode;
use uefi_async::tick;

use crate::emit;
use crate::error::{NyaStatus, Result};
use crate::graphics::Screen;

fn fail(msg: &str) -> NyaStatus {
    NyaStatus::_Debug(alloc::string::String::from(msg))
}

/// nano 线：两个借用节点 + `join!` + 超时/定时/起搏，全在 200 步内跑完。
fn nano_smoke() -> core::result::Result<(), NyaStatus> {
    let flag = Rc::new(RefCell::new(0u32));
    let c1 = Rc::clone(&flag);
    let c2 = Rc::clone(&flag);
    let c3 = Rc::clone(&flag);

    let t1 = async move {
        *c1.borrow_mut() += 1;
    };
    let t2 = async move {
        let a = async {
            *c2.borrow_mut() += 10;
        };
        let b = async {
            *c3.borrow_mut() += 100;
        };
        join!(a, b).await;
    };
    // 控制原语行：足额 Timeout 透传；过期 `.timeout(0)` 直接 Err（不 poll 内层）；
    // 0ms 定时即就绪；Yield/Skip/Pacer 各让出一轮（协作式，不丢进度）。
    // 失败只记槽（固件 `panic!` 会进 stall，不适合诊断项）。
    let err: Rc<RefCell<Option<&'static str>>> = Rc::new(RefCell::new(None));
    let e1 = Rc::clone(&err);
    let ctl = async move {
        match Timeout::new(async { 7u32 }, 1_000_000_000).await {
            Ok(7) => {}
            _ => *e1.borrow_mut() = Some("timeout passthrough"),
        }
        match async { 1u32 }.timeout(0).await {
            Err(()) => {}
            _ => *e1.borrow_mut() = Some("expired timeout"),
        }
        WaitTimer::from_ms(0).await;
        Yield.await;
        Skip(1).await;
        let mut pacer = Pacer::new(0);
        pacer.repeat().await;
    };

    let mut exec = NanoExec::new();
    let mut n1 = NanoNode::new(Box::pin(t1), 0);
    let mut n2 = NanoNode::new(Box::pin(t2), 0);
    let mut n3 = NanoNode::new(Box::pin(ctl), 0);
    exec.add(&mut n1).add(&mut n2).add(&mut n3);
    let mut cx = NanoExec::init_step();
    for _ in 0..200 {
        exec.run_step(tick(), &mut cx);
        if *flag.borrow() == 111 {
            break;
        }
    }
    if *flag.borrow() != 111 {
        return Err(fail("nano tasks did not complete"));
    }
    if let Some(what) = *err.borrow() {
        return Err(fail(what));
    }
    Ok(())
}

static DYN_RAN: AtomicU32 = AtomicU32::new(0);
static DYN_DROPPED_RAN: AtomicU32 = AtomicU32::new(0);

/// dynamic 线：spawn 跑一次即回收；先 cancel 的永不跑；未知 id 安全 false；
/// `set_freq` 热调即时生效。
fn dynamic_smoke() -> core::result::Result<(), NyaStatus> {
    DYN_RAN.store(0, Ordering::Relaxed);
    DYN_DROPPED_RAN.store(0, Ordering::Relaxed);
    let exec = DynExec::new();
    let id = exec
        .spawn(
            Box::pin(async {
                DYN_RAN.fetch_add(1, Ordering::Relaxed);
            }),
            0,
        )
        .ok_or(fail("dyn spawn full"))?;
    if !exec.is_alive(id) {
        return Err(fail("dyn spawned task not alive"));
    }
    let doomed = exec
        .spawn(
            Box::pin(async {
                DYN_DROPPED_RAN.fetch_add(1, Ordering::Relaxed);
            }),
            0,
        )
        .ok_or(fail("dyn spawn2 full"))?;
    if !exec.cancel(doomed) {
        return Err(fail("dyn cancel refused"));
    }
    // 未知 id 永不碰内存，只回 false（注册表纯数据槽位，见上游设计）。
    if exec.cancel(999_999) {
        return Err(fail("dyn cancel unknown returned true"));
    }
    if !exec.set_freq(id, 30) {
        return Err(fail("dyn set_freq refused"));
    }
    let mut cx = DynExec::init_step();
    for _ in 0..200 {
        exec.run_step(tick(), &mut cx);
        if DYN_RAN.load(Ordering::Relaxed) == 1 {
            break;
        }
    }
    if DYN_RAN.load(Ordering::Relaxed) != 1 {
        return Err(fail("dyn task did not run"));
    }
    if DYN_DROPPED_RAN.load(Ordering::Relaxed) != 0 {
        return Err(fail("dyn cancelled task ran"));
    }
    if exec.is_alive(id) {
        return Err(fail("dyn finished task still alive"));
    }
    Ok(())
}

pub fn run(screen: &mut Screen) -> Result {
    emit!(screen, "=== uefi-async smoke (nano + dynamic) ===");
    nano_smoke()?;
    emit!(screen, "async-exec: nano ok (join/timeout/timer/pacer)");
    dynamic_smoke()?;
    emit!(screen, "async-exec: dynamic ok (spawn/cancel/retune)");
    emit!(screen, "async-exec: PASS");
    Ok(())
}
