//! Kernel-global pseudo-random number generator.
//!
//! Provides cryptographically non‑secure randomness for `getrandom()` syscall,
//! `/dev/urandom`, ASLR, etc.  Uses **xoshiro256**** with **splitmix64** seeding.
//!
//! # Entropy source
//! Seeded from multiple weak sources at `init()`: cycle counter, wall clock,
//! boot‑time constants, pointer addresses.  This is **not** suitable for
//! cryptographic key generation on a hostile boot — adequate for QEMU test
//! images and userspace build tools (libgit2, cargo, etc.).

use core::sync::atomic::{AtomicU64, Ordering};
use crate::hal::get_time;
use crate::task::perf::perf_time_now;

// ── xoshiro256** state machine ──────────────────────────────────────

/// xoshiro256** state (4 × 64-bit).  Not `Copy` to force explicit reuse.
struct Xoshiro256([u64; 4]);

impl Xoshiro256 {
    /// Create from a 256-bit seed.
    const fn new(s: [u64; 4]) -> Self {
        Self(s)
    }

    /// Generate the next `u64` (xoshiro256**).
    fn next_u64(&mut self) -> u64 {
        let [x, y, z, w] = &mut self.0;
        let result = u128::from(*x)
            .wrapping_mul(5)
            .rotate_left(7)
            .wrapping_mul(9) as u64;

        let t = *y << 17;
        *z ^= *x;
        *w ^= *y;
        *y ^= *z;
        *x ^= *w;
        *z ^= t;
        *w = w.rotate_left(45);

        result
    }

    /// Fill a byte slice.
    fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_ne_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }
}

// ── splitmix64 (seeds xoshiro from a single u64) ────────────────────

/// SplitMix64 state — used only during `init()` to expand a single seed
/// into the 256-bit xoshiro state.
struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut z = self.0.wrapping_add(0x9e3779b97f4a7c15);
        self.0 = z;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
}

// ── Kernel-global singleton ─────────────────────────────────────────

const XOSHIRO256_INIT: Xoshiro256 = Xoshiro256([0; 4]);
static STATE: spin::Mutex<Xoshiro256> = spin::Mutex::new(XOSHIRO256_INIT);
static SEEDED: AtomicU64 = AtomicU64::new(0);

/// Initialise the kernel PRNG.
///
/// Called once during `rust_main()`.  Collects weak entropy from:
/// - cycle counter (rdcycle / rdtime.d)
/// - wall clock (CLOCK_BOOTTIME via `hal::get_time()`)
/// - address of the static state (ASLR slide hint)
/// - boot time captured on the first call
pub fn init() {
    let mut sm = SplitMix64::new(collect_seed());
    let s = [
        sm.next_u64(),
        sm.next_u64(),
        sm.next_u64(),
        sm.next_u64(),
    ];
    // ensure non‑zero — xoshiro degenerates on all‑zero state
    let s = [
        s[0] | 1,
        s[1] | 1,
        s[2] | 1,
        s[3] | 1,
    ];
    let mut state = STATE.lock();
    state.0 = s;
    SEEDED.store(1, Ordering::Release);
}

/// Collect a best‑effort entropy seed from available sources.
fn collect_seed() -> u64 {
    let cycle = perf_time_now() as u64;     // rdcycle / rdtime.d
    let wall = get_time() as u64;           // system timer (ns)
    let state_addr = &XOSHIRO256_INIT as *const _ as u64;
    let static_addr = &SEEDED as *const _ as u64;

    // Mix everything together
    let mut acc = 0u64;
    acc ^= cycle.wrapping_mul(0x9e3779b97f4a7c15);
    acc ^= wall.wrapping_mul(0xbf58476d1ce4e5b9);
    acc ^= state_addr.wrapping_mul(0x94d049bb133111eb);
    acc ^= static_addr;
    acc
}

/// Fill a byte buffer with random bytes.
///
/// Used by `sys_getrandom` and `/dev/urandom::read_at`.
pub fn fill_random(buf: &mut [u8]) {
    // If not seeded yet, seed on first use (fallback for early callers).
    if SEEDED.load(Ordering::Acquire) == 0 {
        // Double‑check under lock to avoid races.
        let mut state = STATE.lock();
        if state.0 == [0; 4] {
            let mut sm = SplitMix64::new(collect_seed().wrapping_add(42));
            let s = [
                sm.next_u64() | 1,
                sm.next_u64() | 1,
                sm.next_u64() | 1,
                sm.next_u64() | 1,
            ];
            state.0 = s;
            SEEDED.store(1, Ordering::Release);
        }
        // else: someone else seeded while we waited — fall through
        drop(state);
    }

    let mut state = STATE.lock();
    state.fill_bytes(buf);
}
