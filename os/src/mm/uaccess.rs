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
//! 标量、数组、字符串和 `UserBuffer` copy 都在每个用户页的 VM 锁内完成权限
//! 检查与实际访问，避免 PTE 翻译完成后被另一 CPU 并发 unmap、降权或 CoW。
//! `UserBuffer` 只保存虚拟地址区间，不保存物理页、direct-map 指针或 Rust slice。

use core::marker::PhantomData;

use super::page_table::{FaultAccess, PageTable, UserAccess};
use super::{AddressSpace, VirtAddr};
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
        let buffer = UserBuffer::from_range(token, ptr as usize, len, UserAccess::Read)?;
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
        // 先初始化为 0，避免并发映射变化导致部分 copy 时留下未初始化尾部。
        dst.resize(self.buffer.len(), 0);
        let copied = self.buffer.read_into(&mut dst)?;
        if copied != self.buffer.len() {
            return Err(crate::syscall::errno::EFAULT);
        }
        Ok(dst)
    }

    pub fn read_into(&self, dst: &mut [u8]) -> Result<usize, isize> {
        self.buffer.read_into(dst)
    }

    /// 完整读取固定格式对象；部分完成对这类调用仍属于 `EFAULT`。
    pub fn read_exact(&self, dst: &mut [u8]) -> Result<(), isize> {
        if dst.len() > self.buffer.len() {
            return Err(crate::syscall::errno::EFAULT);
        }
        if self.buffer.read_into(dst)? == dst.len() {
            Ok(())
        } else {
            Err(crate::syscall::errno::EFAULT)
        }
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
        let buffer = UserBuffer::from_range(token, ptr as usize, len, UserAccess::Write)?;
        Ok(Self { buffer })
    }

    /// 构造从 `ptr` 开始、当前可以写入的最大连续前缀。
    ///
    /// 已可写页只在同一次 VM 临界区内检查 PTE；遇到第一个不可写页时，若
    /// 前缀为空则 fault-in 该页，否则立即返回已有前缀。这样 read/pread 可以
    /// 先完成 POSIX partial-read，而不是为了后续页提前触发 CoW 或 TLB flush。
    /// 返回的 `UserBuffer` 仍只保存 VA，真正写入时会逐页重新校验映射。
    pub fn new_writable_prefix(
        token: usize,
        ptr: *mut u8,
        len: usize,
    ) -> Result<(Self, usize), isize> {
        if len == 0 {
            return Ok((
                Self {
                    buffer: UserBuffer::from_range_without_fault(
                        token,
                        ptr as usize,
                        0,
                        UserAccess::Write,
                    ),
                },
                0,
            ));
        }
        if len > MAX_BUFFER_SIZE || ptr.is_null() {
            return Err(crate::syscall::errno::EFAULT);
        }

        let start = ptr as usize;
        let end = check_user_range(start, len)?;
        let vm = current_user_vm(token)?;
        let accessible = vm.write(|address_space| -> Result<usize, isize> {
            let mut current = start;
            while current < end {
                let va = VirtAddr::from(current);
                if address_space
                    .resolve_user_va(va, FaultAccess::Store)
                    .is_err()
                {
                    if current != start {
                        break;
                    }
                    address_space.fault_in_user_va(va, FaultAccess::Store)?;
                }

                let page_end = va
                    .floor()
                    .start_addr()
                    .0
                    .checked_add(crate::config::PAGE_SIZE)
                    .ok_or(crate::syscall::errno::EFAULT)?;
                let next = page_end.min(end);
                if next <= current {
                    return Err(crate::syscall::errno::EFAULT);
                }
                current = next;
            }
            Ok(current - start)
        })?;

        Ok((
            Self {
                buffer: UserBuffer::from_range_without_fault(
                    token,
                    start,
                    accessible,
                    UserAccess::Write,
                ),
            },
            accessible,
        ))
    }

    pub fn into_user_buffer(self) -> UserBuffer {
        self.buffer
    }

    pub fn write_from(&mut self, src: &[u8]) -> Result<usize, isize> {
        self.buffer.write_from(src)
    }

    /// 完整写回固定格式对象；不会把部分 copy 伪装成全量成功。
    pub fn write_all(&mut self, src: &[u8]) -> Result<(), isize> {
        if src.len() > self.buffer.len() {
            return Err(crate::syscall::errno::EFAULT);
        }
        if self.buffer.write_from(src)? == src.len() {
            Ok(())
        } else {
            Err(crate::syscall::errno::EFAULT)
        }
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
        let mut ranges = Vec::new();
        ranges
            .try_reserve(self.iovecs.len())
            .map_err(|_| crate::syscall::errno::ENOMEM)?;
        let mut total_len = 0usize;
        for iovec in self.iovecs.iter() {
            if total_len >= self.total_cap {
                break;
            }
            let iov_len = iovec.iov_len.min(self.total_cap - total_len);
            if iov_len == 0 {
                continue;
            }
            let start = iovec.iov_base as usize;
            check_user_range(start, iov_len)?;
            ranges.push(UserRange {
                start,
                len: iov_len,
            });
            total_len += iov_len;
        }
        UserBuffer::from_ranges(self.token, ranges, total_len, access)
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
        let mut ranges = Vec::new();
        ranges
            .try_reserve(self.iovecs.len())
            .map_err(|_| crate::syscall::errno::ENOMEM)?;
        let mut remaining = len.min(self.capped_len().saturating_sub(offset));
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
            let start = base
                .checked_add(inner_off)
                .ok_or(crate::syscall::errno::EFAULT)?;
            check_user_range(start, take)?;
            ranges.push(UserRange { start, len: take });
            logical_off = iov_end;
            remaining = remaining.saturating_sub(take);
            if remaining == 0 {
                break;
            }
        }
        let total_len = len.min(self.capped_len().saturating_sub(offset)) - remaining;
        UserBuffer::from_ranges(self.token, ranges, total_len, access)
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
/// 枚举中只保存 kernel raw pointer；用户物理地址必须在持有 VM 锁时重新解析，
/// 不能跨越 closure 泄漏。
#[derive(Clone, Copy)]
enum UserCopy {
    FromUser { dst: *mut u8 },
    ToUser { src: *const u8 },
    FillUser { value: u8 },
}

