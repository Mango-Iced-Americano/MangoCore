use super::filemap::{filemap_private_fault, filemap_read_fault, filemap_shared_write_fault};
use super::user_mapper::UserMapper;
use super::vma::Vma;
use super::vma::{VmAreaKind, VmAreaMapping, VmPageState};
use super::{FaultAccess, MemoryError, PageTable, PhysAddr, VirtAddr, VirtPageNum};
use crate::utils::error::SyscallErr;
use log::{error, warn};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FaultContext {
    pub addr: VirtAddr,
    pub vpn: VirtPageNum,
    pub access: FaultAccess,
}

impl FaultContext {
    pub fn new(addr: VirtAddr, access: FaultAccess) -> Self {
        Self {
            addr,
            vpn: addr.floor(),
            access,
        }
    }

    pub(super) fn offset_phys(self, ppn: super::PhysPageNum) -> PhysAddr {
        ppn.offset(self.addr.page_offset())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FaultAction {
    LazyAlloc,
    FileBackedRead,
    FileBackedWrite,
    FileBackedSharedWrite,
    #[cfg(feature = "oom_handler")]
    Decompress,
    #[cfg(feature = "oom_handler")]
    SwapIn,
    SharedWrite,
    StaleLazyPte,
    Cow,
    MappedRead,
    ResidentWithoutPte,
}

struct PageFaultHandler;

pub(super) fn handle_page_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    PageFaultHandler::handle(area, page_table, ctx)
}

impl PageFaultHandler {
    fn handle<T: PageTable>(
        area: &mut Vma,
        page_table: &mut T,
        ctx: FaultContext,
    ) -> Result<PhysAddr, MemoryError> {
        check_area_permission(area, ctx)?;

        match Self::classify(area, page_table, ctx)? {
            // 匿名页首次访问: 分配一个清零物理页。
            FaultAction::LazyAlloc => {
                map_lazy_zero_page(area, page_table, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // 文件映射页首次读取/执行: 直接映射文件页缓存。
            FaultAction::FileBackedRead => filemap_read_fault(area, page_table, ctx),
            // 文件映射页首次写入共享映射: 映射 page cache 帧并标脏。
            FaultAction::FileBackedSharedWrite => filemap_shared_write_fault(area, page_table, ctx),
            // 文件映射页首次写入私有映射: 分配私有物理页并从文件填充内容。
            FaultAction::FileBackedWrite => filemap_private_fault(area, page_table, ctx),
            // 压缩匿名页再次访问: 解压后恢复页表映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::Decompress => {
                finish_decompress_page(area, page_table, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // 已换出的匿名页再次访问: 从 swap/zram 换入后恢复映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::SwapIn => {
                finish_swap_in_page(area, page_table, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // MAP_SHARED 写保护 fault: 恢复共享写权限。
            FaultAction::SharedWrite => restore_shared_write(area, page_table, ctx),
            // stale lazy PTE: 页表已有项但元数据仍未分配，先清理再修复。
            FaultAction::StaleLazyPte => repair_stale_lazy_pte(area, page_table, ctx),
            // 私有已映射页写入: 触发 COW。
            FaultAction::Cow => copy_private_page(area, page_table, ctx),
            // 已映射页读取/执行: 直接翻译物理地址。
            FaultAction::MappedRead => translate_mapped_page(page_table, ctx),
            // MAP_SHARED anonymous pages may preallocate shared frames but install
            // user PTEs lazily so mincore can still observe real residency.
            FaultAction::ResidentWithoutPte => {
                map_existing_resident_page(area, page_table, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
        }
    }

    fn classify<T: PageTable>(
        area: &mut Vma,
        page_table: &mut T,
        ctx: FaultContext,
    ) -> Result<FaultAction, MemoryError> {
        if UserMapper::new(page_table).is_mapped(ctx.vpn) {
            return Ok(match ctx.access {
                FaultAccess::Load | FaultAccess::Execute => FaultAction::MappedRead,
                FaultAccess::Store if area.vm_mapping() == VmAreaMapping::Shared => {
                    FaultAction::SharedWrite
                }
                FaultAccess::Store if area.vm_is_stale_lazy(ctx.vpn) => FaultAction::StaleLazyPte,
                FaultAccess::Store => FaultAction::Cow,
            });
        }

        match area.vm_kind() {
            VmAreaKind::FileBacked => Ok(match ctx.access {
                FaultAccess::Store if area.vm_mapping() == VmAreaMapping::Shared => {
                    FaultAction::FileBackedSharedWrite
                }
                FaultAccess::Store => FaultAction::FileBackedWrite,
                FaultAccess::Load | FaultAccess::Execute => FaultAction::FileBackedRead,
            }),
            VmAreaKind::Anonymous => match area.vm_page_state(ctx.vpn)? {
                VmPageState::InMemory => Ok(FaultAction::ResidentWithoutPte),
                VmPageState::Unallocated => Ok(FaultAction::LazyAlloc),
                #[cfg(feature = "oom_handler")]
                VmPageState::Compressed => Ok(FaultAction::Decompress),
                #[cfg(feature = "oom_handler")]
                VmPageState::SwappedOut => Ok(FaultAction::SwapIn),
            },
        }
    }
}

fn check_area_permission(area: &Vma, ctx: FaultContext) -> Result<(), MemoryError> {
    if area.vm_allows(ctx.access) {
        Ok(())
    } else {
        error!(
            "[do_page_fault] addr: {:?}, access: {:?}, result: no permission",
            ctx.addr, ctx.access
        );
        Err(MemoryError::NoPermission)
    }
}

fn map_existing_resident_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    area.map_existing_in_memory(page_table, ctx.vpn)
}

fn map_lazy_zero_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let ppn = area.map_one_zeroed_unchecked(page_table, ctx.vpn)?;
    Ok(ppn)
}

#[cfg(feature = "oom_handler")]
fn finish_decompress_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let ppn = area.vm_decompress_page(ctx.vpn)?;
    UserMapper::new(page_table).map_user_page(ctx.vpn, ppn, area.vm_perm())?;
    area.vm_record_resident_page::<T>(ctx.vpn)?;
    area.vm_dec_compressed();
    Ok(ppn)
}

#[cfg(feature = "oom_handler")]
fn finish_swap_in_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let ppn = area.vm_swap_in_page(ctx.vpn)?;
    UserMapper::new(page_table).map_user_page(ctx.vpn, ppn, area.vm_perm())?;
    area.vm_record_resident_page::<T>(ctx.vpn)?;
    area.vm_dec_swapped();
    Ok(ppn)
}

fn restore_shared_write<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    // For file-backed shared pages: mark dirty in page cache before restoring W.
    if area.vm_kind() == VmAreaKind::FileBacked {
        if let (Some(inode), Ok(file_offset)) =
            (area.vm_file(), area.vm_file_offset(ctx.vpn))
        {
            if let Some(pc) = inode.ensure_page_cache() {
                let page_index = file_offset >> crate::config::PAGE_SIZE_BITS;
                if let Err(e) = pc.frame_for_write(page_index) {
                    return Err(match e {
                        SyscallErr::ENOMEM => MemoryError::OutOfMemory,
                        _ => MemoryError::BackingStoreFailure,
                    });
                }
            }
        }
    }
    let mut mapper = UserMapper::new(page_table);
    mapper.set_user_flags(ctx.vpn, area.vm_perm())?;
    let ppn = mapper.translate(ctx.vpn).ok_or(MemoryError::NotMapped)?;
    Ok(ctx.offset_phys(ppn))
}

fn repair_stale_lazy_pte<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    warn!(
        "[do_page_fault] clear stale lazy pte: addr={:?}, vpn={:?}, area={:?}",
        ctx.addr, ctx.vpn, area
    );
    area.clear_stale_pte(page_table, ctx.vpn);

    if area.vm_kind() == VmAreaKind::FileBacked {
        return Err(MemoryError::NotMapped);
    }

    let allocated_ppn = area.map_one_zeroed_unchecked(page_table, ctx.vpn)?;
    Ok(ctx.offset_phys(allocated_ppn))
}

fn copy_private_page<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let allocated_ppn = area.copy_on_write(page_table, ctx.vpn)?;
    Ok(ctx.offset_phys(allocated_ppn))
}

fn translate_mapped_page<T: PageTable>(
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let ppn = UserMapper::new(page_table)
        .translate(ctx.vpn)
        .ok_or(MemoryError::NotMapped)?;
    Ok(ctx.offset_phys(ppn))
}
