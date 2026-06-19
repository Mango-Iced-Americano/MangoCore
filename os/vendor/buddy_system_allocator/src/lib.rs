#![cfg_attr(feature = "const_fn", feature(const_mut_refs, const_fn_fn_ptr_basics))]
#![no_std]

#[cfg(test)]
#[macro_use]
extern crate std;

#[cfg(feature = "use_spin")]
extern crate spin;

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cmp::{max, min};
use core::fmt;
use core::mem::size_of;
#[cfg(feature = "use_spin")]
use core::ops::Deref;
use core::ptr::NonNull;
#[cfg(feature = "use_spin")]
use spin::Mutex;

mod frame;
pub mod linked_list;
#[cfg(test)]
mod test;

pub use frame::*;

/// Optional hook called by Heap::dealloc with scan step count.
/// Set by the kernel at init time to record perf statistics.
/// Default is a no-op.
fn noop_dealloc_scan_hook(_steps: usize) {}
pub static mut DEALLOC_SCAN_HOOK: fn(usize) = noop_dealloc_scan_hook;

/// A heap that uses buddy system with configurable order.
///
/// # Usage
///
/// Create a heap and add a memory region to it:
/// ```
/// use buddy_system_allocator::*;
/// # use core::mem::size_of;
/// let mut heap = Heap::<32>::empty();
/// # let space: [usize; 100] = [0; 100];
/// # let begin: usize = space.as_ptr() as usize;
/// # let end: usize = begin + 100 * size_of::<usize>();
/// # let size: usize = 100 * size_of::<usize>();
/// unsafe {
///     heap.init(begin, size);
///     // or
///     heap.add_to_heap(begin, end);
/// }
/// ```
pub struct Heap<const ORDER: usize> {
    // buddy system with max order of `ORDER`
    free_list: [linked_list::LinkedList; ORDER],

    // statistics
    user: usize,
    allocated: usize,
    total: usize,

    // base address of the full allocated region (start of bitmaps)
    start: usize,

    // heap data region boundaries (after bitmap carving)
    // These define the managed region: [heap_start, heap_end)
    heap_start: usize,
    heap_end: usize,

    // per-class free-membership bitmap pointers
    // Bit i for block at addr in class c is set when the block is in free_list[c].
    // Block index = (addr - heap_start) >> c.
    free_bits: [*mut usize; ORDER],
}

// SAFETY: Heap is always accessed under a Mutex (see LockedHeap).
// The raw pointers in free_bits point into the static heap region and are
// valid for the lifetime of the kernel.
unsafe impl<const ORDER: usize> Send for Heap<ORDER> {}

impl<const ORDER: usize> Heap<ORDER> {
    /// Create an empty heap
    pub const fn new() -> Self {
        Heap {
            free_list: [linked_list::LinkedList::new(); ORDER],
            user: 0,
            allocated: 0,
            total: 0,
            start: 0,
            heap_start: 0,
            heap_end: 0,
            free_bits: [core::ptr::null_mut::<usize>(); ORDER],
        }
    }

    /// Create an empty heap
    pub const fn empty() -> Self {
        Self::new()
    }

    /// Bits per word for bitmap indexing
    const BITS_PER_WORD: usize = 8 * core::mem::size_of::<usize>();

    /// Set bitmap bit for block at `addr` in class `c`
    fn bitmap_set(&mut self, c: usize, addr: usize) {
        if self.free_bits[c].is_null() {
            return;
        }
        if addr < self.heap_start || addr >= self.heap_end {
            return;
        }
        let idx = (addr - self.heap_start) >> c;
        let block_count = (self.heap_end - self.heap_start) >> c;
        if idx >= block_count {
            return;
        }
        let word = idx / Self::BITS_PER_WORD;
        let bit = idx % Self::BITS_PER_WORD;
        unsafe { *self.free_bits[c].add(word) |= 1usize << bit; }
    }

