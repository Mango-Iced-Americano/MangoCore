//! 地址和页号基础类型。
//!
//! 提供物理/虚拟地址、物理/虚拟页号之间的转换，以及通过内核直映区访问物理
//! 内存的辅助方法。
//!
//! # Safety
//!
//! `PhysAddr`/`PhysPageNum` 的引用访问方法假定目标物理内存已经由调用方保证
//! 有效、对齐且不会与其他可变引用别名。

use crate::config::{MEMORY_HIGH_BASE, PAGE_SIZE, PAGE_SIZE_BITS};
use core::fmt::{self, Debug, Formatter};

#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
/// 物理地址
pub struct PhysAddr(pub usize);

#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
/// 虚拟地址
pub struct VirtAddr(pub usize);

#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
/// 物理页号
pub struct PhysPageNum(pub usize);

#[repr(C)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
/// 虚拟页号
pub struct VirtPageNum(pub usize);

/// Debug formatter for VirtAddr
impl Debug for VirtAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VA")
            .field(&format_args!("{:#X}", self.0))
            .finish()
    }
}

impl Debug for VirtPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VPN")
            .field(&format_args!("{:#X}", self.0))
            .finish()
    }
}

impl Debug for PhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PA")
            .field(&format_args!("{:#X}", self.0))
            .finish()
    }
}

impl Debug for PhysPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PPN")
            .field(&format_args!("{:#X}", self.0))
            .finish()
    }
}

// 如下内容实现了上述类型与usize的双向转换
impl From<usize> for PhysAddr {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for PhysPageNum {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for VirtPageNum {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<PhysAddr> for usize {
    fn from(v: PhysAddr) -> Self {
        v.0
    }
}
impl From<PhysPageNum> for usize {
    fn from(v: PhysPageNum) -> Self {
        v.0
    }
}
impl From<VirtAddr> for usize {
    fn from(v: VirtAddr) -> Self {
        v.0
    }
}
impl From<VirtPageNum> for usize {
    fn from(v: VirtPageNum) -> Self {
        v.0
    }
}

impl VirtAddr {
    /// 计算地址所在的页号（向下取整）
    pub fn floor(&self) -> VirtPageNum {
        let a = self.0 / PAGE_SIZE;
        VirtPageNum(a)
    }
    /// 计算地址所在的页号（向上取整）
    pub fn ceil(&self) -> VirtPageNum {
        if self.0 == 0 {
            VirtPageNum(0)
        } else {
            VirtPageNum((self.0 - 1) / PAGE_SIZE + 1)
        }
    }
    /// 计算地址在页内的偏移量
    pub fn page_offset(&self) -> usize {
        {
            let c = PAGE_SIZE - 1;
            self.0 & (c)
        }
    }
    /// 检查地址是否页对齐
    pub fn aligned(&self) -> bool {
        self.page_offset() == 0
    }
}

/// 虚拟地址 转 虚拟页号
impl From<VirtAddr> for VirtPageNum {
    fn from(v: VirtAddr) -> Self {
        assert_eq!(v.page_offset(), 0);
        v.floor()
    }
}

/// 虚拟页号 转 虚拟地址
impl From<VirtPageNum> for VirtAddr {
    fn from(v: VirtPageNum) -> Self {
        let d = v.0 << PAGE_SIZE_BITS;
        Self(d)
    }
}

impl PhysAddr {
    /// 计算地址所在的页号（向下取整）
    pub fn floor(&self) -> PhysPageNum {
        let e = self.0 / PAGE_SIZE;
        PhysPageNum(e)
    }
    /// 计算地址所在的页号（向上取整）
    pub fn ceil(&self) -> PhysPageNum {
        if self.0 == 0 {
            PhysPageNum(0)
        } else {
            PhysPageNum((self.0 - 1) / PAGE_SIZE + 1)
        }
    }
    /// 计算地址在页内的偏移量
    pub fn page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    /// 检查地址是否页对齐
    pub fn aligned(&self) -> bool {
        self.page_offset() == 0
    }
}

/// 物理地址 转 物理页号
impl From<PhysAddr> for PhysPageNum {
    fn from(v: PhysAddr) -> Self {
        assert_eq!(v.page_offset(), 0);
        v.floor()
    }
}

/// 物理页号 转 物理地址
impl From<PhysPageNum> for PhysAddr {
    fn from(v: PhysPageNum) -> Self {
        let g = v.0 << PAGE_SIZE_BITS;
        Self(g)
    }
}

impl VirtPageNum {
    /// 获取页的起始地址
    pub fn start_addr(&self) -> VirtAddr {
        let f = self.0 << PAGE_SIZE_BITS;
        VirtAddr::from(f)
    }
    /// 通过页内偏移量计算地址
    pub fn offset(&self, offset: usize) -> VirtAddr {
        VirtAddr::from((self.0 << PAGE_SIZE_BITS) + offset)
    }
    /// 用于多级页表索引计算
    pub fn indexes<const T: usize>(&self) -> [usize; T] {
        // 获取虚拟页号
        let mut vpn = self.0;
        // 初始化索引数组
        let mut idx = [0usize; T];
        for i in (0..T).rev() {
            // 获取当前级别页表的索引
            idx[i] = vpn & 511;
            // 处理后右移9位
            vpn >>= 9;
        }
        idx
    }
}

impl PhysAddr {
    // 物理地址通过内核直映区访问，具体偏移由架构配置给出。
    #[inline(always)]
    fn direct_map_addr(&self) -> usize {
        self.0 | MEMORY_HIGH_BASE
    }

