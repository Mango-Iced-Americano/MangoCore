//! Slab allocator built on MetadataHeap<32,12> page-granular buddy.
//!
//! Size classes: 8, 16, 32, 64, 128, 256, 512, 1024, 2048 bytes.
//! Metadata (SlabPage) lives at the tail of each slab page — no separate allocation.

use core::{
    alloc::Layout,
    mem::{align_of, size_of},
    ptr::NonNull,
};

use buddy_system_allocator::{MetadataHeap, PageOrder, PageRun, AllocError as PageAllocError};

const HEAP_ORDER: usize = 32;
const HEAP_MIN_ORDER: usize = 12;
const PAGE_SHIFT: usize = HEAP_MIN_ORDER;
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;

const SLAB_MIN: usize = 8;
const SLAB_MAX: usize = 2048;
const SLAB_CLASS_COUNT: usize = 9;

const SLAB_CLASSES: [usize; SLAB_CLASS_COUNT] =
    [8, 16, 32, 64, 128, 256, 512, 1024, 2048];

const SLAB_MAGIC: u32 = 0x51AB_5A6B;
const FREE_END: u16 = u16::MAX;

/// 512 bits — enough for 512 objects of size 8 in a 4 KiB page.
const SLAB_BITMAP_WORDS: usize = (PAGE_SIZE / SLAB_MIN).div_ceil(usize::BITS as usize);
// (4096/8) / 64 = 512/64 = 8

#[inline]
const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}

/// Determine which size class (index, class_bytes) a layout belongs to.
/// Returns None if the layout is too large or requires page-level alignment.
#[inline]
pub fn slab_class_for(layout: Layout) -> Option<(usize, usize)> {
    let need = layout
        .size()
        .max(layout.align())
        .max(size_of::<usize>())
        .next_power_of_two();

    if need <= SLAB_MAX {
        let index = need.trailing_zeros() as usize - SLAB_MIN.trailing_zeros() as usize;
        Some((index, need))
    } else {
        None
    }
}

/// Charge (bytes) for a direct (non-slab) allocation.
#[inline]
pub fn direct_charge(layout: Layout) -> usize {
    layout
        .size()
        .max(layout.align())
        .max(PAGE_SIZE)
        .next_power_of_two()
}

// ---- PageAllocator trait ----

pub trait PageAllocator {
    fn alloc_pages(&mut self, order: PageOrder) -> Result<PageRun, PageAllocError>;
    unsafe fn dealloc_pages(&mut self, run: PageRun);
}

// ---- SlabPage ----

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlabListId {
    Detached = 0,
    Empty = 1,
    Partial = 2,
    Full = 3,
}

#[repr(C)]
pub struct SlabPage {
    magic: u32,
    cache_index: u8,
    list_id: SlabListId,
    order: u8,
    _pad0: u8,
    size_class: u16,
    inuse: u16,
    max_objects: u16,
    freelist_head: u16,
    prev: Option<NonNull<SlabPage>>,
    next: Option<NonNull<SlabPage>>,
    bitmap: [usize; SLAB_BITMAP_WORDS],
}

// Safety: All types below are only accessed behind the global heap mutex
// on a single-core kernel. The raw-pointer fields in SlabPage (prev/next)
// do not break Send/Sync because access is serialised by the lock.
unsafe impl Send for SlabPage {}
unsafe impl Sync for SlabPage {}
unsafe impl Send for SlabList {}
unsafe impl Sync for SlabList {}
unsafe impl Send for SlabCache {}
unsafe impl Sync for SlabCache {}
unsafe impl Send for SlabAllocator {}
unsafe impl Sync for SlabAllocator {}

impl SlabPage {
    /// Offset from page start where the SlabPage metadata lives.
    const fn meta_offset() -> usize {
        align_down(PAGE_SIZE - size_of::<SlabPage>(), align_of::<SlabPage>())
    }

    /// Get a pointer to the SlabPage header from a page base address.
    unsafe fn header_from_base(base: usize) -> NonNull<SlabPage> {
        NonNull::new_unchecked((base + Self::meta_offset()) as *mut SlabPage)
    }

