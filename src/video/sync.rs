//! `video::sync`：AP↔BSP 细粒度借还/互斥/发布原语（无锁协议层，只供 stage-2 接线）。
//!
//! 轮子结论：搜过 `crossbeam`/`parking_lot`/`spin`：全要 `std` 线程/系统 futex，
//! UEFI 皆无；自研放 `src/`（见 `AGENTS.md`“没有才造”）。
//!
//! # 协议图（单槽 4 状态借还，生产者=解码侧，消费者=上传侧）
//! ```text
//!   Empty --try_acquire_fill--> Filling --publish_full--> Full --try_acquire_drain--> Draining --release_empty--> Empty
//!     ^                             |                        |                                |
//!     |                        生产者独写                   消费者独写                        归还
//!     |                        (BSP/AP 解码器)              (上传器/BSP 整推)                  (消费者)
//!     +-- 空闲：谁都可认领 -----------------------------------------------------------------+
//! ```
//! - `Empty`：闲置，生产者 CAS 认领后独写；`Filling`：生产者写入中，消费者不可碰。
//! - `Full`：已发布，消费者 CAS 认领后独读/上传；`Draining`：消费者上传中，生产者不可碰。
//! - 世代号 `generation`：`publish_full` 每次 Release-bump，单调递增永不回绕；
//!   供“丢/重帧”诊断 + `next_full_min_gen` 选最老帧（保序上传）。
//!
//! # 排序纪律（Release-发布 / Acquire-消费）
//! | 方向 | 发布侧（先写数据，后 Release） | 消费侧（先 Acquire 读状态，后读数据） |
//! |---|---|---|
//! | 槽发布 | 缓冲字节写完 → `publish_full`（先 `generation.fetch_add(Release)`，再 `state.store(Full, Release)`） | `try_acquire_drain` CAS（`AcqRel` 成功 / `Acquire` 失败）成功后读缓冲 |
//! | 槽归还 | 上传读完 → `release_empty`（`state.store(Empty, Release)`） | `try_acquire_fill` CAS（同上）成功后写缓冲 |
//! | 配置发布 | `SeqGate::begin_write`（奇数，Release）→ 数据 `Relaxed` 写 → `end_write`（偶数，Release） | `try_read` 双采 `seq`（`Acquire`），奇数/前后不一致即撕裂，重试 |
//! | 锁临界 | `SpinGuard` drop 时 `locked.store(false, Release)` | `try_lock` CAS（`AcqRel`/`Acquire`），临界内读写经 `Deref` 可见 |
//! CAS 失败一律 `Acquire`；程序序 + Release/Acquire 配对即 happens-before，无需 `SeqCst`
//!（旧 `sync_counter`/`done` 的 `SeqCst` 保持现状，本文件不用）。
//!
//! # 看门狗 / 超时铁律（5 分钟固件看门狗，永不无界自旋）
//! - 默认**非阻塞**：`try_acquire_*` 单次 CAS 即返；`next_*` 线性扫即返；`try_read` 单次双采即返。
//! - 唯一可等的是 `lock(budget)` 与 `wait_state_eq`（调用方预算：自旋次数/`tsc` 截止期，
//!   `MpDrive::wait_done` 同款 5 秒熔断）：到界返 `None`/`false`，调用方回退，绝不等死 AP。
//! - `TicketLock` 绝不跨 `GOP`/`Blt`、绝不跨 AP rendezvous 持有（死锁形见该节）。
//!
//! # ROADMAP（stage-2 接线，别人做，本文件只给原语）
//! - stage-2 在 `mp_draw` 里为 qois/raw 建每核解码缓冲（`SlotRing<N>`，`N`≤32）：
//!   每核解码器 `try_acquire_fill` → 写自家槽 → `publish_full`；BSP/上传侧按
//!   `next_full_min_gen` 顺序 `try_acquire_drain` → 整推 → `release_empty`。
//! - 只接帧并行安全格式（qois/raw：帧独立，解码器无跨帧状态）；有状态流
//!   （H.264/H.265/VP9/AVC/Theora 等）恒走 BSP 单核解码，不进本协议。
//! - 行边界配置（如 dynamic 加权边界）走 `SeqGate` 发布，AP 读侧无锁重试。
//!
//! # 与调度后端的接轨（executor-agnostic，零 `uefi-async` 依赖）
//! - 纯 `core`，不引用 `uefi-async` 任何符号（调度器是轮子，见 `AGENTS.md`“轮子优先”——
//!   执行器就是调度轮子，本文件绝不复刻 steal 队列/任务分发）。
//! - manual（`mp_draw` 手写路）：直接用本模块（`SlotRing` 借还替代/增强旧世代号 +
//!   done 计数器；`SeqGate` 替 `bgen` 发布行边界）。
//! - dynamic（`dynamic_draw` steal 泵）：本模块只做数据面（decode 产出槽位、BSP 消费）；
//!   控制面（分发/steal）永留 `uefi-async` 执行器（`DynExec` + stealer 互联），两层各管各的。
//! - nano（`nano_pump` 单核泵）：无跨核共享不适用（协作点是 `Yield`，不是原子协议）。
//!   供以后 `dynamic_draw` 改造参考，本次不接线。
//!
//! # unsafe 政策
//! - 模块体 safe-only：唯一 `unsafe` 是裸指针槽访问（`as_bytes*` + `SlotRing::new`
//!   区片划分），均有 `# Safety` 段，风格同 `copy_rows_cal`（`unsafe fn` 体内显式
//!   `unsafe {}` 块，edition 2024）。零 `uefi`/`uefi-async` 导入（纯 `core`）：
//!   host 可拷贝单文件进测试 crate 跑 `std` 线程锤打。模块永不分配/释放
//!   （`buf` 全归调用方，`new` 只做指针加法区片）。

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

