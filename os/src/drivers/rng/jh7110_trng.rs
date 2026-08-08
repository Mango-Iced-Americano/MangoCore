//! Polling entropy source for the JH7110 security TRNG.
//!
//! The binding check happens before any MMIO access: QEMU has no `starfive,trng`
//! node, so it must fall through to virtio-rng without touching board addresses.

use crate::hal::device::DeviceManager;
use crate::hal::platform::info::DeviceInfo;
use crate::timer;

use super::EntropyError;

const TRNG_COMPATIBLE: &str = "starfive,trng";
const TRNG_UPSTREAM_COMPATIBLE: &str = "starfive,jh7110-trng";
const TRNG_BASE: usize = 0x1600_c000;
const TRNG_MIN_SIZE: usize = 0x4000;
const STG_CRG_BASE: usize = 0x1023_0000;

const CLOCK_ENABLE: u32 = 1 << 31;
// Clock IDs 205 and 206 are in the STG clock register range, which starts at
// global clock ID 190. The hardware register offsets are therefore 0x3c and
// 0x40 from STGCRG, not 0x334 and 0x338 from SYSCRG.
const STG_CLOCK_FIRST_ID: usize = 190;
const SEC_HCLK: usize = (205 - STG_CLOCK_FIRST_ID) * core::mem::size_of::<u32>();
const SEC_MISCAHB_CLK: usize = (206 - STG_CLOCK_FIRST_ID) * core::mem::size_of::<u32>();
const RESET_ASSERT: usize = 0x74;
const RESET_STATUS: usize = 0x78;
const SEC_AHB_RESET: u32 = 1 << 3;

const CTRL: usize = 0x00;
const STAT: usize = 0x04;
const MODE: usize = 0x08;
const IE: usize = 0x10;
const ISTAT: usize = 0x14;
const RAND0: usize = 0x20;
const AUTO_RQSTS: usize = 0x60;
const AUTO_AGE: usize = 0x64;

const CTRL_GENE_RANDNUM: u32 = 0x1;
const CTRL_EXEC_RANDRESEED: u32 = 0x2;
const MODE_R256: u32 = 1 << 3;
const STAT_RAND_GENERATING: u32 = 1 << 30;
const STAT_RAND_SEEDING: u32 = 1 << 31;
const ISTAT_RAND_RDY: u32 = 1 << 0;
const ISTAT_SEED_DONE: u32 = 1 << 1;
const ISTAT_LFSR_LOCKUP: u32 = 1 << 4;
const ISTAT_ALL: u32 = ISTAT_RAND_RDY | ISTAT_SEED_DONE | ISTAT_LFSR_LOCKUP;

const RESET_TIMEOUT_MS: usize = 100;
const COMMAND_TIMEOUT_MS: usize = 10;
const SAMPLE_BYTES: usize = 32;

struct Jh7110Trng {
    base: usize,
}

pub(super) fn fill_entropy(dst: &mut [u8]) -> Result<(), EntropyError> {
    if dst.is_empty() {
        return Ok(());
    }

    let platform = crate::hal::platform::platform_info();
    let manager = DeviceManager::new(platform.devices.clone());
    for compatible in [TRNG_COMPATIBLE, TRNG_UPSTREAM_COMPATIBLE] {
        // VF2's vendor DTB marks the present, board-controlled TRNG disabled.
        // `discover` below validates its complete hardware resource contract
        // before any MMIO access; QEMU has no matching node and still falls
        // through to virtio-rng.
        for device in manager.find_by_compatible(compatible) {
            let Some(trng) = Jh7110Trng::discover(device) else {
                continue;
            };
            return trng.fill(dst);
        }
    }
    Err(EntropyError::DeviceUnavailable)
}

impl Jh7110Trng {
    fn discover(device: &DeviceInfo) -> Option<Self> {
        let range = device.mmio_range(0)?;
        let clocks = device.raw_property_exact::<16>("clocks").ok()?;
        let reset = device.raw_property_exact::<8>("resets").ok()?;
        let clock_provider = u32::from_be_bytes([clocks[0], clocks[1], clocks[2], clocks[3]]);
        let hclk = u32::from_be_bytes([clocks[4], clocks[5], clocks[6], clocks[7]]);
        let miscahb = u32::from_be_bytes([clocks[12], clocks[13], clocks[14], clocks[15]]);
        let reset_provider = u32::from_be_bytes([reset[0], reset[1], reset[2], reset[3]]);
        let reset_id = u32::from_be_bytes([reset[4], reset[5], reset[6], reset[7]]);
        if range.base != TRNG_BASE
            || range.size < TRNG_MIN_SIZE
            || clock_provider != 15
            || hclk != 205
            || miscahb != 206
            || reset_provider != 16
            || reset_id != 131
        {
            return None;
        }
        Some(Self { base: range.base })
    }