    /// Get a pointer to the SlabPage header from an object pointer.
    pub unsafe fn from_object(ptr: NonNull<u8>) -> NonNull<SlabPage> {
        let base = align_down(ptr.as_ptr() as usize, PAGE_SIZE);
        Self::header_from_base(base)
    }

    /// Get the base address of the page this SlabPage is in.
    fn page_base(&self) -> usize {
        align_down(self as *const _ as usize, PAGE_SIZE)
    }

    /// Get a pointer to the object at the given index.
    unsafe fn object_ptr(&self, index: u16) -> *mut u8 {
        (self.page_base() + index as usize * self.size_class as usize) as *mut u8
    }

    /// Read the next-free index from a freed object's first 2 bytes.
    unsafe fn read_next_free(obj: *mut u8) -> u16 {
        (obj as *const u16).read()
    }

    /// Write the next-free index into a freed object's first 2 bytes.
    unsafe fn write_next_free(obj: *mut u8, next: u16) {
        (obj as *mut u16).write(next);
    }

    /// Initialize a slab page from a freshly allocated buddy page.
    ///
    /// Writes SlabPage metadata at the page tail and populates the freelist.
    ///
    /// # Safety
    ///
    /// `run` must be a freshly allocated page that has not been given to anyone else.
    /// The returned NonNull<SlabPage> points within that page.
    pub unsafe fn init_at(
        run: PageRun,
        cache_index: usize,
        size_class: usize,
    ) -> NonNull<SlabPage> {
        let base = run.base.as_ptr() as usize;
        let header = Self::header_from_base(base);

        // Calculate max objects
        let meta_off = Self::meta_offset();
        let max_objects = (meta_off / size_class) as u16;
        debug_assert!(max_objects > 0, "slab page too small for any object");

        // Write SlabPage metadata
        let sp = header.as_ptr();
        unsafe {
            sp.write(SlabPage {
                magic: SLAB_MAGIC,
                cache_index: cache_index as u8,
                list_id: SlabListId::Detached,
                order: run.order.0,
                _pad0: 0,
                size_class: size_class as u16,
                inuse: 0,
                max_objects,
                freelist_head: 0,
                prev: None,
                next: None,
                bitmap: [0; SLAB_BITMAP_WORDS],
            });
        }

        // Populate freelist: chain objects 0 -> 1 -> 2 -> ... -> max_objects-1 -> FREE_END
        for i in 0..max_objects {
            let obj = unsafe { (*sp).object_ptr(i) };
            let next = if i + 1 < max_objects { i + 1 } else { FREE_END };
            unsafe { Self::write_next_free(obj, next) };
        }

        header
    }

    // ---- Bitmap operations ----

    fn bitmap_test(&self, index: u16) -> bool {
        let idx = index as usize;
        let word = idx / usize::BITS as usize;
        let bit = idx % usize::BITS as usize;
        (self.bitmap[word] >> bit) & 1 != 0
    }

    fn bitmap_set(&mut self, index: u16) {
        let idx = index as usize;
        let word = idx / usize::BITS as usize;
        let bit = idx % usize::BITS as usize;
        self.bitmap[word] |= 1 << bit;
    }

    fn bitmap_clear(&mut self, index: u16) {
        let idx = index as usize;
        let word = idx / usize::BITS as usize;
        let bit = idx % usize::BITS as usize;
        self.bitmap[word] &= !(1 << bit);
    }

    /// Pop an object from the freelist. Returns the object index.
    unsafe fn pop_free(&mut self) -> Option<u16> {
        let head = self.freelist_head;
        if head == FREE_END {
            return None;
        }
        // Read next free from the freed object
        let obj = unsafe { self.object_ptr(head) };
        let next = unsafe { Self::read_next_free(obj) };
        self.freelist_head = next;
        Some(head)
    }

    /// Push an object back onto the freelist.
    unsafe fn push_free(&mut self, index: u16) {
        let obj = unsafe { self.object_ptr(index) };
        unsafe { Self::write_next_free(obj, self.freelist_head) };
        self.freelist_head = index;
    }
}