    /// Clear bitmap bit for block at `addr` in class `c`
    fn bitmap_clear(&mut self, c: usize, addr: usize) {
        if self.free_bits[c].is_null() {
            return;
        }
        if addr < self.heap_start || addr >= self.heap_end {
            return;
        }
        let idx = (addr - self.heap_start) >> c;
        let block_count = (self.heap_end - self.heap_start) >> c;
        if idx >= block_count {
            return;
        }
        let word = idx / Self::BITS_PER_WORD;
        let bit = idx % Self::BITS_PER_WORD;
        unsafe { *self.free_bits[c].add(word) &= !(1usize << bit); }
    }

    /// Check if block at `addr` is in the free list for class `c`
    fn bitmap_test(&self, c: usize, addr: usize) -> bool {
        if self.free_bits[c].is_null() {
            return false;
        }
        if addr < self.heap_start || addr >= self.heap_end {
            return false;
        }
        let idx = (addr - self.heap_start) >> c;
        let block_count = (self.heap_end - self.heap_start) >> c;
        if idx >= block_count {
            return false;
        }
        let word = idx / Self::BITS_PER_WORD;
        let bit = idx % Self::BITS_PER_WORD;
        unsafe { (*self.free_bits[c].add(word)) & (1usize << bit) != 0 }
    }

    /// Add a range of memory [start, end) to the heap
    pub unsafe fn add_to_heap(&mut self, mut start: usize, mut end: usize) {
        // avoid unaligned access on some platforms
        start = (start + size_of::<usize>() - 1) & (!size_of::<usize>() + 1);
        end = end & (!size_of::<usize>() + 1);
        assert!(start <= end);

        let mut total = 0;
        let mut current_start = start;

        while current_start + size_of::<usize>() <= end {
            let lowbit = current_start & (!current_start + 1);
            let size = min(lowbit, prev_power_of_two(end - current_start));
            total += size;

            let class = size.trailing_zeros() as usize;
            self.free_list[class].push(current_start as *mut usize);
            self.bitmap_set(class, current_start);
            current_start += size;
        }

        self.total += total;
    }

    /// Initialize the heap with a memory region, carving bitmap storage from its beginning.
    pub unsafe fn init(&mut self, mut start: usize, size: usize) {
        // Align start to usize boundary to avoid UB on unaligned bitmap word access
        start = (start + size_of::<usize>() - 1) & !(size_of::<usize>() - 1);
        self.start = start;

        // Compute bitmap memory needed, one per class
        let mut bitmap_offset: usize = 0;
        for c in 0..ORDER {
            let block_count = size >> c;
            let word_count = (block_count + Self::BITS_PER_WORD - 1) / Self::BITS_PER_WORD;
            let word_count = word_count.max(1); // at least 1 word per class
            bitmap_offset += word_count * core::mem::size_of::<usize>();
        }

        // Underflow guard: if bitmap overhead >= size, fall back to no-bitmap mode
        if bitmap_offset >= size {
            self.heap_start = start;
            self.heap_end = start + size;
            return;
        }

        // Now zero bitmap memory and store pointers
        let mut offset: usize = 0;
        for c in 0..ORDER {
            let block_count = size >> c;
            let word_count = (block_count + Self::BITS_PER_WORD - 1) / Self::BITS_PER_WORD;
            let word_count = word_count.max(1);
            let bitmap_addr = start + offset;
            for i in 0..word_count {
                (bitmap_addr as *mut usize).add(i).write(0);
            }
            self.free_bits[c] = bitmap_addr as *mut usize;
            offset += word_count * core::mem::size_of::<usize>();
        }

        let heap_start = start + bitmap_offset;
        let heap_size = size - bitmap_offset;

        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;

        self.add_to_heap(heap_start, heap_start + heap_size);
    }

