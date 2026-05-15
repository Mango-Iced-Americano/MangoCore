use core::fmt::Debug;

use super::{frame_alloc, FrameTracker, MemoryError, PhysPageNum, VirtPageNum, VPNRange};
#[cfg(feature = "zram")]
use super::zram::{ZramTracker, ZRAM_DEVICE};
#[cfg(feature = "swap")]
use crate::fs::swap::{SwapTracker, SWAP_DEVICE};
use alloc::collections::BTreeMap;
#[cfg(feature = "oom_handler")]
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

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
    pub fn insert_in_memory(
        &mut self,
        frame_tracker: Arc<FrameTracker>,
    ) -> Result<(), MemoryError> {
        match self {
            Frame::Unallocated => {
                *self = Frame::InMemory(frame_tracker);
                Ok(())
            }
            _ => Err(MemoryError::AlreadyAllocated),
        }
    }

    pub fn take_in_memory(&mut self) -> Option<Arc<FrameTracker>> {
        match self {
            Frame::InMemory(frame_ref) => {
                let frame = unsafe { core::ptr::read(frame_ref) };
                unsafe { core::ptr::write(self, Frame::Unallocated) };
                Some(frame)
            }
            _ => None,
        }
    }

    #[cfg(feature = "oom_handler")]
    pub fn swap_out(&mut self) -> Result<usize, MemoryError> {
        match self {
            Frame::InMemory(frame_ref) => {
                if Arc::strong_count(frame_ref) == 1 {
                    let swap_tracker = SWAP_DEVICE.lock().write(frame_ref.ppn.get_bytes_array());
                    let swap_id = swap_tracker.0;
                    *self = Frame::SwappedOut(swap_tracker);
                    Ok(swap_id)
                } else {
                    Err(MemoryError::SharedPage)
                }
            }
            _ => Err(MemoryError::NotInMemory),
        }
    }

    /// This does not check the frame reference count.
    #[cfg(feature = "oom_handler")]
    pub fn force_swap_out(&mut self) -> Result<usize, MemoryError> {
        match self {
            Frame::InMemory(frame_ref) => {
                let swap_tracker = SWAP_DEVICE.lock().write(frame_ref.ppn.get_bytes_array());
                let swap_id = swap_tracker.0;
                *self = Frame::SwappedOut(swap_tracker);
                Ok(swap_id)
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
                    .read(swap_tracker.0, ppn.get_bytes_array());
                *self = Frame::InMemory(frame);
                Ok(ppn)
            }
            _ => Err(MemoryError::NotSwappedOut),
        }
    }

    #[cfg(feature = "oom_handler")]
    pub fn zip(&mut self) -> Result<usize, MemoryError> {
        match self {
            Frame::InMemory(frame_ref) => {
                if Arc::strong_count(frame_ref) == 1 {
                    if let Ok(zram_tracker) =
                        ZRAM_DEVICE.lock().write(frame_ref.ppn.get_bytes_array())
                    {
                        let zram_id = zram_tracker.0;
                        *self = Frame::Compressed(zram_tracker);
                        Ok(zram_id)
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
        Ok(self.clone())
    }

    pub fn gen_dict(&self, vpn_range: VPNRange) -> VmPageStore {
        Self::new(vpn_range)
    }

    pub fn get_start(&self) -> VirtPageNum {
        self.vpn_range.get_start()
    }

    pub fn get_end(&self) -> VirtPageNum {
        self.vpn_range.get_end()
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

    pub fn from_existing_frames(vpn_range: VPNRange, frames: Vec<Frame>) -> Self {
        let mut store = Self::new(vpn_range);
        let start = store.vpn_range.get_start();
        for (offset, frame) in frames.into_iter().enumerate() {
            let vpn = VirtPageNum(start.0 + offset);
            store.insert_existing_frame(vpn, frame);
        }
        store
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

    pub(super) fn frame_mut_if_present(
        &mut self,
        key: VirtPageNum,
    ) -> Result<&mut Frame, MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        self.frames.get_mut(&key).ok_or(MemoryError::NotMapped)
    }

    pub(super) fn set_frame(
        &mut self,
        key: VirtPageNum,
        frame: Frame,
    ) -> Result<Option<Frame>, MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        Ok(self.set_frame_unchecked(key, frame))
    }

    pub(super) fn take_frame(&mut self, key: VirtPageNum) -> Result<Option<Frame>, MemoryError> {
        if !self.contains_vpn(key) {
            return Err(MemoryError::BadAddress);
        }
        #[cfg(feature = "oom_handler")]
        self.active.retain(|&elem| elem != key);
        let removed = self.frames.remove(&key);
        #[cfg(feature = "oom_handler")]
        self.recount_oom_counters();
        Ok(removed)
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

    pub fn into_three(
        &mut self,
        first_cut: VirtPageNum,
        second_cut: VirtPageNum,
    ) -> Result<(Self, Self), ()> {
        let mut second = self.into_two(first_cut)?;
        let third = second.into_two(second_cut)?;
        Ok((second, third))
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

    fn set_frame_unchecked(&mut self, key: VirtPageNum, frame: Frame) -> Option<Frame> {
        let old = match frame {
            Frame::Unallocated => self.frames.remove(&key),
            frame => self.frames.insert(key, frame),
        };
        #[cfg(feature = "oom_handler")]
        self.recount_oom_counters();
        old
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
