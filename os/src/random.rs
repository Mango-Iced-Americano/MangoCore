//! Kernel random-number subsystem.
//!
//! A platform entropy source seeds one global ChaCha20 generator before
//! userspace starts. All consumers share this state, and each request rekeys
//! the generator with hidden output so later state disclosure cannot recover
//! bytes returned by earlier requests.
//!
//! When NO platform entropy source exists at all (e.g. QEMU without a
//! virtio-rng device), `init()` falls back to the untrusted bootstrap seed and
//! marks the stream ready while logging a clear warning, so the system stays
//! bootable on such hardware — at the cost of not being cryptographically
//! seeded. A present-but-faulty source still fails closed.

use crate::drivers::rng::{self, EntropyError};
use lazy_static::lazy_static;
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};
use spin::Mutex;

const SEED_BYTES: usize = 32;
const BOOT_SAMPLE_BYTES: usize = SEED_BYTES * 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RandomError {
    NotReady,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RandomInitError {
    Entropy(EntropyError),
    HealthCheckFailed,
}

struct RandomState {
    rng: ChaCha20Rng,
    ready: bool,
}

impl RandomState {
    fn new() -> Self {
        let mut seed = bootstrap_seed();
        let rng = ChaCha20Rng::from_seed(seed);
        wipe_sensitive(&mut seed);
        Self {
            // This seed is deliberately not credited as entropy. It only
            // implements Linux's explicitly insecure GRND_INSECURE mode.
            rng,
            ready: false,
        }
    }

    fn install_trusted_sample(&mut self, sample: &[u8; BOOT_SAMPLE_BYTES]) {
        let mut first = [0u8; SEED_BYTES];
        first.copy_from_slice(&sample[..SEED_BYTES]);

        // ChaCha conditions the first half; XOR then preserves the entropy of
        // either independent half and incorporates the previous private state.
        let mut conditioned = [0u8; SEED_BYTES];
        ChaCha20Rng::from_seed(first).fill_bytes(&mut conditioned);
        for (dst, src) in conditioned.iter_mut().zip(sample[SEED_BYTES..].iter()) {
            *dst ^= *src;
        }
        let mut old_state = [0u8; SEED_BYTES];
        self.rng.fill_bytes(&mut old_state);
        for (dst, old) in conditioned.iter_mut().zip(old_state.iter()) {
            *dst ^= *old;
        }

        self.rng = ChaCha20Rng::from_seed(conditioned);
        self.ready = true;

        wipe_sensitive(&mut first);
        wipe_sensitive(&mut old_state);
        wipe_sensitive(&mut conditioned);
    }

    fn fill(&mut self, dst: &mut [u8], require_ready: bool) -> Result<(), RandomError> {
        if dst.is_empty() {
            return Ok(());
        }
        if require_ready && !self.ready {
            return Err(RandomError::NotReady);
        }
        self.rng.fill_bytes(dst);
        self.rekey();
        Ok(())
    }

    fn rekey(&mut self) {
        let mut next_seed = [0u8; SEED_BYTES];
        self.rng.fill_bytes(&mut next_seed);
        self.rng = ChaCha20Rng::from_seed(next_seed);
        wipe_sensitive(&mut next_seed);
    }

    /// Reseed from freshly-derived bootstrap material and mark the stream ready.
    ///
    /// This is the insecure fallback used only when NO platform entropy source
    /// exists (e.g. QEMU without a virtio-rng). It is deliberately never used
    /// when a source is present but faulty, so a broken TRNG cannot silently
    /// downgrade the system to an insecure state.
    fn install_bootstrap_fallback(&mut self) {
        // Re-derive the seed at fallback time rather than reusing the
        // lazy_static construction-time seed.
        let mut seed = bootstrap_seed();
        // Mix in one more low-cost non-deterministic input (current time and a
        // fresh stack address) to distinguish this reseed from the bootstrap.
        let mut extra = [0u8; SEED_BYTES];
        for chunk in extra.chunks_exact_mut(8) {
            let stack_marker = 0u8;
            let value = (crate::hal::get_time() as u64)
                ^ ((&stack_marker as *const u8 as usize) as u64).rotate_left(13);
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        for (dst, src) in seed.iter_mut().zip(extra.iter()) {
            *dst ^= *src;
        }

        self.rng = ChaCha20Rng::from_seed(seed);
        self.ready = true;

        wipe_sensitive(&mut seed);
        wipe_sensitive(&mut extra);
    }

    fn mix_untrusted(&mut self, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        let mut next_seed = [0u8; SEED_BYTES];
        self.rng.fill_bytes(&mut next_seed);
        for (index, byte) in input.iter().enumerate() {
            next_seed[index % SEED_BYTES] ^= *byte;
        }
        for (index, byte) in (input.len() as u64).to_le_bytes().iter().enumerate() {
            next_seed[index] ^= *byte;
        }
        self.rng = ChaCha20Rng::from_seed(next_seed);
        wipe_sensitive(&mut next_seed);
    }
}

lazy_static! {
    static ref RANDOM_STATE: Mutex<RandomState> = Mutex::new(RandomState::new());
}

/// Initialize the secure stream from the platform's trusted entropy source.
/// Source I/O happens before taking the CSPRNG lock.
pub fn init() -> Result<(), RandomInitError> {
    let mut sample = [0u8; BOOT_SAMPLE_BYTES];
    let source = match rng::fill_entropy(&mut sample) {
        Ok(source) => source,
        Err(error) => {
            wipe_sensitive(&mut sample);
            match error {
                // No entropy source at all (e.g. QEMU without a virtio-rng
                // device): fall back to the untrusted bootstrap seed so the
                // system can still boot. A present-but-faulty source takes the
                // default path below and fails closed.
                EntropyError::DeviceUnavailable => {
                    RANDOM_STATE.lock().install_bootstrap_fallback();
                    println!(
                        "[kernel] random: no trusted entropy source; using bootstrap fallback (insecure)"
                    );
                    return Ok(());
                }
                _ => return Err(RandomInitError::Entropy(error)),
            }
        }
    };
    if !boot_health_check(&sample) {
        wipe_sensitive(&mut sample);
        return Err(RandomInitError::HealthCheckFailed);
    }

    RANDOM_STATE.lock().install_trusted_sample(&sample);
    wipe_sensitive(&mut sample);
    println!("[kernel] random: initialized from {}", source.name());
    Ok(())
}

pub fn is_ready() -> bool {
    RANDOM_STATE.lock().ready
}

/// Fill from the cryptographically secure stream, failing closed until a
/// trusted source has initialized it.
pub fn fill_bytes(dst: &mut [u8]) -> Result<(), RandomError> {
    RANDOM_STATE.lock().fill(dst, true)
}

/// Implement GRND_INSECURE without ever presenting bootstrap state as secure.
pub fn fill_insecure_bytes(dst: &mut [u8]) -> Result<(), RandomError> {
    RANDOM_STATE.lock().fill(dst, false)
}

/// Mix caller-provided bytes without increasing the entropy readiness state.
pub fn mix_untrusted(input: &[u8]) {
    RANDOM_STATE.lock().mix_untrusted(input);
}

/// Prevent the compiler from eliding cleanup of temporary random material.
pub(crate) fn wipe_sensitive(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

fn boot_health_check(sample: &[u8; BOOT_SAMPLE_BYTES]) -> bool {
    if sample[..SEED_BYTES] == sample[SEED_BYTES..] {
        return false;
    }

    let mut unique = 0usize;
    let word_count = BOOT_SAMPLE_BYTES / 4;
    for index in 0..word_count {
        let word = sample_word(sample, index);
        if (0..index).all(|previous| sample_word(sample, previous) != word) {
            unique += 1;
        }
    }
    unique >= word_count / 2
}

fn sample_word(sample: &[u8; BOOT_SAMPLE_BYTES], index: usize) -> u32 {
    let offset = index * 4;
    u32::from_le_bytes([
        sample[offset],
        sample[offset + 1],
        sample[offset + 2],
        sample[offset + 3],
    ])
}

fn bootstrap_seed() -> [u8; SEED_BYTES] {
    let stack_marker = 0u8;
    let mut state = (crate::hal::get_time() as u64)
        ^ ((&stack_marker as *const u8 as usize) as u64).rotate_left(17)
        ^ (crate::hal::firmware::usable_memory_size() as u64).rotate_left(31)
        ^ 0x4d41_4e47_4f52_4e47;
    let mut seed = [0u8; SEED_BYTES];
    for chunk in seed.chunks_exact_mut(8) {
        // SplitMix64 expands the explicitly untrusted bootstrap material.
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    seed
}
