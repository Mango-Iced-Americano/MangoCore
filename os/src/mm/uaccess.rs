use core::{marker::PhantomData, ops::IndexMut};

use crate::fs::iov::IOVec;
use super::page_table::{FaultAccess, PageTable, UserAccess};
use super::{PhysAddr, StepByOne, VirtAddr};
use alloc::string::String;
use alloc::vec::Vec;

// Cap a single user buffer translation to avoid kernel OOM.
const MAX_BUFFER_SIZE: usize = 1024 * 1024 * 8;
const MAX_IOVEC_COUNT: usize = 1024;

#[derive(Clone, Copy)]
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
    pub fn new(token: usize, ptr: *const u8, len: usize) -> Result<Self, isize> {
        Ok(Self {
            buffer: UserBuffer::new(translated_byte_buffer(token, ptr, len, UserAccess::Read)?),
        })
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
    pub fn new(token: usize, ptr: *mut u8, len: usize) -> Result<Self, isize> {
        Ok(Self {
            buffer: UserBuffer::new(translated_byte_buffer(
                token,
                ptr as *const u8,
                len,
                UserAccess::Write,
            )?),
        })
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
}

// Check only user range bounds and arithmetic overflow.
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

fn uaccess_user_range_ok(ptr: usize, end: usize) -> bool {
    // la64 可能传入低地址用户指针；真实合法性由后续页表权限检查决定。
    ptr < crate::config::USER_VA_END && end <= crate::config::USER_VA_END
}

fn is_current_user_token(token: usize) -> bool {
    crate::task::current_task()
        .map(|task| task.get_user_token() == token)
        .unwrap_or(false)
}

// 区分用户触发缺页时的权限
// 例如：copy_from_user - Read; copy_to_user - Write; 获得可变引用-ReadWrite等
// 通过不同权限的区分使得缺页处理更灵活更稳定
fn handle_user_page_fault(token: usize, va: VirtAddr, access: UserAccess) -> Result<(), isize> {
    // page_fault只能由当前任务token处理
    if !is_current_user_token(token) {
        return Err(crate::syscall::errno::EFAULT);
    }
    // ReadWrite first resolves read access, then write access.
    match access {
        UserAccess::Read => super::address_space::check_page_fault(va, FaultAccess::Load).map(|_| ()),
        UserAccess::Write => {
            super::address_space::check_page_fault(va, FaultAccess::Store).map(|_| ())
        }
        UserAccess::ReadWrite => {
            super::address_space::check_page_fault(va, FaultAccess::Load)?;
            super::address_space::check_page_fault(va, FaultAccess::Store).map(|_| ())
        }
    }
}

// 将用户va翻译为pa
pub fn translate_user_va_checked(
    token: usize,
    va: VirtAddr,
    access: UserAccess,
) -> Result<PhysAddr, isize> {
    check_user_range(va.0, 1)?;
    let vpn = va.floor();
    let mut page_table = super::PageTableImpl::from_token(token);
    // pte不存在，可能是lazy alloc、swap、页压缩等情况，做缺页处理
    if page_table.translate_va(va).is_none() {
        handle_user_page_fault(token, va, access)?;
        page_table = super::PageTableImpl::from_token(token);
    }
    let mut ok = page_table.user_access_ok(vpn, access).unwrap_or(false);
    // pte存在但不可写，可能为cow等情况，再进行一次缺页处理
    if !ok && access.needs_write() {
        handle_user_page_fault(token, va, access)?;
        page_table = super::PageTableImpl::from_token(token);
        ok = page_table.user_access_ok(vpn, access).unwrap_or(false);
    }
    // 如果这时候还不ok，是真的权限错误了
    if !ok {
        return Err(crate::syscall::errno::EFAULT);
    }
    page_table
        .translate_va(va)
        .ok_or(crate::syscall::errno::EFAULT)
}

// Split a user buffer by page.
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

// Append translated user buffer slices to an existing vector.
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

// 旧代码遗留函数，暂时不使用
pub fn get_right_aligned_bytes<T>(ptr: *const T) -> usize {
    let ptr = ptr as usize;
    let align = core::mem::align_of::<T>();
    let mask = align - 1;
    (align - (ptr & mask)) & mask
}

// Read a C string from user space.
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
        cur = cur.checked_add(1).ok_or(crate::syscall::errno::EFAULT)?;
    }
    Ok(string)
}

// Read-only user object reference.
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

// Read-write user object reference.
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

// Write-only user object reference.
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

// 用户缓冲区，可能跨页
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
        let copy_limit = dst.len().saturating_add(offset);
        for buffer in self.buffers.iter() {
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
            dst[copy_src_start..copy_src_end]
                .copy_from_slice(&buffer[copy_buffer_start..copy_buffer_end]);
            read_bytes += copy_dst_end - copy_dst_start;
            dst_start = dst_end;
        }
        read_bytes
    }

    pub fn write_at(&mut self, offset: usize, src: &[u8]) -> usize {
        if offset >= self.len {
            return 0;
        }
        let mut write_bytes = 0usize;
        let mut dst_start = 0usize;
        let copy_limit = src.len().saturating_add(offset);
        for buffer in self.buffers.iter_mut() {
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
            buffer[copy_buffer_start..copy_buffer_end]
                .copy_from_slice(&src[copy_src_start..copy_src_end]);
            write_bytes += copy_dst_end - copy_dst_start;
            dst_start = dst_end;
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
        while self.current_buffer < self.buffers.len()
            && self.buffers[self.current_buffer].is_empty()
        {
            self.current_buffer += 1;
            self.current_idx = 0;
        }
        if self.current_buffer >= self.buffers.len() {
            return None;
        }
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
