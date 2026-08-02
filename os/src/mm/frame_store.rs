//! VMA 内部的页帧状态表。
//!
//! `VmPageStore` 记录一个 VMA 范围内哪些虚拟页已经驻留、未分配、压缩或换出。
//! 它只维护元数据和 `FrameTracker`/swap/zram tracker 的所有权，不直接修改用户页表。
//!
//! # OOM
//!
//! 启用 `oom_handler` 时，active 队列和 compressed/swapped 计数用于浅回收、深回收以及
//! procfs 统计。范围拆分或收缩后必须重算这些计数，避免后续回收访问越界 VPN。

use core::fmt::Debug;

#[cfg(feature = "zram")]
use super::zram::{ZramTracker, ZRAM_DEVICE};
use super::{frame_alloc, FrameTracker, MemoryError, PhysPageNum, VPNRange, VirtPageNum};
#[cfg(feature = "swap")]
use crate::fs::swap::{SwapTracker, SWAP_DEVICE};
use alloc::collections::BTreeMap;
#[cfg(feature = "oom_handler")]
use alloc::collections::VecDeque;
use alloc::sync::Arc;

#[cfg(feature = "oom_handler")]
#[derive(Clone, Debug)]
pub enum Frame {
    InMemory(Arc<FrameTracker>),
    Compressed(Arc<ZramTracker>),
    SwappedOut(Arc<SwapTracker>),
    Unallocated,
}

#[cfg(not(feature = "oom_handler"))]
#[derive(Clone, Debug)]
pub enum Frame {
    InMemory(Arc<FrameTracker>),
    Unallocated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FrameState {
    InMemory,
    Unallocated,
    #[cfg(feature = "oom_handler")]
    Compressed,
    #[cfg(feature = "oom_handler")]
    SwappedOut,
}

impl Frame {
    /// 写入 swap 并返回原驻留 frame。调用方必须先清除 PTE、完成 TLB 提交，
    /// 才能释放这个 tracker。
    #[cfg(feature = "oom_handler")]
    pub fn swap_out(&mut self) -> Result<Arc<FrameTracker>, MemoryError> {
        match self {
            Frame::InMemory(frame_ref) => {
                if Arc::strong_count(frame_ref) == 1 {
                    let swap_tracker = SWAP_DEVICE.lock().write(frame_ref.ppn.get_bytes_array())?;
                    let old = core::mem::replace(self, Frame::SwappedOut(swap_tracker));
                    let Frame::InMemory(frame) = old else {
                        unreachable!("swap_out source changed while exclusively borrowed")
                    };
                    Ok(frame)
                } else {
                    Err(MemoryError::SharedPage)
                }
            }
            _ => Err(MemoryError::NotInMemory),
        }
    }

    #[cfg(feature = "oom_handler")]
    pub fn swap_in(&mut self) -> Result<PhysPageNum, MemoryError> {
        match self {
            Frame::SwappedOut(swap_tracker) => {
                let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
                let ppn = frame.ppn;
                SWAP_DEVICE
                    .lock()
                    .read(swap_tracker.0, ppn.get_bytes_array())?;
                *self = Frame::InMemory(frame);
                Ok(ppn)
            }
            _ => Err(MemoryError::NotSwappedOut),
        }
    }

    /// 压缩驻留页并返回原 frame，供调用方跨越 PTE/TLB 提交边界持有。
    #[cfg(feature = "oom_handler")]
    pub fn zip(&mut self) -> Result<Arc<FrameTracker>, MemoryError> {
        match self {
            Frame::InMemory(frame_ref) => {
                if Arc::strong_count(frame_ref) == 1 {
                    if let Ok(zram_tracker) =
                        ZRAM_DEVICE.lock().write(frame_ref.ppn.get_bytes_array())
                    {
                        let old = core::mem::replace(self, Frame::Compressed(zram_tracker));
                        let Frame::InMemory(frame) = old else {
                            unreachable!("zip source changed while exclusively borrowed")
                        };
                        Ok(frame)
                    } else {
                        Err(MemoryError::ZramIsFull)
                    }
                } else {
                    Err(MemoryError::SharedPage)
                }
            }
            _ => Err(MemoryError::NotInMemory),
        }
    }

