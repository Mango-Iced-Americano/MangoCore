use core::ops::IndexMut;

pub use super::memory_set::check_page_fault;
use super::{MapPermission, PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
use alloc::string::String;
use alloc::vec::Vec;

// 防止用户一次性传入过大参数导致oom
const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 8;

// user-copy 方向，读是从用户拿，写是往用户填
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccess {
    Read,
    Write,
    ReadWrite,
}

impl UserAccess {
    #[inline(always)]
    pub fn needs_read(self) -> bool {
        matches!(self, UserAccess::Read | UserAccess::ReadWrite)
    }

    #[inline(always)]
    pub fn needs_write(self) -> bool {
        matches!(self, UserAccess::Write | UserAccess::ReadWrite)
    }
}

// 缺页时区分读写取指
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultAccess {
    Load,
    Store,
    Execute,
}

#[allow(unused)]
pub trait PageTable {
    /// 基本映射操作
    /// map、unmap、translate、translate_va
    /// 通过指定flags将vpn映射到ppn
    /// # 注意
    /// Allocation should be done elsewhere.
    /// # 特例
    /// Panics if the `vpn` is mapped.
    fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission);
    #[inline(always)]
    fn map_identical(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map(vpn, ppn, flags)
    }
    #[allow(unused)]
    /// Unmap the `vpn` to `ppn` with the `flags`.
    /// # Exceptions
    /// Panics if the `vpn` is NOT mapped (invalid).
    fn unmap(&mut self, vpn: VirtPageNum);
    #[inline(always)]
    fn unmap_identical(&mut self, vpn: VirtPageNum) {
        self.unmap(vpn)
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` if exists
    /// `None` is returned if nothing is found.
    fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum>;
    /// Translate the virtual address into its corresponding `PhysAddr` if mapped in current page table.
    /// `None` is returned if nothing is found.
    fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr>;
    fn block_and_ret_mut(&self, vpn: VirtPageNum) -> Option<PhysPageNum>;
    /// Return the physical token to current page.
    fn token(&self) -> usize;
    fn revoke_read(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn revoke_write(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn revoke_execute(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()>;
    fn set_pte_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()>;
    fn clear_access_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn new() -> Self;
    #[inline(always)]
    fn new_kern_space() -> Self
    where
        Self: Sized,
    {
        Self::new()
    }
    /// Create an empty page table from `satp`
    /// # Argument
    /// * `satp` Supervisor Address Translation & Protection reg. that points to the physical page containing the root page.
    fn from_token(satp: usize) -> Self;
    /// Predicate for the valid bit.
    fn is_mapped(&mut self, vpn: VirtPageNum) -> bool;
    fn activate(&self);
    fn is_valid(&self, vpn: VirtPageNum) -> Option<bool>;
    fn is_dirty(&self, vpn: VirtPageNum) -> Option<bool>;
    fn readable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn writable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn executable(&self, vpn: VirtPageNum) -> Option<bool>;
    // 只看 PTE 用户位和读写位
    fn user_access_ok(&self, vpn: VirtPageNum, access: UserAccess) -> Option<bool>;
}

#[allow(unused)]
pub fn gen_start_end(start: VirtAddr, end: VirtAddr) -> (VirtPageNum, VirtPageNum) {
    (start.floor(), end.ceil())
}

// 只查用户地址范围和溢出
pub fn check_user_range(ptr: usize, len: usize) -> Result<usize, isize> {
    if len == 0 {
        return Ok(ptr);
    }
    let end = ptr
        .checked_add(len)
        .ok_or(crate::syscall::errno::EFAULT)?;
    if ptr >= crate::hal::config::USER_VA_END || end > crate::hal::config::USER_VA_END {
        return Err(crate::syscall::errno::EFAULT);
    }
    Ok(end)
}

fn is_current_user_token(token: usize) -> bool {
    crate::task::current_task()
        .map(|task| task.get_user_token() == token)
        .unwrap_or(false)
}

fn handle_user_page_fault(token: usize, va: VirtAddr, access: UserAccess) -> Result<(), isize> {
    // 只有当前任务能补 lazy/COW
    if !is_current_user_token(token) {
        return Err(crate::syscall::errno::EFAULT);
    }
    // ReadWrite 先按读查再按写查
    match access {
        UserAccess::Read => check_page_fault(va, FaultAccess::Load).map(|_| ()),
        UserAccess::Write => check_page_fault(va, FaultAccess::Store).map(|_| ()),
        UserAccess::ReadWrite => {
            check_page_fault(va, FaultAccess::Load)?;
            check_page_fault(va, FaultAccess::Store).map(|_| ())
        }
    }
}

// 查完用户地址后拿物理地址
pub fn translate_user_va_checked(
    token: usize,
    va: VirtAddr,
    access: UserAccess,
) -> Result<PhysAddr, isize> {
    check_user_range(va.0, 1)?;
    let vpn = va.floor();
    let mut page_table = super::PageTableImpl::from_token(token);
    if page_table.translate_va(va).is_none() {
        handle_user_page_fault(token, va, access)?;
        page_table = super::PageTableImpl::from_token(token);
    }
    let mut ok = page_table.user_access_ok(vpn, access).unwrap_or(false);
    if !ok && access.needs_write() {
        handle_user_page_fault(token, va, access)?;
        page_table = super::PageTableImpl::from_token(token);
        ok = page_table.user_access_ok(vpn, access).unwrap_or(false);
    }
    if !ok {
        return Err(crate::syscall::errno::EFAULT);
    }
    page_table
        .translate_va(va)
        .ok_or(crate::syscall::errno::EFAULT)
}

// 按页拆用户 buffer
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
    let mut start = ptr as usize;
    let end = check_user_range(start, len)?;
    let mut v = Vec::with_capacity(32);
    while start < end {
        let start_va = VirtAddr::from(start);
        let pa = translate_user_va_checked(token, start_va, access)?;
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

// 把拆好的用户 buffer 追加进去
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

// 老工具函数先留着
pub fn ptf_ok(ptf: usize) -> bool {
    ptf & 1 == 1
}

pub fn translated_byte_buffer(
    token: usize,
    ptr: *const u8,
    len: usize,
    access: UserAccess,
) -> Result<Vec<&'static mut [u8]>, isize> {
    translate_user_buffer_checked(token, ptr, len, access)
}

// 老工具函数先留着
pub fn get_right_aligned_bytes<T>(ptr: *const T) -> usize {
    let ptr = ptr as usize;
    let align = core::mem::align_of::<T>();
    let mask = align - 1;
    (align - (ptr & mask)) & mask
}

// 从用户空间读 C 字符串
pub fn translated_str(token: usize, ptr: *const u8) -> Result<String, isize> {
    let mut string = String::new();
    let mut cur = ptr as usize;
    let max_len = MAX_BUFFER_SIZE;
    loop {
        if string.len() >= max_len {
            return Err(crate::syscall::errno::EFAULT);
        }
        let ch: u8 = *({
            let va = VirtAddr::from(cur);
            let pa = translate_user_va_checked(token, va, UserAccess::Read)?;
            pa.get_ref()
        });
        if ch == 0 {
            break;
        }
        string.push(ch as char);
        cur = cur
            .checked_add(1)
            .ok_or(crate::syscall::errno::EFAULT)?;
    }
    Ok(string)
}

// 用户对象只读
pub fn translated_ref<T>(token: usize, ptr: *const T) -> Result<&'static T, isize> {
    let size = core::mem::size_of::<T>();
    let va = VirtAddr::from(ptr as usize);
    check_user_range(va.0, size)?;
    if size > 0 {
        let last = VirtAddr::from(
            va.0.checked_add(size - 1)
                .ok_or(crate::syscall::errno::EFAULT)?,
        );
        if va.floor() != last.floor() {
            return Err(crate::syscall::errno::EFAULT);
        }
    }
    let pa = translate_user_va_checked(token, va, UserAccess::Read)?;
    Ok(pa.get_ref())
}

// 用户对象读写
pub fn translated_refmut<T>(token: usize, ptr: *mut T) -> Result<&'static mut T, isize> {
    let size = core::mem::size_of::<T>();
    let va = VirtAddr::from(ptr as usize);
    check_user_range(va.0, size)?;
    if size > 0 {
        let last = VirtAddr::from(
            va.0.checked_add(size - 1)
                .ok_or(crate::syscall::errno::EFAULT)?,
        );
        if va.floor() != last.floor() {
            return Err(crate::syscall::errno::EFAULT);
        }
    }
    let pa = translate_user_va_checked(token, va, UserAccess::ReadWrite)?;
    Ok(pa.get_mut())
}

// 用户对象纯写
pub fn translated_ref_write<T>(token: usize, ptr: *mut T) -> Result<&'static mut T, isize> {
    let size = core::mem::size_of::<T>();
    let va = VirtAddr::from(ptr as usize);
    check_user_range(va.0, size)?;
    if size > 0 {
        let last = VirtAddr::from(
            va.0.checked_add(size - 1)
                .ok_or(crate::syscall::errno::EFAULT)?,
        );
        if va.floor() != last.floor() {
            return Err(crate::syscall::errno::EFAULT);
        }
    }
    let pa = translate_user_va_checked(token, va, UserAccess::Write)?;
    Ok(pa.get_mut())
}

pub struct UserBuffer {
    pub buffers: Vec<&'static mut [u8]>,

    pub len: usize,
}

impl UserBuffer {
    pub fn new(buffers: Vec<&'static mut [u8]>) -> Self {
        Self {
            len: buffers.iter().map(|buffer| buffer.len()).sum(),
            buffers,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn read(&self, dst: &mut [u8]) -> usize {
        let mut start = 0;
        let dst_len = dst.len();
        for buffer in self.buffers.iter() {
            let end = start + buffer.len();
            if end > dst_len {
                dst[start..].copy_from_slice(&buffer[..dst_len - start]);
                return dst_len;
            } else {
                dst[start..end].copy_from_slice(buffer);
            }
            start = end;
        }
        self.len
    }

    pub fn write(&mut self, src: &[u8]) -> usize {
        let mut start = 0;
        let src_len = src.len();
        for buffer in self.buffers.iter_mut() {
            let end = start + buffer.len();
            if end > src_len {
                buffer[..src_len - start].copy_from_slice(&src[start..]);
                return src_len;
            } else {
                buffer.copy_from_slice(&src[start..end]);
            }
            start = end;
        }
        self.len
    }

    pub fn read_at(&self, offset: usize, dst: &mut [u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        let mut read_bytes = 0usize;
        let mut dst_start = 0usize;
        for buffer in self.buffers.iter() {
            let dst_end = dst_start + buffer.len();
            //we can image mapping 'dst' categories to 'src' categories
            //then we just need to intersect two intervals to get the corresponding interval
            let copy_dst_start = dst_start.max(offset);
            //we may worry about overflow,
            //but we can guarantee that offset(we have checked before) and
            //dst.len()(because of limited memory) won't be too large
            let copy_dst_end = dst_end.min(dst.len() + offset);
            if copy_dst_start >= copy_dst_end {
                dst_start = dst_end; //don't forget to update dst_start
                continue;
            }
            //mapping 'dst' categories to 'src' categories
            let copy_src_start = copy_dst_start - offset;
            let copy_src_end = copy_dst_end - offset;
            //mapping 'dst' categories to 'buffer' categories
            let copy_buffer_start = copy_dst_start - dst_start;
            let copy_buffer_end = copy_dst_end - dst_start;
            dst[copy_src_start..copy_src_end]
                .copy_from_slice(&buffer[copy_buffer_start..copy_buffer_end]);
            read_bytes += copy_dst_end - copy_dst_start;
            dst_start = dst_end; //don't forget to update dst_start
        }
        read_bytes
    }
    pub fn write_at(&mut self, offset: usize, src: &[u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        let mut write_bytes = 0usize;
        let mut dst_start = 0usize;
        for buffer in self.buffers.iter_mut() {
            let dst_end = dst_start + buffer.len();
            //we can image mapping 'src' categories to 'dst' categories
            //then we just need to intersect two intervals to get the corresponding interval
            let copy_dst_start = dst_start.max(offset);
            //we may worry about overflow,
            //but we can guarantee that offset(we have checked before) and
            //src.len()(because of limited memory) won't be too large
            let copy_dst_end = dst_end.min(src.len() + offset);
            if copy_dst_start >= copy_dst_end {
                dst_start = dst_end; //don't forget to update dst_start
                continue;
            }
            //mapping 'dst' categories to 'src' categories
            let copy_src_start = copy_dst_start - offset;
            let copy_src_end = copy_dst_end - offset;
            //mapping 'dst' categories to 'buffer' categories
            let copy_buffer_start = copy_dst_start - dst_start;
            let copy_buffer_end = copy_dst_end - dst_start;
            buffer[copy_buffer_start..copy_buffer_end]
                .copy_from_slice(&src[copy_src_start..copy_src_end]);
            write_bytes += copy_dst_end - copy_dst_start;
            dst_start = dst_end; //don't forget to update dst_start
        }
        write_bytes
    }

    pub fn clear(&mut self) {
        self.buffers.iter_mut().for_each(|buffer| {
            buffer.fill(0);
        })
    }
}

//There may be better implementations here to cover more types
impl core::ops::Index<usize> for UserBuffer {
    type Output = u8;

    fn index(&self, index: usize) -> &Self::Output {
        assert!((index as usize) < self.len);
        let mut left = index;
        for buffer in &self.buffers {
            if (left as usize) < buffer.len() {
                return &buffer[left];
            } else {
                left -= buffer.len();
            }
        }
        unreachable!();
    }
}
impl IndexMut<usize> for UserBuffer {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        assert!((index as usize) < self.len);
        let mut left = index;
        for buffer in &mut self.buffers {
            if (left as usize) < buffer.len() {
                return &mut buffer[left];
            } else {
                left -= buffer.len();
            }
        }
        unreachable!();
    }
}

impl IntoIterator for UserBuffer {
    type Item = *mut u8;
    type IntoIter = UserBufferIterator;
    fn into_iter(self) -> Self::IntoIter {
        UserBufferIterator {
            buffers: self.buffers,
            current_buffer: 0,
            current_idx: 0,
        }
    }
}

/// Iterator to a UserBuffer returning u8
pub struct UserBufferIterator {
    buffers: Vec<&'static mut [u8]>,
    current_buffer: usize,
    current_idx: usize,
}

impl Iterator for UserBufferIterator {
    type Item = *mut u8;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current_buffer >= self.buffers.len() {
            None
        } else {
            let r = &mut self.buffers[self.current_buffer][self.current_idx] as *mut _;
            if self.current_idx + 1 == self.buffers[self.current_buffer].len() {
                self.current_idx = 0;
                self.current_buffer += 1;
            } else {
                self.current_idx += 1;
            }
            Some(r)
        }
    }
}
pub fn get_add_one<T: StepByOne>(ptr: *const T) {
    //TODO!
}
/// Copy `*src: T` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
pub fn copy_from_user<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>();
    if size == 0 {
        return Ok(());
    }
    UserBuffer::new(translated_byte_buffer(
        token,
        src as *const u8,
        size,
        UserAccess::Read,
    )?)
    .read(unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, size) });
    Ok(())
}
// pub fn copy_right_aligned<T: Copy>(token: usize, src: *const T, dst: *mut T) -> Result<(), isize> {
// 	let size = core::mem::size_of::<T>();
// 	let right_aligned_bytes = get_right_aligned_bytes(src);
// 	if right_aligned_bytes == 0 {
// 		copy_from_user(token, src, dst)?;
// 	} else {
// 		let mut buffer = translated_byte_buffer(token, src as *const u8, size + right_aligned_bytes)?;
// 		let buffer = &mut buffer[0];
// 		unsafe {
// 			core::ptr::copy_nonoverlapping(buffer.as_ptr(), dst as *mut u8, size);
// 		}
// 	}
// 	Ok(())
// }
/// Copy array `*src: [T;len]` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
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
    UserBuffer::new(translated_byte_buffer(
        token,
        src as *const u8,
        size,
        UserAccess::Read,
    )?)
    .read(unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, size) });
    Ok(())
}

/// Copy `*src: T` to user space.
/// `src` is a pointer in kernel space, `dst` is a pointer in user space.
pub fn copy_to_user<T: 'static + Copy>(
    token: usize,
    src: *const T,
    dst: *mut T,
) -> Result<(), isize> {
    let size = core::mem::size_of::<T>();
    if size == 0 {
        return Ok(());
    }
    UserBuffer::new(translated_byte_buffer(
        token,
        dst as *const u8,
        size,
        UserAccess::Write,
    )?)
    .write(unsafe { core::slice::from_raw_parts(src as *const u8, size) });
    Ok(())
}

/// Copy `*src: T` to kernel space.
/// `src` is a pointer in user space, `dst` is a pointer in kernel space.
#[inline(always)]
pub fn get_from_user<T: 'static + Copy>(token: usize, src: *const T) -> Result<T, isize> {
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
    UserBuffer::new(translated_byte_buffer(
        token,
        dst as *const u8,
        size,
        UserAccess::Write,
    )?)
    .write(unsafe { core::slice::from_raw_parts(src as *const u8, size) });
    Ok(())
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
    let mut user_buf = UserBuffer::new(translated_byte_buffer(
        token,
        dst as *const u8,
        size,
        UserAccess::Write,
    )?);
    user_buf.write(unsafe { core::slice::from_raw_parts(src.as_ptr(), src.len()) });
    user_buf.write_at(src.len(), b"\0");
    Ok(())
}
