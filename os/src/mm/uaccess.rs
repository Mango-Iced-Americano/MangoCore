//! 用户地址访问辅助层。
//!
//! 本模块把用户指针转换、跨页缓冲区拆分、C 字符串读取和内核/用户拷贝统一
//! 封装在当前任务地址空间上。所有会 fault-in 的路径都要求 `token` 属于当前
//! 运行任务，避免跨进程地址空间访问绕过权限检查。
//!
//! # Errors
//!
//! 用户地址越界、NULL 指针、不属于当前任务的页表 token、跨页对象引用或缺页
//! 处理失败均返回负 errno，主要为 `-EFAULT` 或 `-ENOMEM`。
//!
//! # Locking
//!
//! fault-in 会获取当前进程 `AddressSpaceInner` 锁。调用方不得在已持有同一锁时进入
//! 本模块的 faulting uaccess 路径。
//! 标量、数组和字符串 copy 在每个用户页的 VM 锁内完成权限检查与实际
//! 访问，避免 PTE 翻译完成后被另一 CPU 并发 unmap、降权或 CoW。旧 `UserBuffer`
//! 尚未提供同等保证，调用方不得把其中的物理页切片长期保存或跨地址空间变更使用。

use core::marker::PhantomData;

use super::page_table::{FaultAccess, PageTable, UserAccess};
use super::{AddressSpace, PhysAddr, StepByOne, VirtAddr};
use crate::fs::iov::IOVec;
use crate::hal::PageTableImpl;
use crate::task::current_task;
use alloc::{string::String, sync::Arc, vec::Vec};

/// 单次用户缓冲区翻译上限，防止恶意长度导致内核 OOM。
const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 8;
const MAX_IOVEC_COUNT: usize = 1024;

/// Walk user pages from `ptr` to `ptr + len`, returning bytes before the
/// first page that is not already mapped with the requested user permission.
///
/// This is a non-faulting probe: it only walks existing PTEs and must not
/// allocate, fault-in, or trigger CoW/lazy population.
pub fn user_accessible_len(token: usize, ptr: *const u8, len: usize, access: UserAccess) -> usize {
    if len == 0 {
        return 0;
    }

    let start = ptr as usize;
    let end = match start.checked_add(len) {
        Some(v) => v,
        None => return 0,
    };
    if !uaccess_user_range_ok(start, end) {
        return 0;
    }

    // Keep the same current-task-only safety contract as faulting uaccess.
    if !is_current_user_token(token) {
        return 0;
    }

    let page_table = PageTableImpl::from_token(token);
    let mut cur = start;

    while cur < end {
        let va = VirtAddr::from(cur);
        let vpn = va.floor();

        if !page_table.user_access_ok(vpn, access).unwrap_or(false) {
            break;
        }

        let next_page = match vpn.start_addr().0.checked_add(crate::config::PAGE_SIZE) {
            Some(v) => v.min(end),
            None => end,
        };
        if next_page <= cur {
            break;
        }
        cur = next_page;
    }

    cur.saturating_sub(start)
}

#[derive(Clone, Copy)]
/// 只读用户指针包装。
///
/// # Semantics
///
/// `read` 会检查 NULL、确认 token 属于当前任务，并按需触发读缺页处理。
pub struct UserPtr<T> {
    ptr: *const T,
    _marker: PhantomData<T>,
}