    #[cfg(feature = "oom_handler")]
    pub fn unzip(&mut self) -> Result<PhysPageNum, MemoryError> {
        match self {
            Frame::Compressed(zram_tracker) => {
                let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
                let ppn = frame.ppn;
                ZRAM_DEVICE
                    .lock()
                    .read(zram_tracker.0, ppn.get_bytes_array())
                    .map_err(|_| MemoryError::BackingStoreFailure)?;
                *self = Frame::InMemory(frame);
                Ok(ppn)
            }
            _ => Err(MemoryError::NotCompressed),
        }
    }
}

#[derive(Clone)]
pub struct VmPageStore {
    pub vpn_range: VPNRange,
    // Frame改为BTree存储，避免使用Vec导致大量写入时直接写满堆内存
    frames: BTreeMap<VirtPageNum, Frame>,
    #[cfg(feature = "oom_handler")]
    active: VecDeque<VirtPageNum>,
    #[cfg(feature = "oom_handler")]
    compressed: usize,
    #[cfg(feature = "oom_handler")]
    swapped: usize,
}

impl Debug for VmPageStore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        #[cfg(feature = "oom_handler")]
        return f
            .debug_struct("VmPageStore")
            .field("vpn_range", &self.vpn_range)
            .field("resident_entries", &self.frames.len())
            .field("active", &self.active.len())
            .field("compressed", &self.compressed)
            .field("swapped", &self.swapped)
            .finish();
        #[cfg(not(feature = "oom_handler"))]
        return f
            .debug_struct("VmPageStore")
            .field("vpn_range", &self.vpn_range)
            .field("resident_entries", &self.frames.len())
            .finish();
    }
}

impl VmPageStore {
    pub fn try_clone(&self) -> Result<Self, isize> {
        Ok(Self {
            vpn_range: self.vpn_range,
            frames: self.frames.clone(),
            #[cfg(feature = "oom_handler")]
            active: VecDeque::new(),
            #[cfg(feature = "oom_handler")]
            compressed: self.compressed,
            #[cfg(feature = "oom_handler")]
            swapped: self.swapped,
        })
    }

    pub fn new(vpn_range: VPNRange) -> Self {
        Self::try_new(vpn_range).unwrap()
    }

    pub fn try_new(vpn_range: VPNRange) -> Result<Self, isize> {
        Ok(Self {
            vpn_range,
            frames: BTreeMap::new(),
            #[cfg(feature = "oom_handler")]
            active: VecDeque::new(),
            #[cfg(feature = "oom_handler")]
            compressed: 0,
            #[cfg(feature = "oom_handler")]
            swapped: 0,
        })
    }

    pub fn contains_vpn(&self, key: VirtPageNum) -> bool {
        key >= self.vpn_range.get_start() && key < self.vpn_range.get_end()
    }

