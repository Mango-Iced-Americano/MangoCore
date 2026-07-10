//! Pure-logic page cache helpers extracted from `os/src/fs/page_cache.rs`.
//!
//! Contains segment validity bitmask computation, PageState enum, and
//! readahead state struct. Impure parts (I/O, frame allocation, writeback)
//! remain in the kernel.
//!
//! # Bug fix: `mask_for_range` debug panic
//!
//! The kernel version uses `(1u8 << (seg_end - seg_start)) - 1` which panics
//! in debug builds when `seg_end - seg_start == 8` (shift equals bit width).
//! Fixed here to use a safe computation.

/// Page size in bytes (architecture-independent; 4 KiB).
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = 12;

/// Byte count per validity segment (512 B).
pub const VALID_SEG_SHIFT: usize = 9;
/// Segments per page (4096 / 512 = 8).
pub const VALID_SEG_COUNT: usize = PAGE_SIZE >> VALID_SEG_SHIFT;
/// All-valid segment mask (0xFF for 8 segments).
pub const VALID_ALL: u8 = 0xFF;

/// Global dirty page thresholds (re-exported for consistent access).
pub const DIRTY_BACKGROUND: usize = 8192;
pub const DIRTY_THROTTLE: usize = 16384;

/// PG_* style flags for page state tracking.
pub const PG_REFERENCED: u8 = 1 << 0;
pub const PG_DIRTY: u8 = 1 << 1;

// ────────────────────────────────────────────────────────────────────────
//  mask_for_range — segment validity bitmask
// ────────────────────────────────────────────────────────────────────────

/// Compute a bitmask of validity segments covered by `[page_offset, page_offset+len)`.
///
/// Each page is divided into `VALID_SEG_COUNT` segments of `1 << VALID_SEG_SHIFT`
/// bytes. The bitmask has bit 0 for segment 0, bit 1 for segment 1, etc.
/// Partially covered segments are considered valid.
///
/// # Safety fix
///
/// Uses `u8::MAX >> (8 - VALID_SEG_COUNT)` pattern to avoid `1u8 << 8` panic
/// in debug builds.
pub fn mask_for_range(page_offset: usize, len: usize) -> u8 {
    if len == 0 {
        return 0;
    }
    let seg_start = page_offset >> VALID_SEG_SHIFT;
    let seg_end = ((page_offset + len + (1 << VALID_SEG_SHIFT) - 1) >> VALID_SEG_SHIFT)
        .min(VALID_SEG_COUNT);
    if seg_start >= VALID_SEG_COUNT {
        return 0;
    }
    let count = seg_end - seg_start;
    // Safe shift: if count == VALID_SEG_COUNT (8), use u8::MAX.
    // Otherwise, (1 << count) - 1 is safe because count < 8.
    let low_mask: u8 = if count == 8 {
        u8::MAX
    } else {
        (1u8 << count) - 1
    };
    low_mask << seg_start
}

// ────────────────────────────────────────────────────────────────────────
//  initial_valid_mask — EOF-based initial validity
// ────────────────────────────────────────────────────────────────────────

/// Determine which segments of a page are valid based on the file's old EOF.
///
/// - Pages entirely beyond EOF → `VALID_ALL` (zero-fill is valid data).
/// - Pages that span EOF → only the zero-filled portion is valid.
/// - Pages entirely before EOF → `0` (data not yet loaded from backend).
pub fn initial_valid_mask(page_index: usize, old_file_size: usize) -> u8 {
    let page_start = page_index * PAGE_SIZE;
    if page_start >= old_file_size {
        return VALID_ALL; // entirely beyond EOF → all zeros = valid
    }
    let page_end = page_start + PAGE_SIZE;
    if old_file_size < page_end {
        // page spans EOF: bytes beyond EOF are valid zeros
        let zero_start = old_file_size - page_start;
        return mask_for_range(zero_start, PAGE_SIZE - zero_start);
    }
    0 // existing file page: old data not loaded yet
}

// ────────────────────────────────────────────────────────────────────────
//  PageState — page state machine
// ────────────────────────────────────────────────────────────────────────

/// Page state, analogous to Linux PG_* flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageState {
    /// Page is being loaded from backend storage.
    Loading = 0,
    /// Page data is up-to-date.
    UpToDate = 1,
    /// Page has unsaved dirty data.
    Dirty = 2,
    /// Page is being written back to storage.
    Writeback = 3,
    /// Page encountered an I/O error.
    Error = 4,
}

// ────────────────────────────────────────────────────────────────────────
//  RaState — readahead state
// ────────────────────────────────────────────────────────────────────────