// ---------- tsc（`wait_done` 同款截止期时钟） ----------

/// 时间戳计数（与 `mp_copy::tsc` 同口径；非 x86_64 恒返 0，调用方 `max(1)` 兜底）。
#[inline(always)]
pub fn tsc_now() -> u64 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// 等某槽进入目标状态，`5 * tps` 熔断（`wait_done` 同款；`tps` 取 `calibrate_tps` 实测值）。
pub fn wait_state_eq(slot: &BufferSlot, want: SlotState, tps: u64) -> bool {
    let deadline = tsc_now().saturating_add(5 * tps.max(1));
    while slot.state() != want {
        if tsc_now() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

// ---------- 1. 单槽 4 状态 ----------

/// 槽状态（`repr u8`，存 `AtomicU8`；`mp_copy` 的 `CAL_MODE_*` 同风格小整数协议）。
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotState {
    /// 闲置：生产者可 `try_acquire_fill` 认领。
    Empty = 0,
    /// 生产者解码写入中：消费者不可碰。
    Filling = 1,
    /// 已发布一帧：消费者可 `try_acquire_drain` 认领。
    Full = 2,
    /// 消费者上传中：生产者不可碰。
    Draining = 3,
}

impl SlotState {
    #[inline]
    fn from_u8(v: u8) -> Option<Self> {
        [Self::Empty, Self::Filling, Self::Full, Self::Draining]
            .get(v as usize)
            .copied()
    }
}

/// 单缓冲槽：调用方拥有的内存 + 原子借还协议（`buf`/`len` 一经 `new` 永不改）。
pub struct BufferSlot {
    state: AtomicU8,
    generation: AtomicUsize,
    buf: *mut u8,
    len: usize,
}

// 跨核/AP 共享即本意；SAFETY：`buf` 只在独占期经 `as_bytes*` 访问，无数据竞争。
unsafe impl Send for BufferSlot {}
unsafe impl Sync for BufferSlot {}

impl BufferSlot {
    /// 新槽（初始 `Empty`，世代 0）。`len==0` 时 `buf` 可空（槽永不启用）。
    pub const fn new(buf: *mut u8, len: usize) -> Self {
        Self {
            state: AtomicU8::new(SlotState::Empty as u8),
            generation: AtomicUsize::new(0),
            buf,
            len,
        }
    }

    /// 当前状态（`Acquire`：读到 `Full` 即见缓冲全量；野值只可能来自内存踩踏，
    /// 那是调用方 UB，`debug_assert` 留痕后按 `Empty` 兜底）。
    #[inline]
    pub fn state(&self) -> SlotState {
        let v = self.state.load(Ordering::Acquire);
        debug_assert!(v <= SlotState::Draining as u8, "sync: slot state torn");
        SlotState::from_u8(v).unwrap_or(SlotState::Empty)
    }

    /// 发布世代（`Acquire`；单调递增，诊断/保序用，不作同步栅栏）。
    #[inline]
    pub fn generation(&self) -> usize {
        self.generation.load(Ordering::Acquire)
    }

    /// 生产者认领空槽：`Empty -> Filling` 单次 CAS。成功即独占写权。
    #[inline]
    pub fn try_acquire_fill(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Empty as u8,
                SlotState::Filling as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 生产者发布：缓冲写完后调（`debug_assert` 持有 `Filling`）。
    /// 先 `generation.fetch_add(Release)` 再 `state.store(Full, Release)`——反过来
    /// 消费者可能采到旧世代而把新帧判重（排序 subtlety，见文件头表格）。
    #[inline]
    pub fn publish_full(&self) {
        let s = self.state.load(Ordering::Relaxed);
        debug_assert_eq!(s, SlotState::Filling as u8, "sync: no Filling");
        self.generation.fetch_add(1, Ordering::Release);
        self.state.store(SlotState::Full as u8, Ordering::Release);
    }

    /// 消费者认领满槽：`Full -> Draining` 单次 CAS。成功即独占读权。
    #[inline]
    pub fn try_acquire_drain(&self) -> bool {
        self.state
            .compare_exchange(
                SlotState::Full as u8,
                SlotState::Draining as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 消费者归还：上传读完后调（`debug_assert` 持有 `Draining`，`Release` 存 `Empty`）。
    #[inline]
    pub fn release_empty(&self) {
        let s = self.state.load(Ordering::Relaxed);
        debug_assert_eq!(s, SlotState::Draining as u8, "sync: no Draining");
        self.state.store(SlotState::Empty as u8, Ordering::Release);
    }

    /// # Safety
    /// 调用方须正持有本槽（`Filling` 生产者 / `Draining` 消费者），
    /// 且 `[buf, buf+len)` 持有期内有效、不交叠（`copy_rows_cal` 同风格）。
    #[inline]
    pub unsafe fn as_bytes(&self) -> &[u8] {
        unsafe {
            if self.len == 0 {
                &[]
            } else {
                core::slice::from_raw_parts(self.buf as *const u8, self.len)
            }
        }
    }

    /// # Safety
    /// 同 `as_bytes`，另要求 `&mut` 级独占（`Filling` 生产者写）。
    /// `&self -> &mut` 是协议型内部可变（独占期由状态机保证；共享环下
    /// `&mut self` 签名不可用），故 allow（同 `UnsafeCell` 之理，非绕过）。
    #[allow(clippy::mut_from_ref)]
    #[inline]
    pub unsafe fn as_bytes_mut(&self) -> &mut [u8] {
        unsafe {
            if self.len == 0 {
                &mut []
            } else {
                core::slice::from_raw_parts_mut(self.buf, self.len)
            }
        }
    }
}

// ---------- 2. 固定 N 槽环 ----------

/// 固定槽数环（`N`≤32；线性扫 O(N)；`Box::leak` 常驻，会话期不回收）。
/// `n` 为启用槽数；`n` 之外的槽空（`buf` 空/`len` 0），永不扫描。
pub struct SlotRing<const N: usize> {
    slots: [BufferSlot; N],
    n: usize,
}

// 同上，槽协议保证独占期无竞争。
unsafe impl<const N: usize> Send for SlotRing<N> {}
unsafe impl<const N: usize> Sync for SlotRing<N> {}

impl<const N: usize> SlotRing<N> {
    /// 把调用方连续内存切成 `n` 个等长槽（不分配，只做指针加法）。
    ///
    /// # Safety
    /// `[buf_ptr, buf_ptr + n*slot_len)` 有效、不交叠、环期内保持有效；
    /// `n==0` 时 `buf_ptr` 可空（`next_*` 恒返 `None`）。
    pub unsafe fn new(buf_ptr: *mut u8, slot_len: usize, n: usize) -> Self {
        debug_assert!(N <= 32, "sync: SlotRing N>32");
        debug_assert!(n <= N, "sync: SlotRing n>N");
        unsafe {
            Self {
                slots: core::array::from_fn(|i| {
                    if i < n && slot_len > 0 {
                        BufferSlot::new(buf_ptr.add(i * slot_len), slot_len)
                    } else {
                        BufferSlot::new(core::ptr::null_mut(), 0)
                    }
                }),
                n: n.min(N),
            }
        }
    }

    /// 启用槽数（`get`/`next_*` 只扫 `[0, n)`；不叫 `len` 以避 `len_without_is_empty`）。
    #[inline]
    pub fn num_slots(&self) -> usize {
        self.n
    }

    /// 取槽（只借 `&`，所有权仍归环）。
    #[inline]
    pub fn get(&self, idx: usize) -> Option<&BufferSlot> {
        (idx < self.n).then(|| &self.slots[idx])
    }

    /// 找空槽下标（候选；紧接 CAS，失败被抢先则重扫——check-then-act，非原子认领）。
    pub fn next_empty(&self) -> Option<usize> {
        (0..self.n).find(|&i| self.slots[i].state() == SlotState::Empty)
    }

    /// 找世代最小的满槽下标（最老一帧，保序上传；同样先候选后 CAS，失败重扫）。
    pub fn next_full_min_gen(&self) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for i in 0..self.n {
            if self.slots[i].state() == SlotState::Full {
                let g = self.slots[i].generation();
                if best.is_none_or(|(_, bg)| g < bg) {
                    best = Some((i, g));
                }
            }
        }
        best.map(|(i, _)| i)
    }
}

// ---------- 3. 短临界自旋锁 ----------

/// 短临界自旋锁（`T` 为被护数据；`locked: AtomicBool` test-and-set）。
///
/// 死锁形（禁区）：永不跨 `GOP`/`Blt` 持有（Blt 非线程安全可阻塞，
/// AP 临界内等 Blt 而 BSP 等锁即互等死锁）；永不跨 AP rendezvous 持有
/// （`wait_done`/`wait_state_eq`/计数器栅栏：持有者等 AP、AP 等锁，
/// 环形等待，熔断只放等待者不放锁）。只许数十条指令的临界
/// （改计数器/换指针/bump 世代），临界内无循环、无固件调用。
pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// `UnsafeCell` 本 `!Sync`；锁协议保证临界互斥，故 `T: Send` 即可跨核共享。
// SAFETY：`Deref`/`DerefMut` 只经 `SpinGuard`（持有 `locked`）发放。
unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

/// `SpinLock` 持有守卫：drop 时 `Release` 放锁（`locked.store(false)`）。
pub struct SpinGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY：guard 存续期即临界区，锁协议保证独占。
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY：同上。
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

impl<T> SpinLock<T> {
    /// 新锁（初始未锁定；`const`，可放 `static`）。
    pub const fn new(v: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(v),
        }
    }

    /// 试拿（单次 CAS 即返；`false->true` 成功即进临界）。
    pub fn try_lock(&self) -> Option<SpinGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| SpinGuard { lock: self })
    }

    /// 有界拿锁：`budget` 次自旋 + 收尾再试（`budget==0` 即 `try_lock`）；到界 `None`，永不无界等。
    pub fn lock(&self, budget: u32) -> Option<SpinGuard<'_, T>> {
        for _ in 0..budget {
            if let Some(g) = self.try_lock() {
                return Some(g);
            }
            core::hint::spin_loop();
        }
        self.try_lock()
    }
}

