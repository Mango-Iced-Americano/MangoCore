use crate::hal::platform::info::DeviceInfo;
use crate::timer;

use super::DwMshcError;

pub(crate) const SYS_CRG_BASE: usize = 0x1302_0000;
const SDIO1_AHB_CLOCK: usize = 0x0170;
const SDIO1_SDCARD_CLOCK: usize = 0x0178;
const RESET_ASSERT2: usize = 0x0300;
const RESET_STATUS2: usize = 0x0310;
const SDIO1_RESET_BIT: u32 = 1 << 1;
const CLOCK_ENABLE: u32 = 1 << 31;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Jh7110MshcConfig {
    pub(crate) base: usize,
    pub(crate) size: usize,
    pub(crate) fifo_depth: u32,
    pub(crate) input_clock_hz: u32,
    pub(crate) reset_id: u32,
}

pub(crate) fn discover_v1(device: &DeviceInfo) -> Result<Jh7110MshcConfig, DwMshcError> {
    let range = device.mmio_range(0).ok_or(DwMshcError::MalformedFdt)?;
    let bus_width = be_u32(device, "bus-width")?;
    let fifo_depth = be_u32(device, "fifo-depth")?;
    let rate = be_u32(device, "assigned-clock-rates")?;
    let clocks = device.raw_property_exact::<16>("clocks").map_err(|_| DwMshcError::MalformedFdt)?;
    let reset = device.raw_property_exact::<8>("resets").map_err(|_| DwMshcError::MalformedFdt)?;
    let clock_provider = u32::from_be_bytes([clocks[0], clocks[1], clocks[2], clocks[3]]);
    let ahb_id = u32::from_be_bytes([clocks[4], clocks[5], clocks[6], clocks[7]]);
    let card_id = u32::from_be_bytes([clocks[12], clocks[13], clocks[14], clocks[15]]);
    let reset_provider = u32::from_be_bytes([reset[0], reset[1], reset[2], reset[3]]);
    let reset_id = u32::from_be_bytes([reset[4], reset[5], reset[6], reset[7]]);
    if range.base != 0x1602_0000
        || range.size < 0x204
        || bus_width != 4
        || fifo_depth < 32
        || clock_provider != 15
        || ahb_id != 92
        || card_id != 94
        || reset_provider != 16
        || reset_id != 65
        || rate != 50_000_000
    {
        return Err(DwMshcError::UnsupportedController);
    }
    Ok(Jh7110MshcConfig { base: range.base, size: range.size, fifo_depth, input_clock_hz: rate, reset_id })
}

pub(crate) fn enable_clocks_and_release_reset(reset_id: u32) -> Result<(), DwMshcError> {
    if reset_id != 65 {
        return Err(DwMshcError::UnsupportedController);
    }
    // Gate both SDIO1 clocks on. Use read-modify-write so U-Boot's already
    // programmed dividers survive; Linux (clk-starfive-jh7110-gen.c) treats
    // the sdcard clock as a gdiv and only touches the enable bit here.
    update(SYS_CRG_BASE, SDIO1_AHB_CLOCK, |value| value | CLOCK_ENABLE);
    update(SYS_CRG_BASE, SDIO1_SDCARD_CLOCK, |value| value | CLOCK_ENABLE);
    // Deassert the SDIO1 reset (RESET_ASSERT2 bit1 clear) and poll
    // RESET_STATUS2 until the status bit reads 1 (reset released). Per U-Boot
    // (drivers/reset/reset-jh7110.c) and Linux
    // (drivers/reset/starfive/reset-starfive-jh7110.c), the status bit is 0
    // while the reset is asserted and 1 once released, so we must wait for
    // bit == 1; the previous `!= 0` polarity always timed out. U-Boot polls
    // this for up to 10000 iterations, so allow far more than 1 ms here.
    update(SYS_CRG_BASE, RESET_ASSERT2, |value| value & !SDIO1_RESET_BIT);
    let deadline = timer::get_time_ms().saturating_add(100);
    while read(SYS_CRG_BASE, RESET_STATUS2) & SDIO1_RESET_BIT == 0 {
        if timer::get_time_ms() >= deadline {
            return Err(DwMshcError::ClockResetTimeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn be_u32(device: &DeviceInfo, name: &str) -> Result<u32, DwMshcError> {
    let value = device.raw_property_exact::<4>(name).map_err(|_| DwMshcError::MalformedFdt)?;
    Ok(u32::from_be_bytes(*value))
}

#[inline(always)]
fn read(base: usize, offset: usize) -> u32 {
    // SAFETY: Categories 6 and 11. The validated JH7110 CRG base and aligned
    // register offsets are supervisor-mapped before this driver probes.
    unsafe {
        core::ptr::read_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
        )
    }
}

#[inline(always)]
fn write(base: usize, offset: usize, value: u32) {
    // SAFETY: Categories 6 and 11. The validated JH7110 CRG base and aligned
    // register offsets target only documented clock/reset control registers.
    unsafe {
        core::ptr::write_volatile(
            crate::mm::PhysAddr(base + offset)
                .direct_map_ptr()
                .cast::<u32>(),
            value,
        )
    }
}

fn update(base: usize, offset: usize, transform: impl FnOnce(u32) -> u32) {
    write(base, offset, transform(read(base, offset)));
}