impl<T> UserPtr<T> {
    pub const fn new(ptr: *const T) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    pub const fn from_addr(addr: usize) -> Self {
        Self::new(addr as *const T)
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn read(self, token: usize) -> Result<T, isize>
    where
        T: 'static + Copy,
    {
        if self.is_null() {
            return Err(crate::syscall::errno::EFAULT);
        }
        get_from_user(token, self.ptr)
    }

    pub fn read_optional(self, token: usize) -> Result<Option<T>, isize>
    where
        T: 'static + Copy,
    {
        if self.is_null() {
            Ok(None)
        } else {
            self.read(token).map(Some)
        }
    }
}

#[derive(Clone, Copy)]
/// 可写用户指针包装。
///
/// # Semantics
///
/// `read` 按读权限访问用户页，`write` 按写权限访问用户页。NULL 指针在
/// `write_optional(None)` 中被视为合法的“无输出地址”。
pub struct UserPtrMut<T> {
    ptr: *mut T,
    _marker: PhantomData<T>,
}

impl<T> UserPtrMut<T> {
    pub const fn new(ptr: *mut T) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    pub const fn from_addr(addr: usize) -> Self {
        Self::new(addr as *mut T)
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn read(self, token: usize) -> Result<T, isize>
    where
        T: 'static + Copy,
    {
        if self.is_null() {
            return Err(crate::syscall::errno::EFAULT);
        }
        get_from_user(token, self.ptr as *const T)
    }

    pub fn write(self, token: usize, value: &T) -> Result<(), isize>
    where
        T: 'static + Copy,
    {
        if self.is_null() {
            return Err(crate::syscall::errno::EFAULT);
        }
        copy_to_user(token, value, self.ptr)
    }

    pub fn write_optional(self, token: usize, value: Option<&T>) -> Result<(), isize>
    where
        T: 'static + Copy,
    {
        match (self.is_null(), value) {
            (true, None) => Ok(()),
            (true, Some(_)) => Err(crate::syscall::errno::EFAULT),
            (false, None) => Ok(()),
            (false, Some(value)) => self.write(token, value),
        }
    }
}

#[derive(Clone, Copy)]
/// 用户数组切片描述符。
///
/// # Semantics
///
/// `len` 以 `T` 为单位，实际访问前会检查乘法溢出和 `MAX_BUFFER_SIZE` 上限。
pub struct UserSlice<T> {
    ptr: *const T,
    len: usize,
    _marker: PhantomData<T>,
}

impl<T> UserSlice<T> {
    pub const fn new(ptr: *const T, len: usize) -> Self {
        Self {
            ptr,
            len,
            _marker: PhantomData,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn len(&self) -> usize {
        self.len
    }

    fn checked_byte_len(&self) -> Result<usize, isize> {
        let byte_len = core::mem::size_of::<T>()
            .checked_mul(self.len)
            .ok_or(crate::syscall::errno::EFAULT)?;
        if byte_len > MAX_BUFFER_SIZE {
            return Err(crate::syscall::errno::EFAULT);
        }
        Ok(byte_len)
    }

    pub fn read_array_into(self, token: usize, dst: &mut [T]) -> Result<(), isize>
    where
        T: 'static + Copy,
    {
        if dst.len() < self.len {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.checked_byte_len()?;
        copy_from_user_array(token, self.ptr, dst.as_mut_ptr(), self.len)
    }

    pub fn write_array_from(self, token: usize, src: &[T]) -> Result<(), isize>
    where
        T: 'static + Copy,
    {
        if src.len() < self.len {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.checked_byte_len()?;
        copy_to_user_array(token, src.as_ptr(), self.ptr as *mut T, self.len)
    }
}

#[derive(Clone, Copy)]
/// NUL 结尾的用户字符串指针。
///
/// # Semantics
///
/// `read` 最多读取 `MAX_BUFFER_SIZE` 字节，并在跨页时逐页 fault-in。
pub struct UserCString {
    ptr: *const u8,
}

impl UserCString {
    pub const fn new(ptr: *const u8) -> Self {
        Self { ptr }
    }

    pub const fn from_addr(addr: usize) -> Self {
        Self::new(addr as *const u8)
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn read(self, token: usize) -> Result<String, isize> {
        if self.is_null() {
            return Err(crate::syscall::errno::EFAULT);
        }
        translated_str(token, self.ptr)
    }
}

pub struct UserBufferReader {
    buffer: UserBuffer,
}

impl UserBufferReader {
    /// 创建只读用户缓冲区视图。
    ///
    /// # Errors
    ///
    /// 用户地址非法、超出上限或缺页处理失败时返回负 errno。
    pub fn new(token: usize, ptr: *const u8, len: usize) -> Result<Self, isize> {
        // Try single-page fast path first; fall back to Vec of page slices
        let buffer = match translate_single_page_user_bytes(token, ptr, len, UserAccess::Read)? {
            Some(slice) => UserBuffer::single(slice),
            None => UserBuffer::new(translated_byte_buffer(token, ptr, len, UserAccess::Read)?),
        };
        Ok(Self { buffer })
    }

    pub fn into_user_buffer(self) -> UserBuffer {
        self.buffer
    }

    pub fn read_to_vec(&self, cap: usize) -> Result<Vec<u8>, isize> {
        if self.buffer.len() > cap {
            return Err(crate::syscall::errno::EFAULT);
        }
        let mut dst = Vec::new();
        dst.try_reserve(self.buffer.len())
            .map_err(|_| crate::syscall::errno::ENOMEM)?;
        // Safety: `try_reserve` has reserved `buffer.len()` bytes and `u8` has
        // no drop glue. `UserBuffer::read` below initializes the whole slice.
        unsafe {
            dst.set_len(self.buffer.len());
        }
        self.buffer.read(&mut dst);
        Ok(dst)
    }

    pub fn read_into(&self, dst: &mut [u8]) -> Result<usize, isize> {
        Ok(self.buffer.read(dst))
    }
}

pub struct UserBufferWriter {
    buffer: UserBuffer,
}

impl UserBufferWriter {
    /// 创建只写用户缓冲区视图。
    ///
    /// # Errors
    ///
    /// 用户地址非法、超出上限或缺页处理失败时返回负 errno。
    pub fn new(token: usize, ptr: *mut u8, len: usize) -> Result<Self, isize> {
        let buffer = match translate_single_page_user_bytes(
            token,
            ptr as *const u8,
            len,
            UserAccess::Write,
        )? {
            Some(slice) => UserBuffer::single(slice),
            None => UserBuffer::new(translated_byte_buffer(
                token,
                ptr as *const u8,
                len,
                UserAccess::Write,
            )?),
        };
        Ok(Self { buffer })
    }

    pub fn into_user_buffer(self) -> UserBuffer {
        self.buffer
    }

    pub fn write_from(&mut self, src: &[u8]) -> Result<usize, isize> {
        Ok(self.buffer.write(src))
    }
}

pub struct UserIoVec {
    token: usize,
    iovecs: Vec<IOVec>,
    total_len: usize,
    total_cap: usize,
}

impl UserIoVec {
    /// 从用户空间读取 `iovec` 数组并计算总长度。
    ///
    /// # Errors
    ///
    /// `iovcnt` 超过上限、长度累加溢出或用户数组不可读时返回负 errno。
    pub fn read_user_iovecs(
        token: usize,
        iov: *const IOVec,
        iovcnt: usize,
        total_cap: usize,
    ) -> Result<Self, isize> {
        if iovcnt > MAX_IOVEC_COUNT {
            return Err(crate::syscall::errno::EINVAL);
        }
        let mut iovecs = Vec::<IOVec>::new();
        iovecs
            .try_reserve(iovcnt)
            .map_err(|_| crate::syscall::errno::ENOMEM)?;
        if iovcnt != 0 {
            copy_from_user_array(token, iov, iovecs.as_mut_ptr(), iovcnt)?;
        }
        // Safety: `try_reserve(iovcnt)` prepared storage and a successful
        // `copy_from_user_array` initialized exactly `iovcnt` entries.
        unsafe {
            iovecs.set_len(iovcnt);
        }
        let mut total_len = 0usize;
        for iovec in iovecs.iter() {
            total_len = total_len
                .checked_add(iovec.iov_len)
                .filter(|len| *len <= isize::MAX as usize)
                .ok_or(crate::syscall::errno::EINVAL)?;
        }
        Ok(Self {
            token,
            iovecs,
            total_len,
            total_cap,
        })
    }

    pub fn total_len(&self) -> usize {
        self.total_len
    }

    pub fn capped_len(&self) -> usize {
        self.total_len.min(self.total_cap)
    }

    pub fn reader_buffer(&self) -> Result<UserBuffer, isize> {
        self.build_user_buffer(UserAccess::Read)
    }

    pub fn writer_buffer(&self) -> Result<UserBuffer, isize> {
        self.build_user_buffer(UserAccess::Write)
    }

    fn build_user_buffer(&self, access: UserAccess) -> Result<UserBuffer, isize> {
        let mut buffers = Vec::with_capacity(32);
        let mut total_len = 0usize;
        for iovec in self.iovecs.iter() {
            if total_len >= self.total_cap {
                break;
            }
            let iov_len = iovec.iov_len.min(self.total_cap - total_len);
            if iov_len == 0 {
                continue;
            }
            translated_byte_buffer_append_to_existing_vec(
                &mut buffers,
                self.token,
                iovec.iov_base,
                iov_len,
                access,
            )?;
            total_len += iov_len;
        }
        Ok(UserBuffer::new(buffers))
    }

    /// Return how many logical bytes starting at `offset` are accessible in user memory.
    /// Stops at the first inaccessible byte due to page fault or overflow.
    pub fn accessible_len_at(&self, offset: usize, len: usize, access: UserAccess) -> usize {
        let mut remaining = len;
        let mut logical_off = 0usize;
        let mut total_accessible = 0usize;

        for iovec in self.iovecs.iter() {
            if remaining == 0 {
                break;
            }

            let iov_end = match logical_off.checked_add(iovec.iov_len) {
                Some(v) => v,
                None => break,
            };
            if iov_end <= offset {
                logical_off = iov_end;
                continue;
            }

            let inner_off = offset.saturating_sub(logical_off);
            let take = remaining.min(iovec.iov_len.saturating_sub(inner_off));
            if take == 0 {
                logical_off = iov_end;
                continue;
            }

            let base = iovec.iov_base as usize;
            let ptr = match base.checked_add(inner_off) {
                Some(v) => v,
                None => break,
            };

            let accessible = user_accessible_len(self.token, ptr as *const u8, take, access);
            total_accessible = total_accessible.saturating_add(accessible);
            if accessible < take {
                break;
            }

            remaining -= take;
            logical_off = iov_end;
        }

        total_accessible.min(len)
    }

    /// Build a read-only UserBuffer for logical range [offset, offset+len).
    pub fn reader_buffer_at(&self, offset: usize, len: usize) -> Result<UserBuffer, isize> {
        self.build_user_buffer_at(offset, len, UserAccess::Read)
    }

    /// Build a write-only UserBuffer for logical range [offset, offset+len).
    pub fn writer_buffer_at(&self, offset: usize, len: usize) -> Result<UserBuffer, isize> {
        self.build_user_buffer_at(offset, len, UserAccess::Write)
    }

    fn build_user_buffer_at(
        &self,
        offset: usize,
        len: usize,
        access: UserAccess,
    ) -> Result<UserBuffer, isize> {
        let mut buffers = Vec::with_capacity(32);
        let mut remaining = len;
        let mut logical_off = 0usize;
        for iovec in self.iovecs.iter() {
            let iov_end = match logical_off.checked_add(iovec.iov_len) {
                Some(v) => v,
                None => break,
            };
            if iov_end <= offset {
                logical_off = iov_end;
                continue;
            }
            let inner_off = offset.saturating_sub(logical_off);
            let take = remaining.min(iovec.iov_len.saturating_sub(inner_off));
            if take == 0 {
                logical_off = iov_end;
                continue;
            }
            let base = iovec.iov_base as usize;
            let ptr = match base.checked_add(inner_off) {
                Some(v) => v,
                None => break,
            };
            translated_byte_buffer_append_to_existing_vec(
                &mut buffers,
                self.token,
                ptr as *const u8,
                take,
                access,
            )?;
            logical_off = iov_end;
            remaining = remaining.saturating_sub(take);
            if remaining == 0 {
                break;
            }
        }
        Ok(UserBuffer::new(buffers))
    }
}

/// 检查用户地址区间边界和加法溢出。
///
/// # Semantics
///
/// 该函数不访问页表、不 fault-in 页面，只验证 `[ptr, ptr + len)` 位于用户
/// 虚拟地址范围内。
pub fn check_user_range(ptr: usize, len: usize) -> Result<usize, isize> {
    if len == 0 {
        return Ok(ptr);
    }
    let end = ptr.checked_add(len).ok_or(crate::syscall::errno::EFAULT)?;
    if !uaccess_user_range_ok(ptr, end) {
        return Err(crate::syscall::errno::EFAULT);
    }
    Ok(end)
}

pub(crate) fn uaccess_user_range_ok(ptr: usize, end: usize) -> bool {
    // la64 可能传入低地址用户指针；真实合法性由后续页表权限检查决定。
    ptr < crate::config::USER_VA_END && end <= crate::config::USER_VA_END
}

fn is_current_user_token(token: usize) -> bool {
    crate::task::try_current_user_token() == Some(token)
}

fn current_user_vm(token: usize) -> Result<Arc<AddressSpace<PageTableImpl>>, isize> {
    let task = current_task().ok_or(crate::syscall::errno::EFAULT)?;
    if crate::task::current_user_token() != token {
        return Err(crate::syscall::errno::EFAULT);
    }
    Ok(task.process.vm())
}

/// 用户内存复制方向。
///
/// 枚举中只保存 kernel raw pointer；用户物理地址必须在持有 VM 锁时重新 fault-in
/// 并取得，不能跨越 closure 泄漏。
#[derive(Clone, Copy)]
enum UserCopy {
    FromUser { dst: *mut u8 },
    ToUser { src: *const u8 },
}

impl UserCopy {
    fn fault_access(self) -> FaultAccess {
        match self {
            Self::FromUser { .. } => FaultAccess::Load,
            Self::ToUser { .. } => FaultAccess::Store,
        }
    }

    /// 在 VM 锁保护的已验证物理页内完成一个 chunk 的复制。
    ///
    /// # Safety
    ///
    /// `user_ptr` 必须来自当前 closure 内 `fault_in_user_va()` 返回的 PA；枚举中的
    /// kernel pointer 必须对 `[offset, offset + len)` 保持有效。
    unsafe fn copy_chunk(self, user_ptr: *mut u8, offset: usize, len: usize) {
        match self {
            Self::FromUser { dst } => core::ptr::copy(user_ptr, dst.add(offset), len),
            Self::ToUser { src } => core::ptr::copy(src.add(offset), user_ptr, len),
        }
    }
}

/// 逐页执行 fault、权限后验检查和复制。
///
/// 每个 chunk 的 direct-map 访问都发生在对应 `AddressSpace::write` closure 内，
/// 因而同一页上的 fork/CoW、mprotect 和 munmap 只能发生在复制之前或之后。跨页
/// 复制允许并发地址空间修改插入；若后续页失效，调用方收到 `EFAULT`，前页可能已经
/// 完成复制，这与 faultable uaccess 的部分完成语义一致。
fn copy_user_bytes(
    token: usize,
    user_addr: usize,
    len: usize,
    direction: UserCopy,
) -> Result<(), isize> {
    if len == 0 {
        return Ok(());
    }
    if len > MAX_BUFFER_SIZE || user_addr == 0 {
        return Err(crate::syscall::errno::EFAULT);
    }
    let end = check_user_range(user_addr, len)?;
    let vm = current_user_vm(token)?;
    let mut copied = 0usize;

    while copied < len {
        let va = VirtAddr::from(user_addr + copied);
        let chunk_len = (crate::config::PAGE_SIZE - va.page_offset()).min(end - va.0);
        vm.write(|address_space| -> Result<(), isize> {
            // 必须使用 uaccess 专用入口：它在 fault 后再次核对 U/R/W 权限和物理范围。
            let pa = address_space.fault_in_user_va(va, direction.fault_access())?;
            // Safety: VM 锁让 PTE 与 frame 在整个 copy_chunk 期间保持稳定；PA 已由
            // fault_in_user_va 验证，chunk 也被限制在当前物理页内。
            unsafe {
                direction.copy_chunk(pa.direct_map_ptr(), copied, chunk_len);
            }
            Ok(())
        })?;
        copied += chunk_len;
    }
    Ok(())
}

// 区分用户触发缺页时的权限：copy_from_user 使用 Read，copy_to_user 使用
// Write；仍需双向权限的旧 buffer 路径使用 ReadWrite。
fn fault_in_user_va_with_vm(
    vm: &AddressSpace<PageTableImpl>,
    va: VirtAddr,
    access: FaultAccess,
) -> Result<PhysAddr, isize> {
    vm.write(|address_space| address_space.fault_in_user_va(va, access))
}

fn translate_user_va_checked_with_vm(
    vm: &AddressSpace<PageTableImpl>,
    va: VirtAddr,
    access: UserAccess,
) -> Result<PhysAddr, isize> {
    check_user_range(va.0, 1)?;

    match access {
        UserAccess::Read => fault_in_user_va_with_vm(vm, va, FaultAccess::Load),
        UserAccess::Write => fault_in_user_va_with_vm(vm, va, FaultAccess::Store),
        UserAccess::ReadWrite => {
            fault_in_user_va_with_vm(vm, va, FaultAccess::Load)?;
            fault_in_user_va_with_vm(vm, va, FaultAccess::Store)
        }
    }
}

/// 将当前任务的用户虚拟地址翻译为物理地址。
///
/// # Semantics
///
/// 只接受当前任务的页表 token。访问前会按 `access` 触发缺页处理，成功后返回
/// 对应物理地址。
pub fn translate_user_va_checked(
    token: usize,
    va: VirtAddr,
    access: UserAccess,
) -> Result<PhysAddr, isize> {
    let vm = current_user_vm(token)?;
    translate_user_va_checked_with_vm(&vm, va, access)
}

/// 将用户缓冲区按页拆成内核可访问的字节切片。
///
/// # Semantics
///
/// 每个页片都会按 `access` fault-in。返回的切片指向直接映射物理内存，只能在
/// 当前 syscall 路径内短期使用。
pub fn translate_user_buffer_checked(
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<Vec<&'static mut [u8]>, isize> {
    if len > MAX_BUFFER_SIZE {
        log::warn!("[kernel] translate_user_buffer_checked: requested length {} exceeds maximum {}, returning EFAULT", len, MAX_BUFFER_SIZE);
        return Err(crate::syscall::errno::EFAULT);
    }
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(crate::syscall::errno::EFAULT);
    }
    let mut start = ptr as usize;
    let end = check_user_range(start, len)?;
    let vm = current_user_vm(token)?;
    let mut v = Vec::with_capacity(32);
    while start < end {
        let start_va = VirtAddr::from(start);
        let pa = translate_user_va_checked_with_vm(&vm, start_va, access)?;
        let ppn = pa.floor();
        let mut next_vpn = start_va.floor();
        next_vpn.step();
        let mut end_va: VirtAddr = next_vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    Ok(v)
}

/// 将翻译后的用户页片追加到已有 vector。
pub fn translated_byte_buffer_append_to_existing_vec(
    existing_vec: &mut Vec<&'static mut [u8]>,
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<(), isize> {
    existing_vec.extend(translate_user_buffer_checked(token, ptr, len, access)?);
    Ok(())
}

pub fn translated_byte_buffer(
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<Vec<&'static mut [u8]>, isize> {
    translate_user_buffer_checked(token, ptr, len, access)
}

/// Fault-in 并验证一段当前任务用户地址，但不返回物理页视图。
///
/// 该接口只用于必须在产生外部副作用前提前验证输出区间的 ABI。真正读写时仍须再次走
/// `copy_from/to_user`，因为另一个 CPU 可在预校验完成后立刻修改映射。
pub fn fault_in_user_range(
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<(), isize> {
    if len == 0 {
        return Ok(());
    }
    if len > MAX_BUFFER_SIZE || ptr.is_null() {
        return Err(crate::syscall::errno::EFAULT);
    }
    let mut cur = ptr as usize;
    let end = check_user_range(cur, len)?;
    let vm = current_user_vm(token)?;

    while cur < end {
        let va = VirtAddr::from(cur);
        translate_user_va_checked_with_vm(&vm, va, access)?;
        cur = va
            .floor()
            .start_addr()
            .0
            .checked_add(crate::config::PAGE_SIZE)
            .ok_or(crate::syscall::errno::EFAULT)?
            .min(end);
    }
    Ok(())
}

fn append_user_cstr_bytes(dst: &mut String, bytes: &[u8], max_len: usize) -> Result<(), isize> {
    if bytes.is_empty() {
        return Ok(());
    }

    if bytes.iter().all(u8::is_ascii) {
        let remaining = max_len.saturating_sub(dst.len());
        if bytes.len() >= remaining {
            return Err(crate::syscall::errno::EFAULT);
        }
        let s = core::str::from_utf8(bytes).map_err(|_| crate::syscall::errno::EFAULT)?;
        dst.push_str(s);
        return Ok(());
    }

    for &ch in bytes {
        if dst.len() >= max_len {
            return Err(crate::syscall::errno::EFAULT);
        }
        dst.push(ch as char);
        if dst.len() >= max_len {
            return Err(crate::syscall::errno::EFAULT);
        }
    }
    Ok(())
}

/// 从用户空间读取 NUL 结尾字符串。
///
/// # Semantics
///
/// 逐页读取直到遇到 NUL，最大长度为 `MAX_BUFFER_SIZE`。非 ASCII 字节按原有
/// 字节值映射到 `char`，用于兼容现有路径。
pub fn translated_str(token: usize, ptr: *const u8) -> Result<String, isize> {
    if ptr.is_null() {
        return Err(crate::syscall::errno::EFAULT);
    }
    let mut string = String::new();
    let mut cur = ptr as usize;
    let max_len = MAX_BUFFER_SIZE;
    // 物理页内容先在 VM 锁内复制到内核 scratch，NUL 扫描和 String 扩容都在锁外完成。
    // 这样既不泄漏 direct-map slice，也不把 heap allocator 带入 VM 临界区。
    let mut scratch = [0u8; crate::config::PAGE_SIZE];
    loop {
        if string.len() >= max_len {
            return Err(crate::syscall::errno::EFAULT);
        }

        let va = VirtAddr::from(cur);
        let page_offset = va.page_offset();
        let page_len = (crate::config::PAGE_SIZE - page_offset).min(
            crate::config::USER_VA_END
                .checked_sub(cur)
                .ok_or(crate::syscall::errno::EFAULT)?,
        );
        if page_len == 0 {
            return Err(crate::syscall::errno::EFAULT);
        }

        copy_user_bytes(
            token,
            cur,
            page_len,
            UserCopy::FromUser {
                dst: scratch.as_mut_ptr(),
            },
        )?;
        let bytes = &scratch[..page_len];
        if let Some(nul_pos) = bytes.iter().position(|&ch| ch == 0) {
            append_user_cstr_bytes(&mut string, &bytes[..nul_pos], max_len)?;
            break;
        }
        append_user_cstr_bytes(&mut string, bytes, max_len)?;
        cur = cur
            .checked_add(page_len)
            .ok_or(crate::syscall::errno::EFAULT)?;
    }
    Ok(string)
}

// 用户缓冲区可能跨页；这里保留分段形式，避免把大用户缓冲区复制到连续内核内存。
enum UserBufferSegments {
    Empty,
    Single(&'static mut [u8]),
    Multi(Vec<&'static mut [u8]>),
}

/// 已翻译的用户缓冲区。
///
/// # Semantics
///
/// 缓冲区由一个或多个页内切片组成，读写方法返回实际复制字节数。该对象借用
/// 当前地址空间中已 fault-in 的物理页，不能跨调度点长期保存。
pub struct UserBuffer {
    segments: UserBufferSegments,
    pub len: usize,
}

impl UserBuffer {
    pub fn new(buffers: Vec<&'static mut [u8]>) -> Self {
        let len = buffers.iter().map(|buffer| buffer.len()).sum();
        if buffers.len() == 0 {
            return Self {
                segments: UserBufferSegments::Empty,
                len,
            };
        }
        if buffers.len() == 1 {
            let single = buffers.into_iter().next().unwrap();
            return Self {
                segments: UserBufferSegments::Single(single),
                len,
            };
        }
        Self {
            segments: UserBufferSegments::Multi(buffers),
            len,
        }
    }

    pub(crate) fn single(slice: &'static mut [u8]) -> Self {
        let len = slice.len();
        Self {
            segments: UserBufferSegments::Single(slice),
            len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn read(&self, dst: &mut [u8]) -> usize {
        match &self.segments {
            UserBufferSegments::Empty => 0,
            UserBufferSegments::Single(buf) => {
                let n = buf.len().min(dst.len());
                // Safety: both slices are valid for `n` bytes and may overlap
                // conservatively handled by `copy`.
                unsafe {
                    core::ptr::copy(buf.as_ptr(), dst.as_mut_ptr(), n);
                }
                n
            }
            UserBufferSegments::Multi(buffers) => {
                let mut start = 0;
                let dst_len = dst.len();
                for buffer in buffers.iter() {
                    let end = start + buffer.len();
                    if end > dst_len {
                        let n = dst_len - start;
                        // Safety: `start < dst_len` and `n` is capped by the
                        // remaining destination length.
                        unsafe {
                            core::ptr::copy(buffer.as_ptr(), dst.as_mut_ptr().add(start), n);
                        }
                        return dst_len;
                    } else {
                        let n = buffer.len();
                        // Safety: the logical cursor guarantees the destination
                        // range `[start, start + n)` is within `dst`.
                        unsafe {
                            core::ptr::copy(buffer.as_ptr(), dst.as_mut_ptr().add(start), n);
                        }
                    }
                    start = end;
                }
                self.len
            }
        }
    }

    pub fn write(&mut self, src: &[u8]) -> usize {
        match &mut self.segments {
            UserBufferSegments::Empty => 0,
            UserBufferSegments::Single(buf) => {
                let n = buf.len().min(src.len());
                // Safety: both slices are valid for `n` bytes and may overlap
                // conservatively handled by `copy`.
                unsafe {
                    core::ptr::copy(src.as_ptr(), buf.as_mut_ptr(), n);
                }
                n
            }
            UserBufferSegments::Multi(buffers) => {
                let mut start = 0;
                let src_len = src.len();
                for buffer in buffers.iter_mut() {
                    let end = start + buffer.len();
                    if end > src_len {
                        let n = src_len - start;
                        // Safety: `start < src_len` and `n` is capped by the
                        // remaining source length.
                        unsafe {
                            core::ptr::copy(src.as_ptr().add(start), buffer.as_mut_ptr(), n);
                        }
                        return src_len;
                    } else {
                        let n = buffer.len();
                        // Safety: the logical cursor guarantees the source range
                        // `[start, start + n)` is within `src`.
                        unsafe {
                            core::ptr::copy(src.as_ptr().add(start), buffer.as_mut_ptr(), n);
                        }
                    }
                    start = end;
                }
                self.len
            }
        }
    }

    pub fn read_at(&self, offset: usize, dst: &mut [u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        match &self.segments {
            UserBufferSegments::Empty => 0,
            UserBufferSegments::Single(buf) => {
                let start = offset;
                let n = (buf.len() - start).min(dst.len());
                // Safety: `offset < self.len`, and `n` is capped by both the
                // source segment and destination slice lengths.
                unsafe {
                    core::ptr::copy(buf.as_ptr().add(start), dst.as_mut_ptr(), n);
                }
                n
            }
            UserBufferSegments::Multi(buffers) => {
                let mut read_bytes = 0usize;
                let mut dst_start = 0usize;
                let copy_limit = dst.len().saturating_add(offset);
                for buffer in buffers.iter() {
                    let dst_end = dst_start + buffer.len();
                    let copy_dst_start = dst_start.max(offset);
                    let copy_dst_end = dst_end.min(copy_limit);
                    if copy_dst_start >= copy_dst_end {
                        dst_start = dst_end;
                        continue;
                    }
                    let copy_src_start = copy_dst_start - offset;
                    let copy_src_end = copy_dst_end - offset;
                    let copy_buffer_start = copy_dst_start - dst_start;
                    let copy_buffer_end = copy_dst_end - dst_start;
                    let n = copy_src_end - copy_src_start;
                    // Safety: the computed subranges are intersections of the
                    // requested logical range with valid source/destination slices.
                    unsafe {
                        core::ptr::copy(
                            buffer.as_ptr().add(copy_buffer_start),
                            dst.as_mut_ptr().add(copy_src_start),
                            n,
                        );
                    }
                    read_bytes += copy_dst_end - copy_dst_start;
                    dst_start = dst_end;
                }
                read_bytes
            }
        }
    }

    pub fn write_at(&mut self, offset: usize, src: &[u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        match &mut self.segments {
            UserBufferSegments::Empty => 0,
            UserBufferSegments::Single(buf) => {
                let start = offset;
                let n = (buf.len() - start).min(src.len());
                // Safety: `offset < self.len`, and `n` is capped by both the
                // destination segment and source slice lengths.
                unsafe {
                    core::ptr::copy(src.as_ptr(), buf.as_mut_ptr().add(start), n);
                }
                n
            }
            UserBufferSegments::Multi(buffers) => {
                let mut write_bytes = 0usize;
                let mut dst_start = 0usize;
                let copy_limit = src.len().saturating_add(offset);
                for buffer in buffers.iter_mut() {
                    let dst_end = dst_start + buffer.len();
                    let copy_dst_start = dst_start.max(offset);
                    let copy_dst_end = dst_end.min(copy_limit);
                    if copy_dst_start >= copy_dst_end {
                        dst_start = dst_end;
                        continue;
                    }
                    let copy_src_start = copy_dst_start - offset;
                    let copy_src_end = copy_dst_end - offset;
                    let copy_buffer_start = copy_dst_start - dst_start;
                    let copy_buffer_end = copy_dst_end - dst_start;
                    let n = copy_src_end - copy_src_start;
                    // Safety: the computed subranges are intersections of the
                    // requested logical range with valid source/destination slices.
                    unsafe {
                        core::ptr::copy(
                            src.as_ptr().add(copy_src_start),
                            buffer.as_mut_ptr().add(copy_buffer_start),
                            n,
                        );
                    }
                    write_bytes += copy_dst_end - copy_dst_start;
                    dst_start = dst_end;
                }
                write_bytes
            }
        }
    }

    pub fn clear(&mut self) {
        match &mut self.segments {
            UserBufferSegments::Empty => {}
            UserBufferSegments::Single(buf) => buf.fill(0),
            UserBufferSegments::Multi(buffers) => buffers.iter_mut().for_each(|buffer| {
                buffer.fill(0);
            }),
        }
    }

    pub fn fill_at(&mut self, offset: usize, len: usize, value: u8) -> usize {
        if len == 0 || offset >= self.len {
            return 0;
        }
        match &mut self.segments {
            UserBufferSegments::Empty => 0,
            UserBufferSegments::Single(buf) => {
                let start = offset;
                let n = (buf.len() - start).min(len);
                buf[start..start + n].fill(value);
                n
            }
            UserBufferSegments::Multi(buffers) => {
                let limit = offset.saturating_add(len).min(self.len);
                let mut logical = 0usize;
                let mut filled = 0usize;
                for buffer in buffers.iter_mut() {
                    let next = logical + buffer.len();
                    let start = logical.max(offset);
                    let end = next.min(limit);
                    if start < end {
                        let b0 = start - logical;
                        let b1 = end - logical;
                        buffer[b0..b1].fill(value);
                        filled += end - start;
                    }
                    logical = next;
                    if logical >= limit {
                        break;
                    }
                }
                filled
            }
        }
    }
}

pub fn translate_single_page_user_bytes(
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<Option<&'static mut [u8]>, isize> {
    if len == 0 {
        return Ok(None);
    }
    if ptr.is_null() {
        return Err(crate::syscall::errno::EFAULT);
    }

    let start = ptr as usize;
    let end = check_user_range(start, len)?;
    let start_va = VirtAddr::from(start);
    let last_va = VirtAddr::from(end - 1);
    if start_va.floor() != last_va.floor() {
        return Ok(None);
    }

    let pa = translate_user_va_checked(token, start_va, access)?;
    let page_offset = start_va.page_offset();
    Ok(Some(
        &mut pa.floor().get_bytes_array()[page_offset..page_offset + len],
    ))
}

/// Copy `*src: T` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
///
/// # Semantics
///
/// `token` must belong to the current task. `dst` must point to writable kernel
/// memory for one `T`.
pub fn copy_from_user<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>();
    if size == 0 {
        return Ok(());
    }
    copy_user_bytes(
        token,
        src as usize,
        size,
        UserCopy::FromUser {
            dst: dst as *mut u8,
        },
    )
}

/// Copy array `*src: [T;len]` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
///
/// # Semantics
///
/// `dst` must point to writable kernel memory for `len` elements.
pub fn copy_from_user_array<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
    len: usize,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>()
        .checked_mul(len)
        .ok_or(crate::syscall::errno::EFAULT)?;
    if size == 0 {
        return Ok(());
    }
    copy_user_bytes(
        token,
        src as usize,
        size,
        UserCopy::FromUser {
            dst: dst as *mut u8,
        },
    )
}

/// Copy `*src: T` to user space.
/// `src` is a pointer in kernel space, `dst` is a pointer in user space.
///
/// # Semantics
///
/// `src` must point to readable kernel memory for one `T`; `dst` is validated
/// against the current task's user address space.
pub fn copy_to_user<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>();
    if size == 0 {
        return Ok(());
    }
    copy_user_bytes(
        token,
        dst as usize,
        size,
        UserCopy::ToUser {
            src: src as *const u8,
        },
    )
}

/// Copy `*src: T` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
#[inline(always)]
pub fn get_from_user<T: 'static + Copy>(token: usize, src: *const T) -> Result<T, isize> {
    // Safety: `copy_from_user` initializes the MaybeUninit storage on success.
    // `T: Copy` means returning the initialized value does not create aliasing
    // or drop-order obligations.
    unsafe {
        let mut dst = core::mem::MaybeUninit::<T>::uninit();
        copy_from_user(token, src, dst.as_mut_ptr())?;
        return Ok(dst.assume_init());
    }
}

#[inline(always)]
pub fn try_get_from_user<T: 'static + Copy>(
    token: usize,
    src: *const T,
) -> Result<Option<T>, isize> {
    if !src.is_null() {
        Ok(Some(get_from_user(token, src)?))
    } else {
        Ok(None)
    }
}

/// Copy array `*src: [T;len]` to user space.
/// `src` is a pointer in kernel space, `dst` is a pointer in user space.
///
/// # Semantics
///
/// `src` must point to readable kernel memory for `len` elements.
pub fn copy_to_user_array<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
    len: usize,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>()
        .checked_mul(len)
        .ok_or(crate::syscall::errno::EFAULT)?;
    if size == 0 {
        return Ok(());
    }
    copy_user_bytes(
        token,
        dst as usize,
        size,
        UserCopy::ToUser {
            src: src as *const u8,
        },
    )
}

/// Automatically add `'\0'` in the end,
/// so total written length is `src.len() + 1` (with trailing `'\0'`).
/// # Warning
/// Caller should ensure `src` is not too large, or this function will write out of bound.
pub fn copy_to_user_string(token: usize, src: &str, dst: *mut u8) -> Result<(), isize> {
    let size = src
        .len()
        .checked_add(1)
        .ok_or(crate::syscall::errno::EFAULT)?;
    if size > MAX_BUFFER_SIZE {
        return Err(crate::syscall::errno::EFAULT);
    }
    copy_to_user_array(token, src.as_ptr(), dst, src.len())?;
    let nul = 0u8;
    // Safety: `size = src.len() + 1` 已检查溢出，因此末尾地址计算不会回绕。
    copy_to_user(token, &nul, unsafe { dst.add(src.len()) })
}