    pub(super) fn frame_state(&self, key: VirtPageNum) -> Result<FrameState, MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        Ok(match self.frames.get(&key) {
            Some(Frame::InMemory(_)) => FrameState::InMemory,
            #[cfg(feature = "oom_handler")]
            Some(Frame::Compressed(_)) => FrameState::Compressed,
            #[cfg(feature = "oom_handler")]
            Some(Frame::SwappedOut(_)) => FrameState::SwappedOut,
            Some(Frame::Unallocated) | None => FrameState::Unallocated,
        })
    }

    pub fn frame_state_name(&self, key: &VirtPageNum) -> &'static str {
        match self.frame_state(*key) {
            Ok(FrameState::InMemory) => "InMemory",
            Ok(FrameState::Unallocated) => "Unallocated",
            #[cfg(feature = "oom_handler")]
            Ok(FrameState::Compressed) => "Compressed",
            #[cfg(feature = "oom_handler")]
            Ok(FrameState::SwappedOut) => "SwappedOut",
            Err(_) => "OutOfRange",
        }
    }

    pub fn get_in_memory(&self, key: &VirtPageNum) -> Option<&Arc<FrameTracker>> {
        if !self.contains_vpn(*key) {
            return None;
        }
        match self.frames.get(key) {
            Some(Frame::InMemory(tracker)) => Some(tracker),
            _ => None,
        }
    }

    pub(super) fn in_memory_len(&self) -> usize {
        self.frames
            .values()
            .filter(|frame| matches!(frame, Frame::InMemory(_)))
            .count()
    }

    pub(super) fn in_memory_len_in_range(&self, start: VirtPageNum, end: VirtPageNum) -> usize {
        let store_start = self.vpn_range.get_start();
        let store_end = self.vpn_range.get_end();
        let start = if start > store_start {
            start
        } else {
            store_start
        };
        let end = if end < store_end { end } else { store_end };
        if start >= end {
            return 0;
        }
        self.frames
            .range(start..end)
            .filter(|(_, frame)| matches!(frame, Frame::InMemory(_)))
            .count()
    }

    pub(super) fn for_each_in_memory_vpn<F>(&self, mut f: F)
    where
        F: FnMut(VirtPageNum),
    {
        for (vpn, frame) in self.frames.iter() {
            if matches!(frame, Frame::InMemory(_)) {
                f(*vpn);
            }
        }
    }

    pub(super) fn for_each_in_memory_vpn_in_range<F>(
        &self,
        start: VirtPageNum,
        end: VirtPageNum,
        mut f: F,
    ) where
        F: FnMut(VirtPageNum),
    {
        let store_start = self.vpn_range.get_start();
        let store_end = self.vpn_range.get_end();
        let start = if start > store_start {
            start
        } else {
            store_start
        };
        let end = if end < store_end { end } else { store_end };
        if start >= end {
            return;
        }
        for (vpn, frame) in self.frames.range(start..end) {
            if matches!(frame, Frame::InMemory(_)) {
                f(*vpn);
            }
        }
    }

    pub fn is_unallocated(&self, key: &VirtPageNum) -> bool {
        matches!(self.frame_state(*key), Ok(FrameState::Unallocated))
    }

    pub fn alloc_in_memory(
        &mut self,
        key: VirtPageNum,
        value: Arc<FrameTracker>,
    ) -> Result<(), MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        if matches!(self.frames.get(&key), Some(Frame::Unallocated)) {
            self.frames.remove(&key);
        }
        if self.frames.contains_key(&key) {
            return Err(MemoryError::AlreadyAllocated);
        }
        #[cfg(feature = "oom_handler")]
        self.record_active(key)?;
        self.frames.insert(key, Frame::InMemory(value));
        Ok(())
    }

    pub fn remove_in_memory(&mut self, key: &VirtPageNum) -> Option<Arc<FrameTracker>> {
        if !self.contains_vpn(*key) {
            return None;
        }
        #[cfg(feature = "oom_handler")]
        self.active.retain(|&elem| elem != *key);
        match self.frames.remove(key) {
            Some(Frame::InMemory(frame_ref)) => Some(frame_ref),
            Some(frame) => {
                self.insert_existing_frame(*key, frame);
                None
            }
            None => None,
        }
    }

    /// 返回指定范围内第一个驻留页，用于无额外分配地逐页解除映射。
    pub fn first_in_memory_vpn_in_range(
        &self,
        start: VirtPageNum,
        end: VirtPageNum,
    ) -> Option<VirtPageNum> {
        self.frames
            .range(start..end)
            .find_map(|(vpn, frame)| matches!(frame, Frame::InMemory(_)).then_some(*vpn))
    }

    pub(super) fn frame_mut_if_present(
        &mut self,
        key: VirtPageNum,
    ) -> Result<&mut Frame, MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        self.frames.get_mut(&key).ok_or(MemoryError::NotMapped)
    }

    pub fn set_start(&mut self, new_vpn_start: VirtPageNum) -> Result<(), ()> {
        let vpn_end = self.vpn_range.get_end();
        if new_vpn_start > vpn_end {
            return Err(());
        }
        self.vpn_range = VPNRange::new(new_vpn_start, vpn_end);
        self.prune_out_of_range();
        Ok(())
    }

    pub fn set_end(&mut self, new_vpn_end: VirtPageNum) -> Result<(), ()> {
        let vpn_start = self.vpn_range.get_start();
        if vpn_start > new_vpn_end {
            return Err(());
        }
        self.vpn_range = VPNRange::new(vpn_start, new_vpn_end);
        self.prune_out_of_range();
        Ok(())
    }

    #[inline(always)]
    pub fn into_two(&mut self, cut: VirtPageNum) -> Result<Self, ()> {
        let vpn_start = self.vpn_range.get_start();
        let vpn_end = self.vpn_range.get_end();
        if cut <= vpn_start || cut >= vpn_end {
            return Err(());
        }

        let second_frames = self.frames.split_off(&cut);

        #[cfg(feature = "oom_handler")]
        let (first_active, second_active) = Self::split_active_into_two(&self.active, cut);

        let mut second = VmPageStore {
            vpn_range: VPNRange::new(cut, vpn_end),
            frames: second_frames,
            #[cfg(feature = "oom_handler")]
            active: second_active,
            #[cfg(feature = "oom_handler")]
            compressed: 0,
            #[cfg(feature = "oom_handler")]
            swapped: 0,
        };

        self.vpn_range = VPNRange::new(vpn_start, cut);

        #[cfg(feature = "oom_handler")]
        {
            self.active = first_active;
            self.recount_oom_counters();
            second.recount_oom_counters();
        }

        Ok(second)
    }

    fn insert_existing_frame(&mut self, key: VirtPageNum, frame: Frame) {
        if !self.contains_vpn(key) {
            return;
        }
        match frame {
            Frame::Unallocated => {}
            frame => {
                #[cfg(feature = "oom_handler")]
                match &frame {
                    Frame::Compressed(_) => self.compressed += 1,
                    Frame::SwappedOut(_) => self.swapped += 1,
                    _ => {}
                }
                self.frames.insert(key, frame);
            }
        }
    }

    fn prune_out_of_range(&mut self) {
        let start = self.vpn_range.get_start();
        let end = self.vpn_range.get_end();
        self.frames.retain(|vpn, _| *vpn >= start && *vpn < end);
        #[cfg(feature = "oom_handler")]
        {
            self.active.retain(|&vpn| vpn >= start && vpn < end);
            self.recount_oom_counters();
        }
    }
}