#[derive(Clone, Copy)]
enum UserCopyMode {
    /// 普通路径允许按需调入页面或处理 CoW。
    FaultIn,
    /// 不可睡眠锁内只接受仍然有效的 PTE，不进入 fault handler。
    NoFault,
}

impl UserCopy {
    fn fault_access(self) -> FaultAccess {
        match self {
            Self::FromUser { .. } => FaultAccess::Load,
            Self::ToUser { .. } | Self::FillUser { .. } => FaultAccess::Store,
        }
    }

    /// 在 VM 锁保护的已验证物理页内完成一个 chunk 的复制。
    ///
    /// # Safety
    ///
    /// `user_ptr` 必须来自当前 closure 内 uaccess 校验返回的 PA；枚举中的
    /// kernel pointer 必须对 `[offset, offset + len)` 保持有效。
    unsafe fn copy_chunk(self, user_ptr: *mut u8, offset: usize, len: usize) {
        match self {
            Self::FromUser { dst } => core::ptr::copy(user_ptr, dst.add(offset), len),
            Self::ToUser { src } => core::ptr::copy(src.add(offset), user_ptr, len),
            Self::FillUser { value } => core::ptr::write_bytes(user_ptr, value, len),
        }
    }
}

struct UserCopyFault {
    copied: usize,
    errno: isize,
}

/// 逐页执行地址解析、权限后验检查和复制。
///
/// 每个 chunk 的 direct-map 访问都发生在对应 `AddressSpace::write` closure 内，
/// 因而同一页上的 fork/CoW、mprotect 和 munmap 只能发生在复制之前或之后。跨页
/// 复制允许并发地址空间修改插入；若后续页失效，精确复制返回 `EFAULT`，流式复制则
/// 返回已经完成的前缀，这与 faultable uaccess 的部分完成语义一致。
fn copy_user_bytes(
    token: usize,
    user_addr: usize,
    len: usize,
    direction: UserCopy,
) -> Result<(), isize> {
    match copy_user_bytes_progress(token, user_addr, len, direction, UserCopyMode::FaultIn) {
        Ok(copied) if copied == len => Ok(()),
        Ok(_) => Err(crate::syscall::errno::EFAULT),
        Err(error) => Err(error.errno),
    }
}

