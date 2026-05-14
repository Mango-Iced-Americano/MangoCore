#![allow(dead_code)]

use super::frame_store::{Frame, FrameState};
use super::map_area::{MapArea, MapFlags, MapPermission};
use super::{FaultAccess, MemoryError, PageTable, PhysPageNum, VirtPageNum};
use crate::fs::file_trait::File;
use alloc::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmAreaKind {
    Anonymous,
    FileBacked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmAreaMapping {
    Private,
    Shared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmPageState {
    InMemory,
    Unallocated,
    #[cfg(feature = "oom_handler")]
    Compressed,
    #[cfg(feature = "oom_handler")]
    SwappedOut,
}

impl MapArea {
    pub(super) fn vm_kind(&self) -> VmAreaKind {
        if self.map_file.is_some() {
            VmAreaKind::FileBacked
        } else {
            VmAreaKind::Anonymous
        }
    }

    pub(super) fn vm_mapping_type(&self) -> VmAreaMapping {
        if self.flags.contains(MapFlags::MAP_SHARED) {
            VmAreaMapping::Shared
        } else {
            VmAreaMapping::Private
        }
    }

    pub(super) fn vm_mapping(&self) -> VmAreaMapping {
        self.vm_mapping_type()
    }

    pub(super) fn vm_perm(&self) -> MapPermission {
        self.map_perm
    }

    pub(super) fn vm_access_allows(&self, access: FaultAccess) -> bool {
        let required = match access {
            FaultAccess::Load => MapPermission::R,
            FaultAccess::Store => MapPermission::W,
            FaultAccess::Execute => MapPermission::X,
        };
        self.vm_perm().contains(required)
    }

    pub(super) fn vm_allows(&self, access: FaultAccess) -> bool {
        self.vm_access_allows(access)
    }

    pub(super) fn vm_page_state(&self, vpn: VirtPageNum) -> Result<VmPageState, MemoryError> {
        Ok(match self.inner.frame_state(vpn)? {
            FrameState::InMemory => VmPageState::InMemory,
            FrameState::Unallocated => VmPageState::Unallocated,
            #[cfg(feature = "oom_handler")]
            FrameState::Compressed => VmPageState::Compressed,
            #[cfg(feature = "oom_handler")]
            FrameState::SwappedOut => VmPageState::SwappedOut,
        })
    }

    pub(super) fn vm_is_stale_lazy(&self, vpn: VirtPageNum) -> bool {
        matches!(self.vm_page_state(vpn), Ok(VmPageState::Unallocated))
    }

    pub(super) fn vm_file(&self) -> Option<Arc<dyn File>> {
        self.map_file.clone()
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_decompress_page(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<PhysPageNum, MemoryError> {
        self.vm_frame_mut(vpn)?.unzip()
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_swap_in_page(&mut self, vpn: VirtPageNum) -> Result<PhysPageNum, MemoryError> {
        self.vm_frame_mut(vpn)?.swap_in()
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_record_resident_page<T: PageTable>(
        &mut self,
        vpn: VirtPageNum,
    ) -> Result<(), MemoryError> {
        self.inner.record_active(vpn)
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_dec_compressed(&mut self) {
        self.inner.dec_compressed();
    }

    #[cfg(feature = "oom_handler")]
    pub(super) fn vm_dec_swapped(&mut self) {
        self.inner.dec_swapped();
    }

    #[cfg(feature = "oom_handler")]
    fn vm_frame_mut(&mut self, vpn: VirtPageNum) -> Result<&mut Frame, MemoryError> {
        self.inner.frame_mut_if_present(vpn)
    }
}