/// `TicketLock` 即 `SpinLock`（别名）：临界恒短且持有者永不阻塞等待，
/// 票号公平排队不需要（省掉票号/轮号回绕论证面）。
pub type TicketLock<T> = SpinLock<T>;

// ---------- 4. 单写多读 seqlock ----------

/// 单写多读 seqlock（小配置数组发布，如 dynamic 加权行边界 `n+1` 项）。
/// `seq` 偶数=静止、奇数=写中；BSP 独写，AP 只读（`try_read` 双采，撕裂重试）。
/// AP 读者永不阻塞写者（无锁无 CAS）；读到旧边界只影响本轮负载（性能），
/// 不影响正确性（同世代边界成对发布，读者要么整旧要么整新，永不半新半旧）。
pub struct SeqGate<const N: usize> {
    seq: AtomicUsize,
    slots: [AtomicUsize; N],
}

// 全原子成员，自动 `Send + Sync`，无需手动 impl。
impl<const N: usize> SeqGate<N> {
    /// 新门（`seq=0` 偶数静止，槽全 0；槽数组 inline-const 手写，零依赖，见下 `Default` 注）。
    pub const fn new() -> Self {
        Self {
            seq: AtomicUsize::new(0),
            slots: [const { AtomicUsize::new(0) }; N],
        }
    }