/// 与 `copy_user_bytes()` 使用同一锁域，但保留失败前已经完成的字节数。
fn copy_user_bytes_progress(
    token: usize,
    user_addr: usize,
    len: usize,
    direction: UserCopy,
    mode: UserCopyMode,
) -> Result<usize, UserCopyFault> {
    if len == 0 {
        return Ok(0);
    }
    if len > MAX_BUFFER_SIZE || user_addr == 0 {
        return Err(UserCopyFault {
            copied: 0,
            errno: crate::syscall::errno::EFAULT,
        });
    }
    let end =
        check_user_range(user_addr, len).map_err(|errno| UserCopyFault { copied: 0, errno })?;
    let vm = current_user_vm(token).map_err(|errno| UserCopyFault { copied: 0, errno })?;
    let mut copied = 0usize;

    while copied < len {
        let va = VirtAddr::from(user_addr + copied);
        let chunk_len = (crate::config::PAGE_SIZE - va.page_offset()).min(end - va.0);
        if matches!(mode, UserCopyMode::FaultIn) {
            vm.fault_in_user_va_retry(va, direction.fault_access())
                .map_err(|errno| UserCopyFault { copied, errno })?;
        }
        let result = vm.write(|address_space| -> Result<(), isize> {
            // 两种模式最终都在同一 VM 锁内核对 U/R/W 权限和物理范围；区别只是
            // `NoFault` 发现 PTE 已变化时立即失败，不在外层 spin lock 内等待。
            let pa = match mode {
                UserCopyMode::FaultIn => address_space.resolve_user_va(va, direction.fault_access())?,
                UserCopyMode::NoFault => {
                    address_space.resolve_user_va(va, direction.fault_access())?
                }
            };
            // Safety: VM 锁让 PTE 与 frame 在整个 copy_chunk 期间保持稳定；PA 已由
            // uaccess 解析入口验证，chunk 也被限制在当前物理页内。
            unsafe {
                direction.copy_chunk(pa.direct_map_ptr(), copied, chunk_len);
            }
            Ok(())
        });
        if let Err(errno) = result {
            return Err(UserCopyFault { copied, errno });
        }
        copied += chunk_len;
    }
    Ok(copied)
}

/// 在同一个 VM 临界区内完成所需方向的 fault-in。
fn fault_user_access_with_vm(
    vm: &AddressSpace<PageTableImpl>,
    va: VirtAddr,
    access: UserAccess,
) -> Result<(), isize> {
    check_user_range(va.0, 1)?;
    if access.needs_read() {
        vm.fault_in_user_va_retry(va, FaultAccess::Load)?;
    }
    if access.needs_write() {
        vm.fault_in_user_va_retry(va, FaultAccess::Store)?;
    }
    Ok(())
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
        fault_user_access_with_vm(&vm, va, access)?;
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

#[derive(Clone, Copy)]
struct UserRange {
    start: usize,
    len: usize,
}

enum UserRanges {
    /// 普通 read/write 只保存一个区间，不为快路径额外分配 `Vec`。
    Contiguous(UserRange),
    /// readv/writev 保存 iovec 的逻辑区间，不再展开为逐物理页切片。
    Scatter(Vec<UserRange>),
}

impl UserRanges {
    fn as_slice(&self) -> &[UserRange] {
        match self {
            Self::Contiguous(range) => core::slice::from_ref(range),
            Self::Scatter(ranges) => ranges,
        }
    }
}

/// 描述当前地址空间中一段或多段用户虚拟地址。
///
/// 构造阶段只负责提前 fault-in；对象不会保存 PA、direct-map pointer 或 Rust slice。
/// 每次实际复制都会重新取得 VM 锁并校验 PTE，因此可与并发 CoW、mprotect 和 munmap
/// 排序。跨页失败遵循“首字节失败返回 errno，已有进度则返回字节数”的规则。
pub struct UserBuffer {
    token: usize,
    ranges: UserRanges,
    len: usize,
    access: UserAccess,
}

/// Sequential writer for PageCache read plans.
///
/// PageCache produces chunks in logical order.  Retaining the current logical
/// offset avoids rebuilding a temporary kernel buffer for every read chunk and
/// gives the user-copy layer one monotonic cursor to validate.
pub struct UserBufferWriteCursor<'a> {
    buffer: &'a mut UserBuffer,
    offset: usize,
}