    fn fill(&self, dst: &mut [u8]) -> Result<(), EntropyError> {
        self.enable_clocks_and_release_reset()?;
        self.initialize()?;
        for chunk in dst.chunks_mut(SAMPLE_BYTES) {
            self.generate(chunk)?;
        }
        Ok(())
    }

    fn enable_clocks_and_release_reset(&self) -> Result<(), EntropyError> {
        update(STG_CRG_BASE, SEC_HCLK, |value| value | CLOCK_ENABLE);
        update(STG_CRG_BASE, SEC_MISCAHB_CLK, |value| value | CLOCK_ENABLE);
        update(STG_CRG_BASE, RESET_ASSERT, |value| value & !SEC_AHB_RESET);
        let deadline = timer::get_time_ms().saturating_add(RESET_TIMEOUT_MS);
        while read(STG_CRG_BASE, RESET_STATUS) & SEC_AHB_RESET == 0 {
            if timer::get_time_ms() >= deadline {
                return Err(EntropyError::DeviceInit);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn initialize(&self) -> Result<(), EntropyError> {
        // Linux v6.6 exposes both values as module parameters whose default is
        // zero; U-Boot's polling driver writes zero as well, disabling automatic
        // reseed counters in favour of the explicit boot-time reseed below.
        self.write(AUTO_AGE, 0);
        self.write(AUTO_RQSTS, 0);
        self.write(IE, 0);
        self.clear_status(ISTAT_ALL);
        self.write(MODE, self.read(MODE) | MODE_R256);
        self.reseed()
    }

    fn reseed(&self) -> Result<(), EntropyError> {
        loop {
            self.start_command(CTRL_EXEC_RANDRESEED);
            if !self.wait_for(ISTAT_SEED_DONE)? {
                return Ok(());
            }
        }
    }

    fn generate(&self, dst: &mut [u8]) -> Result<(), EntropyError> {
        loop {
            self.wait_idle()?;
            self.start_command(CTRL_GENE_RANDNUM);
            if self.wait_for(ISTAT_RAND_RDY)? {
                self.reseed()?;
                continue;
            }
            for (index, chunk) in dst.chunks_mut(core::mem::size_of::<u32>()).enumerate() {
                let word = self.read(RAND0 + index * core::mem::size_of::<u32>()).to_le_bytes();
                chunk.copy_from_slice(&word[..chunk.len()]);
            }
            return Ok(());
        }
    }

    fn start_command(&self, command: u32) {
        self.clear_status(ISTAT_ALL);
        self.write(CTRL, command);
    }

    /// Returns true when lockup forced the hardware to start a reseed.
    fn wait_for(&self, complete: u32) -> Result<bool, EntropyError> {
        let deadline = timer::get_time_ms().saturating_add(COMMAND_TIMEOUT_MS);
        loop {
            let status = self.read(ISTAT);
            if status & ISTAT_LFSR_LOCKUP != 0 {
                self.clear_status(ISTAT_LFSR_LOCKUP);
                return Ok(true);
            }
            if status & complete != 0 {
                self.clear_status(complete);
                return Ok(false);
            }
            if timer::get_time_ms() >= deadline {
                return Err(EntropyError::DeviceRead);
            }
            core::hint::spin_loop();
        }
    }

    fn wait_idle(&self) -> Result<(), EntropyError> {
        let deadline = timer::get_time_ms().saturating_add(COMMAND_TIMEOUT_MS);
        while self.read(STAT) & (STAT_RAND_GENERATING | STAT_RAND_SEEDING) != 0 {
            if timer::get_time_ms() >= deadline {
                return Err(EntropyError::DeviceRead);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn clear_status(&self, bits: u32) {
        self.write(ISTAT, bits);
    }

    #[inline(always)]
    fn read(&self, offset: usize) -> u32 {
        read(self.base, offset)
    }

    #[inline(always)]
    fn write(&self, offset: usize, value: u32) {
        write(self.base, offset, value)
    }
}

#[inline(always)]
fn read(base: usize, offset: usize) -> u32 {
    // SAFETY: Categories 6 and 11. FDT discovery validates the aligned TRNG
    // range before construction; CRG offsets are documented aligned registers.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

#[inline(always)]
fn write(base: usize, offset: usize, value: u32) {
    // SAFETY: Categories 6 and 11. All calls use validated, identity-mapped
    // JH7110 MMIO ranges and documented aligned register offsets.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) }
}

fn update(base: usize, offset: usize, transform: impl FnOnce(u32) -> u32) {
    write(base, offset, transform(read(base, offset)));
}