#[cfg(feature = "oom_handler")]
impl VmPageStore {
    pub(super) fn record_active(&mut self, vpn: VirtPageNum) -> Result<(), MemoryError> {
        if !self.contains_vpn(vpn) {
            return Err(MemoryError::BadAddress);
        }
        self.active
            .try_reserve(1)
            .map_err(|_| MemoryError::OutOfMemory)?;
        self.active.push_back(vpn);
        Ok(())
    }

    pub(super) fn pop_active(&mut self) -> Option<VirtPageNum> {
        self.active.pop_front()
    }

    /// 把本轮因外部引用而不能回收的页放回候选队尾。
    ///
    /// 调用方刚从同一个 `VecDeque` 弹出一项，因此这里不会扩容。保留该项很重要：
    /// futex waiter 解除 backing pin 后，后续 OOM 扫描仍应有机会回收该页。
    pub(super) fn requeue_active(&mut self, vpn: VirtPageNum) {
        debug_assert!(self.contains_vpn(vpn));
        self.active.push_back(vpn);
    }

    pub(super) fn compressed_count(&self) -> usize {
        self.compressed
    }

    pub(super) fn swapped_count(&self) -> usize {
        self.swapped
    }

    pub(super) fn inc_compressed(&mut self) {
        self.compressed += 1;
    }

    pub(super) fn inc_swapped(&mut self) {
        self.swapped += 1;
    }

    pub(super) fn dec_compressed(&mut self) {
        self.compressed = self.compressed.saturating_sub(1);
    }

    pub(super) fn dec_swapped(&mut self) {
        self.swapped = self.swapped.saturating_sub(1);
    }

    pub(super) fn active_len(&self) -> usize {
        self.active.len()
    }

    fn recount_oom_counters(&mut self) {
        self.compressed = 0;
        self.swapped = 0;
        for frame in self.frames.values() {
            match frame {
                Frame::Compressed(_) => self.compressed += 1,
                Frame::SwappedOut(_) => self.swapped += 1,
                _ => {}
            }
        }
    }

    fn split_active_into_two(
        active: &VecDeque<VirtPageNum>,
        cut: VirtPageNum,
    ) -> (VecDeque<VirtPageNum>, VecDeque<VirtPageNum>) {
        if active.is_empty() {
            (VecDeque::new(), VecDeque::new())
        } else {
            active.iter().fold(
                (VecDeque::new(), VecDeque::new()),
                |(mut first_active, mut second_active), &vpn| {
                    if vpn < cut {
                        first_active.push_back(vpn);
                    } else {
                        second_active.push_back(vpn);
                    }
                    (first_active, second_active)
                },
            )
        }
    }
}