    /// Alloc a range of memory from the heap satifying `layout` requirements
    pub fn alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, ()> {
        let size = max(
            layout.size().next_power_of_two(),
            max(layout.align(), size_of::<usize>()),
        );
        let class = size.trailing_zeros() as usize;
        for i in class..self.free_list.len() {
            // Find the first non-empty size class
            if !self.free_list[i].is_empty() {
                // Split buffers
                for j in (class + 1..i + 1).rev() {
                    if let Some(block) = self.free_list[j].pop() {
                        let block_addr = block as usize;
                        self.bitmap_clear(j, block_addr);
                        unsafe {
                            let buddy_addr = block_addr + (1 << (j - 1));
                            self.free_list[j - 1]
                                .push(buddy_addr as *mut usize);
                            self.free_list[j - 1].push(block);
                        }
                        self.bitmap_set(j - 1, block_addr);
                        self.bitmap_set(j - 1, block_addr + (1 << (j - 1)));
                    } else {
                        return Err(());
                    }
                }

                let popped = self.free_list[class]
                    .pop()
                    .expect("current block should have free space now");
                self.bitmap_clear(class, popped as usize);
                let result = NonNull::new(popped as *mut u8);
                if let Some(result) = result {
                    self.user += layout.size();
                    self.allocated += size;
                    return Ok(result);
                } else {
                    return Err(());
                }
            }
        }
        Err(())
    }

    /// Dealloc a range of memory from the heap
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let size = max(
            layout.size().next_power_of_two(),
            max(layout.align(), size_of::<usize>()),
        );
        let class = size.trailing_zeros() as usize;

        unsafe {
            // Put back into free list
            let ptr_addr = ptr.as_ptr() as usize;
            self.free_list[class].push(ptr_addr as *mut usize);
            self.bitmap_set(class, ptr_addr);

            // Merge free buddy lists
            let mut current_ptr = ptr_addr;
            let mut current_class = class;
            let mut scan_steps: usize = 0;
            while current_class < self.free_list.len() {
                let buddy = current_ptr ^ (1 << current_class);

                // Fast path: bitmap guard — skip scan if buddy is not free.
                // Only active when bitmaps are initialized; falls back to full
                // scan for the legacy add_to_heap()-without-init() path.
                let bitmap_available = !self.free_bits[current_class].is_null();
                if bitmap_available && !self.bitmap_test(current_class, buddy) {
                    break;
                }

                // Buddy IS free — fall through to existing linear scan
                let mut flag = false;
                for block in self.free_list[current_class].iter_mut() {
                    scan_steps += 1;
                    if block.value() as usize == buddy {
                        block.pop();
                        flag = true;
                        break;
                    }
                }

                // Free buddy found
                if flag {
                    self.bitmap_clear(current_class, buddy);
                    let old_ptr = current_ptr;
                    self.free_list[current_class].pop();
                    self.bitmap_clear(current_class, old_ptr);
                    current_ptr = min(old_ptr, buddy);
                    current_class += 1;
                    self.free_list[current_class].push(current_ptr as *mut usize);
                    self.bitmap_set(current_class, current_ptr);
                } else {
                    // Buddy bit was set but not found in list (shouldn't happen)
                    break;
                }
            }
            unsafe { DEALLOC_SCAN_HOOK(scan_steps); }
        }

        self.user -= layout.size();
        self.allocated -= size;
    }

    /// Return the number of bytes that user requests
    pub fn stats_alloc_user(&self) -> usize {
        self.user
    }

    /// Return the number of bytes that are actually allocated
    pub fn stats_alloc_actual(&self) -> usize {
        self.allocated
    }

    /// Return the total number of bytes in the heap
    pub fn stats_total_bytes(&self) -> usize {
        self.total
    }

    /// Return number of free blocks per order (for heap fragmentation diagnosis)
    pub fn free_block_counts(&self) -> [usize; ORDER] {
        let mut counts = [0usize; ORDER];
        for i in 0..ORDER {
            counts[i] = self.free_list[i].iter().count();
        }
        counts
    }
}

impl<const ORDER: usize> fmt::Debug for Heap<ORDER> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Heap")
            .field("user", &self.user)
            .field("allocated", &self.allocated)
            .field("total", &self.total)
            .finish()
    }
}