impl UserBuffer {
    /// 只登记虚拟地址范围，不在构造阶段再次 fault-in。
    ///
    /// 调用方可在一次 VM 临界区内先验证或 fault-in 前缀；后续实际复制仍会
    /// 逐页重新取得 VM 锁并校验 PTE，因此该对象不保存可失效的物理页视图。
    fn from_range_without_fault(
        token: usize,
        start: usize,
        len: usize,
        access: UserAccess,
    ) -> Self {
        Self {
            token,
            ranges: UserRanges::Contiguous(UserRange { start, len }),
            len,
            access,
        }
    }

    fn from_range(
        token: usize,
        start: usize,
        len: usize,
        access: UserAccess,
    ) -> Result<Self, isize> {
        fault_in_user_range(token, start as *const u8, len, access)?;
        Ok(Self {
            token,
            ranges: UserRanges::Contiguous(UserRange { start, len }),
            len,
            access,
        })
    }

    fn from_ranges(
        token: usize,
        ranges: Vec<UserRange>,
        expected_len: usize,
        access: UserAccess,
    ) -> Result<Self, isize> {
        let mut actual_len = 0usize;
        for range in &ranges {
            actual_len = actual_len
                .checked_add(range.len)
                .ok_or(crate::syscall::errno::EFAULT)?;
            fault_in_user_range(token, range.start as *const u8, range.len, access)?;
        }
        if actual_len != expected_len {
            return Err(crate::syscall::errno::EFAULT);
        }
        Ok(Self {
            token,
            ranges: UserRanges::Scatter(ranges),
            len: actual_len,
            access,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn write_cursor(&mut self) -> UserBufferWriteCursor<'_> {
        UserBufferWriteCursor {
            buffer: self,
            offset: 0,
        }
    }

    /// 从用户缓冲区读取到内核 slice。
    pub fn read_into(&self, dst: &mut [u8]) -> Result<usize, isize> {
        self.read_into_at(0, dst)
    }

    /// 从逻辑 `offset` 开始读取到内核 slice。
    pub fn read_into_at(&self, offset: usize, dst: &mut [u8]) -> Result<usize, isize> {
        self.read_into_at_mode(offset, dst, UserCopyMode::FaultIn)
    }

    /// 只复制仍保持映射的前缀，不触发缺页处理。
    ///
    /// 仅供已经在锁外完成 fault-in、但锁内不能等待的实现使用。
    pub(crate) fn read_into_at_nofault(
        &self,
        offset: usize,
        dst: &mut [u8],
    ) -> Result<usize, isize> {
        self.read_into_at_mode(offset, dst, UserCopyMode::NoFault)
    }

    fn read_into_at_mode(
        &self,
        offset: usize,
        dst: &mut [u8],
        mode: UserCopyMode,
    ) -> Result<usize, isize> {
        let len = dst.len().min(self.len.saturating_sub(offset));
        if len == 0 {
            return Ok(0);
        }
        if !self.access.needs_read() {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.transfer(
            offset,
            len,
            BufferCopy::FromUser {
                dst: dst.as_mut_ptr(),
            },
            mode,
        )
    }

    /// 将内核 slice 写入用户缓冲区。
    pub fn write_from(&mut self, src: &[u8]) -> Result<usize, isize> {
        self.write_from_at(0, src)
    }

    /// 将内核 slice 写入从逻辑 `offset` 开始的用户缓冲区。
    pub fn write_from_at(&mut self, offset: usize, src: &[u8]) -> Result<usize, isize> {
        let len = src.len().min(self.len.saturating_sub(offset));
        if len == 0 {
            return Ok(0);
        }
        if !self.access.needs_write() {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.transfer(
            offset,
            len,
            BufferCopy::ToUser { src: src.as_ptr() },
            UserCopyMode::FaultIn,
        )
    }

    /// 将内核数据写入仍保持映射的用户前缀，不触发缺页处理。
    pub(crate) fn write_from_at_nofault(
        &mut self,
        offset: usize,
        src: &[u8],
    ) -> Result<usize, isize> {
        let len = src.len().min(self.len.saturating_sub(offset));
        if len == 0 {
            return Ok(0);
        }
        if !self.access.needs_write() {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.transfer(
            offset,
            len,
            BufferCopy::ToUser { src: src.as_ptr() },
            UserCopyMode::NoFault,
        )
    }

    /// 用固定字节填充逻辑区间；常用于 `/dev/zero` 与 sparse hole。
    pub fn fill_at(&mut self, offset: usize, len: usize, value: u8) -> Result<usize, isize> {
        let len = len.min(self.len.saturating_sub(offset));
        if len == 0 {
            return Ok(0);
        }
        if !self.access.needs_write() {
            return Err(crate::syscall::errno::EFAULT);
        }
        self.transfer(
            offset,
            len,
            BufferCopy::FillUser { value },
            UserCopyMode::FaultIn,
        )
    }

    pub fn clear(&mut self) -> Result<usize, isize> {
        self.fill_at(0, self.len, 0)
    }

    fn transfer(
        &self,
        offset: usize,
        len: usize,
        copy: BufferCopy,
        mode: UserCopyMode,
    ) -> Result<usize, isize> {
        if len == 0 || offset >= self.len {
            return Ok(0);
        }

        let limit = offset.saturating_add(len).min(self.len);
        let mut logical = 0usize;
        let mut copied = 0usize;
        for range in self.ranges.as_slice() {
            let next = logical + range.len;
            let start = logical.max(offset);
            let end = next.min(limit);
            if start < end {
                let range_offset = start - logical;
                let user_addr = range
                    .start
                    .checked_add(range_offset)
                    .ok_or(crate::syscall::errno::EFAULT)?;
                let chunk_len = end - start;
                // Safety: `copied + chunk_len <= len`，而调用者提供的 kernel slice
                // 至少有 `len` 字节；这里只移动 kernel pointer，不解引用用户指针。
                let chunk_copy = unsafe { copy.at(copied) };
                match copy_user_bytes_progress(self.token, user_addr, chunk_len, chunk_copy, mode) {
                    Ok(done) => copied += done,
                    Err(error) => {
                        copied += error.copied;
                        return if copied == 0 {
                            Err(error.errno)
                        } else {
                            Ok(copied)
                        };
                    }
                }
            }
            logical = next;
            if logical >= limit {
                break;
            }
        }
        Ok(copied)
    }
}

impl UserBufferWriteCursor<'_> {
    /// Copy the next sequential source chunk to the user buffer.
    pub fn try_write_from(&mut self, src: &[u8]) -> Result<usize, isize> {
        let copied = self.buffer.write_from_at(self.offset, src)?;
        self.offset = self.offset.saturating_add(copied);
        Ok(copied)
    }

    /// Lossy convenience form for callers that already treat a short copy as
    /// an error; new kernel paths should use `try_write_from`.
    pub fn write_from(&mut self, src: &[u8]) -> usize {
        self.try_write_from(src).unwrap_or(0)
    }
}

#[derive(Clone, Copy)]
enum BufferCopy {
    FromUser { dst: *mut u8 },
    ToUser { src: *const u8 },
    FillUser { value: u8 },
}

impl BufferCopy {
    /// # Safety
    ///
    /// `offset` 必须仍位于构造该方向时传入的 kernel slice 内。
    unsafe fn at(self, offset: usize) -> UserCopy {
        match self {
            Self::FromUser { dst } => UserCopy::FromUser {
                dst: dst.add(offset),
            },
            Self::ToUser { src } => UserCopy::ToUser {
                src: src.add(offset),
            },
            Self::FillUser { value } => UserCopy::FillUser { value },
        }
    }
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