// ---- SlabList ----

struct SlabList {
    head: Option<NonNull<SlabPage>>,
    len: usize,
}

impl SlabList {
    pub const fn new() -> Self {
        Self { head: None, len: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    unsafe fn push_front(&mut self, mut page: NonNull<SlabPage>, id: SlabListId) {
        unsafe {
            (*page.as_ptr()).list_id = id;
            (*page.as_ptr()).prev = None;
            (*page.as_ptr()).next = self.head;
        }
        if let Some(mut old_head) = self.head {
            unsafe { (*old_head.as_ptr()).prev = Some(page) };
        }
        self.head = Some(page);
        self.len += 1;
    }

    unsafe fn remove(&mut self, mut page: NonNull<SlabPage>, id: SlabListId) {
        let p = unsafe { &mut *page.as_ptr() };
        p.list_id = SlabListId::Detached;
        match p.prev {
            Some(mut prev) => unsafe { (*prev.as_ptr()).next = p.next },
            None => self.head = p.next,
        }
        if let Some(mut n) = p.next {
            unsafe { (*n.as_ptr()).prev = p.prev };
        }
        p.prev = None;
        p.next = None;
        self.len -= 1;
    }

    unsafe fn pop_front(&mut self, id: SlabListId) -> Option<NonNull<SlabPage>> {
        let page = self.head?;
        unsafe {
            (*page.as_ptr()).list_id = SlabListId::Detached;
        }
        self.head = unsafe { (*page.as_ptr()).next };
        if let Some(mut new_head) = self.head {
            unsafe { (*new_head.as_ptr()).prev = None };
        }
        unsafe {
            (*page.as_ptr()).next = None;
        }
        self.len -= 1;
        Some(page)
    }
}

// ---- SlabCache ----

/// Per-size-class slab cache.
///
/// Manages three lists of pages: empty, partial, and full.
pub struct SlabCache {
    size_class: usize,
    max_objects: u16,
    partial: SlabList,
    empty: SlabList,
    full: SlabList,
    /// Total in-use object count across all pages.
    inuse_total: usize,
}

impl SlabCache {
    pub const fn new(class_idx: usize) -> Self {
        let size_class = SLAB_CLASSES[class_idx];
        // max_objects computed later when a page is first initialized
        let meta_off = SlabPage::meta_offset();
        let max_objects = (meta_off / size_class) as u16;
        Self {
            size_class,
            max_objects,
            partial: SlabList::new(),
            empty: SlabList::new(),
            full: SlabList::new(),
            inuse_total: 0,
        }
    }

    /// Allocate one object from this cache.
    ///
    /// Returns (ptr, is_new_page) — is_new_page is true if a new page was allocated.
    unsafe fn alloc(
        &mut self,
        heap: &mut impl PageAllocator,
        cache_index: usize,
    ) -> Option<(NonNull<u8>, bool)> {
        // 1. Try partial list first
        if let Some(mut page) = self.partial.pop_front(SlabListId::Partial) {
            let p = unsafe { page.as_mut() };
            let free_idx = unsafe { p.pop_free() }?;
            p.bitmap_set(free_idx);
            p.inuse += 1;
            self.inuse_total += 1;
            let ptr = unsafe { NonNull::new_unchecked(p.object_ptr(free_idx)) };
            if p.inuse >= p.max_objects {
                unsafe { self.full.push_front(page, SlabListId::Full) };
            } else {
                unsafe { self.partial.push_front(page, SlabListId::Partial) };
            }
            return Some((ptr, false));
        }

        // 2. Try empty list
        if let Some(mut page) = self.empty.pop_front(SlabListId::Empty) {
            let p = unsafe { page.as_mut() };
            let free_idx = unsafe { p.pop_free() }?;
            p.bitmap_set(free_idx);
            p.inuse += 1;
            self.inuse_total += 1;
            let ptr = unsafe { NonNull::new_unchecked(p.object_ptr(free_idx)) };
            if p.inuse >= p.max_objects {
                unsafe { self.full.push_front(page, SlabListId::Full) };
            } else {
                unsafe { self.partial.push_front(page, SlabListId::Partial) };
            }
            return Some((ptr, false));
        }

        // 3. Grow: allocate a new page from buddy
        self.grow_slab(heap, cache_index)
    }

    /// Allocate a new page and return the first object.
    unsafe fn grow_slab(
        &mut self,
        heap: &mut impl PageAllocator,
        cache_index: usize,
    ) -> Option<(NonNull<u8>, bool)> {
        let run = heap.alloc_pages(PageOrder(PAGE_SHIFT as u8)).ok()?;
        let mut page = unsafe { SlabPage::init_at(run, cache_index, self.size_class) };
        let p = unsafe { page.as_mut() };
        let free_idx = unsafe { p.pop_free() }?;
        p.bitmap_set(free_idx);
        p.inuse += 1;
        self.inuse_total += 1;
        let ptr = unsafe { NonNull::new_unchecked(p.object_ptr(free_idx)) };
        if p.inuse >= p.max_objects {
            unsafe { self.full.push_front(page, SlabListId::Full) };
        } else {
            unsafe { self.partial.push_front(page, SlabListId::Partial) };
        }
        Some((ptr, true))
    }

    /// Deallocate an object from this cache.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated from this cache.
    unsafe fn dealloc(&mut self, heap: &mut impl PageAllocator, ptr: NonNull<u8>) {
        let mut page = unsafe { SlabPage::from_object(ptr) };
        let p = unsafe { page.as_mut() };
        debug_assert_eq!(p.magic, SLAB_MAGIC, "slab dealloc: bad magic");

        // Compute object index
        let obj_offset = ptr.as_ptr() as usize - p.page_base();
        let index = (obj_offset / self.size_class) as u16;
        debug_assert!(index < p.max_objects, "slab dealloc: object index out of range");
        debug_assert!(p.bitmap_test(index), "slab dealloc: double-free detected");

        p.bitmap_clear(index);
        unsafe { p.push_free(index) };
        p.inuse -= 1;
        self.inuse_total -= 1;

        // Update list membership
        let was_full = p.inuse + 1 >= p.max_objects;
        let is_empty = p.inuse == 0;

        match p.list_id {
            SlabListId::Full => {
                if was_full {
                    unsafe { self.full.remove(page, SlabListId::Full) };
                }
                if is_empty {
                    unsafe { self.empty.push_front(page, SlabListId::Empty) };
                } else {
                    unsafe { self.partial.push_front(page, SlabListId::Partial) };
                }
            }
            SlabListId::Partial => {
                if is_empty {
                    unsafe { self.partial.remove(page, SlabListId::Partial) };
                    unsafe { self.empty.push_front(page, SlabListId::Empty) };
                }
            }
            _ => {
                // Should not happen - object from detached/empty list with inuse > 0
                debug_assert!(false, "slab dealloc: unexpected list_id");
            }
        }

        // Reclaim empty pages if needed
        // (reclaim logic is driven by the allocator layer)
    }

    /// Return one empty page to the buddy allocator if any exist.
    unsafe fn release_one_empty(&mut self, heap: &mut impl PageAllocator) -> bool {
        if let Some(page) = self.empty.pop_front(SlabListId::Empty) {
            let p = unsafe { page.as_ref() };
            debug_assert_eq!(p.inuse, 0, "release_one_empty: page not empty");
            let base = p.page_base();
            let order = PageOrder(p.order);
            unsafe {
                heap.dealloc_pages(PageRun {
                    base: NonNull::new_unchecked(base as *mut u8),
                    order,
                });
            }
            true
        } else {
            false
        }
    }

    /// Trim excess empty pages, keeping at most `limit` empty pages.
    unsafe fn trim_empty_over_limit(
        &mut self,
        heap: &mut impl PageAllocator,
        limit: usize,
    ) -> usize {
        let mut released = 0;
        while self.empty.len > limit {
            if self.release_one_empty(heap) {
                released += 1;
            } else {
                break;
            }
        }
        released
    }

    /// Return the total in-use byte count for this cache.
    pub fn user_bytes(&self) -> usize {
        self.inuse_total * self.size_class
    }

    /// Total number of slab pages in this cache.
    pub fn slab_pages(&self) -> usize {
        self.partial.len + self.empty.len + self.full.len
    }

    /// Total in-use objects.
    pub fn inuse_objects(&self) -> usize {
        self.inuse_total
    }

    #[allow(unused)]
    pub fn empty_pages(&self) -> usize {
        self.empty.len
    }
}

// ---- SlabAllocator ----

pub struct SlabAllocator {
    caches: [SlabCache; SLAB_CLASS_COUNT],
    /// Per-cache slab user bytes for stats.
    /// Equivalent to summing self.caches[].user_bytes().
    slab_user_bytes_total: usize,
}

impl SlabAllocator {
    pub const fn empty() -> Self {
        Self {
            caches: [
                SlabCache::new(0),
                SlabCache::new(1),
                SlabCache::new(2),
                SlabCache::new(3),
                SlabCache::new(4),
                SlabCache::new(5),
                SlabCache::new(6),
                SlabCache::new(7),
                SlabCache::new(8),
            ],
            slab_user_bytes_total: 0,
        }
    }

    /// Initialize the allocator. Currently a no-op (caches are statically initialized).
    pub fn init(&mut self) {
        // All caches are already in valid state from empty()
    }

    /// Try to allocate an object from the slab.
    ///
    /// Returns SlabAllocResult on success, or None if slab cannot serve this request
    /// (e.g., layout too large).
    pub fn alloc(
        &mut self,
        heap: &mut impl PageAllocator,
        layout: Layout,
    ) -> Option<SlabAllocResult> {
        let (index, class_bytes) = slab_class_for(layout)?;
        let cache = &mut self.caches[index];
        unsafe {
            let (ptr, _new_page) = cache.alloc(heap, index)?;
            self.slab_user_bytes_total += class_bytes;
            Some(SlabAllocResult {
                ptr,
                charge: class_bytes,
                cache_index: index,
            })
        }
    }

    /// Deallocate an object from the slab.
    ///
    /// Returns true if the object was slab-allocated and freed, false if not.
    ///
    /// # Safety
    ///
    /// `ptr` and `layout` must match a previous slab allocation.
    pub unsafe fn dealloc(
        &mut self,
        heap: &mut impl PageAllocator,
        ptr: NonNull<u8>,
        layout: Layout,
    ) -> bool {
        let (index, class_bytes) = match slab_class_for(layout) {
            Some(v) => v,
            None => return false,
        };
        let cache = &mut self.caches[index];
        unsafe { cache.dealloc(heap, ptr) };
        self.slab_user_bytes_total = self.slab_user_bytes_total.saturating_sub(class_bytes);
        true
    }

    /// Try to release empty pages back to the buddy allocator.
    pub unsafe fn reclaim_empty(&mut self, heap: &mut impl PageAllocator) -> usize {
        let mut released = 0;
        for cache in &mut self.caches {
            while cache.release_one_empty(heap) {
                released += 1;
            }
        }
        released
    }

    /// Total user bytes tracked by the slab allocator.
    pub fn slab_user_bytes(&self) -> usize {
        self.slab_user_bytes_total
    }

    /// Total slab pages.
    pub fn slab_pages_total(&self) -> usize {
        self.caches.iter().map(|c| c.slab_pages()).sum()
    }

    /// Per-class stats: (size_class, inuse_objects, slab_pages).
    pub fn per_class_stats(&self) -> [(usize, usize, usize); SLAB_CLASS_COUNT] {
        let mut stats = [(0, 0, 0); SLAB_CLASS_COUNT];
        for (i, cache) in self.caches.iter().enumerate() {
            stats[i] = (cache.size_class, cache.inuse_objects(), cache.slab_pages());
        }
        stats
    }
}

// ---- SlabAllocResult ----

/// Result of a successful slab allocation.
pub struct SlabAllocResult {
    /// Pointer to the allocated object.
    pub ptr: NonNull<u8>,
    /// Number of bytes to charge (class_bytes).
    pub charge: usize,
    /// Which slab cache was used.
    pub cache_index: usize,
}