    /// 通过内核直映区获取 `T` 的共享引用。
    ///
    /// # Safety
    ///
    /// 调用方必须保证物理地址有效、满足 `T` 的对齐要求，且该内存区域在返回
    /// 引用存活期间不会被释放或以可变方式别名访问。
    pub fn get_ref<T>(&self) -> &'static T {
        // Safety: the caller-side contract above ensures the direct-map address
        // is a valid, aligned `T` reference.
        unsafe { (self.direct_map_addr() as *const T).as_ref().unwrap() }
    }

    /// 通过内核直映区获取 `T` 的可变引用。
    ///
    /// # Safety
    ///
    /// 调用方必须独占对应物理内存，并保证地址有效且满足 `T` 的对齐要求。
    pub fn get_mut<T>(&self) -> &'static mut T {
        // Safety: the caller-side contract above ensures exclusive access to a
        // valid, aligned `T` at the direct-map address.
        unsafe { (self.direct_map_addr() as *mut T).as_mut().unwrap() }
    }

    /// 以字节数组形式读取从该物理地址开始的 `size_of::<T>()` 字节。
    pub fn get_bytes_ref<T>(&self) -> &'static [u8] {
        // Safety: bytes have alignment 1; callers guarantee the physical range
        // is valid for `size_of::<T>()` bytes.
        unsafe {
            core::slice::from_raw_parts(
                self.direct_map_addr() as *const u8,
                core::mem::size_of::<T>(),
            )
        }
    }
    /// 以字节数组形式写入从该物理地址开始的 `size_of::<T>()` 字节。
    ///
    /// # Safety
    ///
    /// 调用方必须独占对应物理字节范围。
    pub fn get_bytes_mut<T>(&self) -> &'static mut [u8] {
        // Safety: bytes have alignment 1; callers guarantee exclusive access to
        // a valid physical range for `size_of::<T>()` bytes.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.direct_map_addr() as *mut u8,
                core::mem::size_of::<T>(),
            )
        }
    }
}

impl PhysPageNum {
    /// 获取页的起始地址
    pub fn start_addr(&self) -> PhysAddr {
        PhysAddr::from(self.0 << PAGE_SIZE_BITS)
    }
    /// 通过页内偏移量计算地址
    pub fn offset(&self, offset: usize) -> PhysAddr {
        PhysAddr::from((self.0 << PAGE_SIZE_BITS) + offset)
    }
    /// 将整页解释为页表项数组。
    ///
    /// # Safety
    ///
    /// 调用方必须保证该物理页确实保存 `T` 类型页表项，且当前路径独占该页表页。
    pub fn get_pte_array<T>(&self) -> &'static mut [T] {
        let pa: PhysAddr = self.clone().into();
        let entry_size = core::mem::size_of::<T>();
        assert!(entry_size != 0, "page table entry must not be zero-sized");
        // Safety: callers guarantee this physical page stores page-table entries
        // of type `T`; the length is derived from page size and entry size.
        unsafe {
            core::slice::from_raw_parts_mut(pa.direct_map_addr() as *mut T, PAGE_SIZE / entry_size)
        }
    }

    /// 获取整个物理页的可变字节视图。
    ///
    /// # Safety
    ///
    /// 调用方必须独占该物理页。
    pub fn get_bytes_array(&self) -> &'static mut [u8] {
        let pa: PhysAddr = self.clone().into();
        // Safety: callers guarantee exclusive access to this whole physical
        // page through the direct map.
        unsafe { core::slice::from_raw_parts_mut(pa.direct_map_addr() as *mut u8, PAGE_SIZE) }
    }

    /// 将整页解释为 `u64` 数组。
    ///
    /// # Safety
    ///
    /// 调用方必须保证该页按 `u64` 对齐并独占访问。
    pub fn get_dwords_array(&self) -> &'static mut [u64] {
        let pa: PhysAddr = self.clone().into();
        // Safety: callers guarantee the page is valid, u64-aligned, and
        // exclusively accessed for the returned lifetime.
        unsafe {
            core::slice::from_raw_parts_mut(
                pa.direct_map_addr() as *mut u64,
                PAGE_SIZE / core::mem::size_of::<u64>(),
            )
        }
    }
    /// 获取指定类型的可变引用
    pub fn get_mut<T>(&self) -> &'static mut T {
        let pa: PhysAddr = self.clone().into();
        pa.get_mut()
    }
}

/// 范围迭代器
/// 提供单步递增能力
pub trait StepByOne {
    fn step(&mut self);
}
impl StepByOne for VirtPageNum {
    fn step(&mut self) {
        self.0 += 1;
    }
}
impl StepByOne for PhysPageNum {
    fn step(&mut self) {
        self.0 += 1;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
/// 表示一个范围区间
pub struct SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    l: T,
    r: T,
}
impl<T> SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(start: T, end: T) -> Self {
        assert!(start <= end, "start {:?} > end {:?}!", start, end);
        Self { l: start, r: end }
    }
    pub fn get_start(&self) -> T {
        self.l
    }
    pub fn get_end(&self) -> T {
        self.r
    }
}
impl<T> IntoIterator for SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug + From<usize>,
{
    type Item = T;
    type IntoIter = SimpleRangeIterator<T>;
    fn into_iter(self) -> Self::IntoIter {
        SimpleRangeIterator::new(self.l, self.r)
    }
}
pub struct SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    current: T,
    end: T,
}
impl<T> SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(l: T, r: T) -> Self {
        Self { current: l, end: r }
    }
}
impl<T> Iterator for SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.end {
            None
        } else {
            let t = self.current;
            self.current.step();
            Some(t)
        }
    }
}
pub type VPNRange = SimpleRange<VirtPageNum>;
pub type PPNRange = SimpleRange<PhysPageNum>;