/// A locked version of `Heap`
///
/// # Usage
///
/// Create a locked heap and add a memory region to it:
/// ```
/// use buddy_system_allocator::*;
/// # use core::mem::size_of;
/// let mut heap = LockedHeap::<32>::new();
/// # let space: [usize; 100] = [0; 100];
/// # let begin: usize = space.as_ptr() as usize;
/// # let end: usize = begin + 100 * size_of::<usize>();
/// # let size: usize = 100 * size_of::<usize>();
/// unsafe {
///     heap.lock().init(begin, size);
///     // or
///     heap.lock().add_to_heap(begin, end);
/// }
/// ```
#[cfg(feature = "use_spin")]
pub struct LockedHeap<const ORDER: usize>(Mutex<Heap<ORDER>>);

#[cfg(feature = "use_spin")]
impl<const ORDER: usize> LockedHeap<ORDER> {
    /// Creates an empty heap
    pub const fn new() -> Self {
        LockedHeap(Mutex::new(Heap::<ORDER>::new()))
    }

    /// Creates an empty heap
    pub const fn empty() -> Self {
        LockedHeap(Mutex::new(Heap::<ORDER>::new()))
    }
}

#[cfg(feature = "use_spin")]
impl<const ORDER: usize> Deref for LockedHeap<ORDER> {
    type Target = Mutex<Heap<ORDER>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(feature = "use_spin")]
unsafe impl<const ORDER: usize> GlobalAlloc for LockedHeap<ORDER> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0
            .lock()
            .alloc(layout)
            .ok()
            .map_or(0 as *mut u8, |allocation| allocation.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.lock().dealloc(NonNull::new_unchecked(ptr), layout)
    }
}

/// A locked version of `Heap` with rescue before oom
///
/// # Usage
///
/// Create a locked heap:
/// ```
/// use buddy_system_allocator::*;
/// let heap = LockedHeapWithRescue::new(|heap: &mut Heap<32>, layout: &core::alloc::Layout| {});
/// ```
///
/// Before oom, the allocator will try to call rescue function and try for one more time.
#[cfg(feature = "use_spin")]
pub struct LockedHeapWithRescue<const ORDER: usize> {
    inner: Mutex<Heap<ORDER>>,
    rescue: fn(&mut Heap<ORDER>, &Layout),
}

#[cfg(feature = "use_spin")]
impl<const ORDER: usize> LockedHeapWithRescue<ORDER> {
    /// Creates an empty heap
    #[cfg(feature = "const_fn")]
    pub const fn new(rescue: fn(&mut Heap<ORDER>, &Layout)) -> Self {
        LockedHeapWithRescue {
            inner: Mutex::new(Heap::<ORDER>::new()),
            rescue,
        }
    }

    /// Creates an empty heap
    #[cfg(not(feature = "const_fn"))]
    pub fn new(rescue: fn(&mut Heap<ORDER>, &Layout)) -> Self {
        LockedHeapWithRescue {
            inner: Mutex::new(Heap::<ORDER>::new()),
            rescue,
        }
    }
}

#[cfg(feature = "use_spin")]
impl<const ORDER: usize> Deref for LockedHeapWithRescue<ORDER> {
    type Target = Mutex<Heap<ORDER>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(feature = "use_spin")]
unsafe impl<const ORDER: usize> GlobalAlloc for LockedHeapWithRescue<ORDER> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut inner = self.inner.lock();
        match inner.alloc(layout) {
            Ok(allocation) => allocation.as_ptr(),
            Err(_) => {
                (self.rescue)(&mut inner, &layout);
                inner
                    .alloc(layout)
                    .ok()
                    .map_or(0 as *mut u8, |allocation| allocation.as_ptr())
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner
            .lock()
            .dealloc(NonNull::new_unchecked(ptr), layout)
    }
}

pub(crate) fn prev_power_of_two(num: usize) -> usize {
    1 << (8 * (size_of::<usize>()) - num.leading_zeros() as usize - 1)
}