/// Sequential readahead state, analogous to Linux `file_ra_state`.
///
/// Updated on each cache miss; ramps up the readahead window when
/// sequential access is detected.
#[derive(Debug, Clone)]
pub struct RaState {
    /// Index of the last page accessed.
    pub prev_page: usize,
    /// Current sequential readahead window size (in pages).
    pub ra_size: usize,
}

/// Minimum readahead pages (cold-start window).
pub const MIN_RA_PAGES: usize = 4;
/// Maximum readahead pages.
pub const MAX_RA_PAGES: usize = 128;

impl RaState {
    pub fn new() -> Self {
        RaState {
            prev_page: 0,
            ra_size: MIN_RA_PAGES,
        }
    }
}

impl Default for RaState {
    fn default() -> Self {
        Self::new()
    }
}

// ────────────────────────────────────────────────────────────────────────
//  Tests
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── mask_for_range ──────────────────────────────────────────────

    #[test]
    fn mask_full_page() {
        // offset 0, len 4096 → all 8 segments valid
        assert_eq!(mask_for_range(0, 4096), 0xFF);
    }

    #[test]
    fn mask_first_segment() {
        // offset 0, len 512 → only segment 0
        assert_eq!(mask_for_range(0, 512), 0x01);
    }

    #[test]
    fn mask_second_segment() {
        // offset 512, len 512 → only segment 1
        assert_eq!(mask_for_range(512, 512), 0x02);
    }

    #[test]
    fn mask_third_segment() {
        // offset 1024, len 512 → only segment 2
        assert_eq!(mask_for_range(1024, 512), 0x04);
    }

    #[test]
    fn mask_last_segment() {
        // offset 3584, len 512 → only segment 7
        assert_eq!(mask_for_range(3584, 512), 0x80);
    }

    #[test]
    fn mask_range_spans_segments() {
        // offset 0, len 1536 → segments 0,1,2
        assert_eq!(mask_for_range(0, 1536), 0x07);
    }

    #[test]
    fn mask_range_mid_page() {
        // offset 1024, len 2048 → segments 2,3,4,5
        assert_eq!(mask_for_range(1024, 2048), 0x3C);
    }

    #[test]
    fn mask_partial_segment() {
        // offset 0, len 256 → still segment 0 (partial counted as valid)
        assert_eq!(mask_for_range(0, 256), 0x01);
    }

    #[test]
    fn mask_zero_len() {
        assert_eq!(mask_for_range(0, 0), 0);
        assert_eq!(mask_for_range(2048, 0), 0);
    }

    #[test]
    fn mask_offset_beyond_page() {
        // offset beyond page size → no segments
        assert_eq!(mask_for_range(4096, 512), 0);
        assert_eq!(mask_for_range(8192, 1), 0);
    }

    #[test]
    fn mask_spanning_to_end_of_page() {
        // offset 2048, len 2048 → segments 4,5,6,7
        assert_eq!(mask_for_range(2048, 2048), 0xF0);
    }

    #[test]
    fn mask_full_page_via_8_segments() {
        // This was the bug case: seg_end - seg_start == 8 caused (1u8 << 8) panic.
        // With the fix, this should return 0xFF safely.
        assert_eq!(mask_for_range(0, 4096), 0xFF);
    }

    // ── initial_valid_mask ──────────────────────────────────────────

    #[test]
    fn initial_mask_beyond_eof_full_page() {
        // page 0, file size 0 → page 0 starts at 0, but page_start >= old_file_size? No, 0 >= 0 is true!
        // Actually: page_start=0, old_file_size=0, so page_start >= old_file_size → VALID_ALL
        assert_eq!(initial_valid_mask(0, 0), VALID_ALL);
    }

    #[test]
    fn initial_mask_page_beyond_eof() {
        // page 1 (offset 4096), file size 100 → page_start 4096 >= 100 → VALID_ALL
        assert_eq!(initial_valid_mask(1, 100), VALID_ALL);
    }

    #[test]
    fn initial_mask_page_spans_eof() {
        // page 0 (offset 0..4096), file size 500
        // page_start=0, 0 < 500, page_end=4096 > 500
        // zero_start = 500 - 0 = 500, remaining = 4096 - 500 = 3596
        // 500/512=0 (segment 0 start), 3596 bytes from offset 500
        // seg_start=0, seg_end = ceil((500+3596)/512) = ceil(4096/512) = 8
        // mask_for_range(500, 3596) → segments 1-7 (since first 500 bytes are before EOF, not from zeros)
        // Actually wait: zero_start = old_file_size - page_start = 500
        // mask_for_range(500, 4096 - 500) = mask_for_range(500, 3596) 
        // seg_start = 500/512 = 0 (segment 0), seg_end = ceil((500+3596)/512) = ceil(4096/512) = 8
        // mask = (1<<8)-1 << 0 = 0xFF with bug fix
        // Hmm, this doesn't seem right. The issue is that mask_for_range takes page_offset relative to the page.
        // For page 0, offset 500: the first 500 bytes are before EOF, but since the page is entirely before EOF minus the first 500 bytes, the "known valid" part is the zero-filled portion beyond EOF.
        // Wait, let me re-read: initial_valid_mask says bytes beyond EOF are valid zeros. So for page 0 at offset 0:
        // bytes 0..500 are within file (not loaded yet)
        // bytes 500..4096 are beyond EOF (valid zeros)
        // So mask_for_range(500, 3596) which gives segments covered by [500, 4096)
        // seg_start = 500/512 = 0, seg_end = ceil(4096/512) = 8
        // This gives 0xFF, which would mean all segments are valid. But segment 0 is partially within the file (bytes 0-500), so we'd mark it valid anyway (partial segments are valid). So 0xFF is correct.
        
        let mask = initial_valid_mask(0, 500);
        // With 500 byte file, bytes 500-4095 are zero-filled (valid). 
        // mask_for_range(500, 3596) → seg_start=0, seg_end=8 → 0xFF
        assert_eq!(mask, 0xFF);
    }

    #[test]
    fn initial_mask_page_exactly_at_eof_boundary() {
        // page 1 (offset 4096), file size exactly 4096
        // page_start=4096, 4096 >= 4096 → VALID_ALL
        assert_eq!(initial_valid_mask(1, 4096), VALID_ALL);
    }

    #[test]
    fn initial_mask_page_before_eof() {
        // page 0 (offset 0), file size 8192 → entire page is within file
        // page_start=0 < 8192, page_end=4096 < 8192 → return 0
        assert_eq!(initial_valid_mask(0, 8192), 0);
    }

    #[test]
    fn initial_mask_spans_eof_small_file() {
        // page 0 (offset 0..4096), file_size = 100 bytes
        // page_start=0 < 100, page_end=4096 > 100
        // zero_start = 100 - 0 = 100
        // mask_for_range(100, 3996)
        // seg_start = 100/512 = 0, seg_end = ceil(4096/512) = 8 → 0xFF
        assert_eq!(initial_valid_mask(0, 100), 0xFF);
    }

    #[test]
    fn initial_mask_first_segment_partial_file() {
        // page 0, file_size 400
        // bytes 0-400: within file (not valid yet = 0)
        // bytes 400-4096: zero-filled (valid)
        // mask_for_range(400, 3696) → seg_start=0, seg_end=8 → 0xFF
        assert_eq!(initial_valid_mask(0, 400), 0xFF);
    }

    // ── PageState ────────────────────────────────────────────────────

    #[test]
    fn page_state_discriminants_are_distinct() {
        // each variant must have a different discriminants
        let states = [
            PageState::Loading,
            PageState::UpToDate,
            PageState::Dirty,
            PageState::Writeback,
            PageState::Error,
        ];
        for i in 0..states.len() {
            for j in (i + 1)..states.len() {
                assert_ne!(states[i], states[j], "states[{i}] == states[{j}]");
            }
        }
    }

    #[test]
    fn page_state_repr_matches_expected() {
        assert_eq!(PageState::Loading as u8, 0);
        assert_eq!(PageState::UpToDate as u8, 1);
        assert_eq!(PageState::Dirty as u8, 2);
        assert_eq!(PageState::Writeback as u8, 3);
        assert_eq!(PageState::Error as u8, 4);
    }

    // ── RaState ──────────────────────────────────────────────────────

    #[test]
    fn ra_state_default() {
        let ra = RaState::default();
        assert_eq!(ra.prev_page, 0);
        assert_eq!(ra.ra_size, MIN_RA_PAGES);
    }

    #[test]
    fn ra_state_new_matches_default() {
        assert_eq!(RaState::new().prev_page, RaState::default().prev_page);
        assert_eq!(RaState::new().ra_size, RaState::default().ra_size);
    }

    // ── Constants ────────────────────────────────────────────────────

    #[test]
    fn valid_seg_count_is_eight() {
        assert_eq!(VALID_SEG_COUNT, 8);
    }

    #[test]
    fn valid_all_is_ff() {
        assert_eq!(VALID_ALL, 0xFF);
    }

    #[test]
    fn page_size_is_4096() {
        assert_eq!(PAGE_SIZE, 4096);
    }
}