    /// 开始写（BSP 独写；`seq` 奇数化，`Release`，此后 AP 的 `try_read` 全败重试）。
    #[inline]
    pub fn begin_write(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        let s = self.seq.load(Ordering::Relaxed) & 1;
        debug_assert_eq!(s, 1, "sync: begin unpaired");
    }

    /// 结束写（`seq` 偶数化，`Release` 发布；与 `begin_write` 严格配对）。
    #[inline]
    pub fn end_write(&self) {
        let s = self.seq.load(Ordering::Relaxed) & 1;
        debug_assert_eq!(s, 1, "sync: end without begin");
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// 发布整组（单写者假设：并发调即竞争，调用方保证 BSP 独写）。
    #[inline]
    pub fn publish(&self, values: &[usize; N]) {
        self.begin_write();
        for (s, v) in self.slots.iter().zip(values.iter()) {
            s.store(*v, Ordering::Relaxed);
        }
        self.end_write();
    }

    /// 快照读：偶数开局 → 拷槽 → 复采一致即成功。撕裂返 `false`，
    /// 调用方 `spin_loop` + 重试（上限调用方定，建议 `tps` 熔断，永不无界重试）。
    pub fn try_read(&self, out: &mut [usize; N]) -> bool {
        let s0 = self.seq.load(Ordering::Acquire);
        if s0 & 1 == 1 {
            return false;
        }
        for (o, s) in out.iter_mut().zip(self.slots.iter()) {
            *o = s.load(Ordering::Relaxed);
        }
        let s1 = self.seq.load(Ordering::Acquire);
        s0 == s1
    }
}

// 手写 `Default`（调 `new()`；`derive` 对 `[AtomicUsize; N]` 不可用，见上）。
impl<const N: usize> Default for SeqGate<N> {
    fn default() -> Self {
        Self::new()
    }
}
