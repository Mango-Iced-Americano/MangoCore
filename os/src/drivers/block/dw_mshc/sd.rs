use super::mmio::{DwMshcHost, Response};
use super::DwMshcError;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SdCardInfo {
    pub(crate) rca: u16,
    pub(crate) high_capacity: bool,
    pub(crate) capacity_sectors: u64,
    pub(crate) csd: [u32; 4],
}

pub(crate) fn initialize_card(host: &mut DwMshcHost) -> Result<SdCardInfo, DwMshcError> {
    // U-Boot mmc_go_idle: udelay(1000) before CMD0; card needs time to stabilize
    // its clock/state after power-up before it can accept the reset command.
    wait_ms(1);
    host.command(0, 0, Response::None, true, false)?;
    // U-Boot mmc_go_idle: udelay(2000) after CMD0; card needs ~74+ clock cycles
    // after reset before it can reliably answer the next command (CMD8/ACMD41).
    wait_ms(2);
    let v2 = match host.command(8, 0x1aa, Response::R7, false, false) {
        Ok(response) if response[0] & 0xfff == 0x1aa => true,
        Ok(_) => return Err(DwMshcError::UnsupportedCard),
        Err(DwMshcError::CommandTimeout(8)) => false,
        Err(error) => return Err(error),
    };
    // Give the card time after the interface-conditions handshake before the
    // first CMD55 of the ACMD41 probing loop (U-Boot's per-iteration overhead
    // provides this implicitly; make it explicit on the first iteration).
    wait_ms(1);
    let mut ocr = 0;
    for _ in 0..100 {
        host.command(55, 0, Response::R1, false, false)?;
        ocr = host.command(41, 0x00ff_8000 | if v2 { 1 << 30 } else { 0 }, Response::R3, false, false)?[0];
        if ocr & (1 << 31) != 0 {
            break;
        }
        wait_10ms();
    }
    if ocr & (1 << 31) == 0 {
        return Err(DwMshcError::CommandTimeout(41));
    }
    let _cid = host.command(2, 0, Response::R2, false, false)?;
    let rca = (host.command(3, 0, Response::R6, false, false)?[0] >> 16) as u16;
    let csd = host.command(9, (rca as u32) << 16, Response::R2, false, false)?;
    let high_capacity = ocr & (1 << 30) != 0;
    let capacity_sectors = csd_capacity_sectors(csd)?;
    host.command(7, (rca as u32) << 16, Response::R1b, false, true)?;
    if !high_capacity {
        host.command(16, 512, Response::R1, false, false)?;
    }
    host.command(55, (rca as u32) << 16, Response::R1, false, false)?;
    host.command(6, 2, Response::R1, false, false)?;
    host.set_bus_width_4bit();
    host.set_card_clock(1)?;
    Ok(SdCardInfo { rca, high_capacity, capacity_sectors, csd })
}

pub(crate) fn csd_capacity_sectors(csd: [u32; 4]) -> Result<u64, DwMshcError> {
    let structure = bits(csd, 127, 126);
    let bytes = match structure {
        1 => (bits(csd, 69, 48) as u64 + 1).checked_mul(512 * 1024),
        0 => {
            let c_size = bits(csd, 73, 62) as u64 + 1;
            let multiplier = bits(csd, 49, 47) + 2;
            let read_len = bits(csd, 83, 80);
            c_size.checked_shl(multiplier).and_then(|value| value.checked_shl(read_len))
        }
        _ => return Err(DwMshcError::UnsupportedCard),
    };
    bytes.map(|value| value / 512).ok_or(DwMshcError::OutOfRange)
}

pub(crate) const fn command_word(index: u8, response: Response, data: bool, init: bool) -> u32 {
    (index as u32 & 0x3f) | response.command_bits() | if data { 1 << 9 } else { 0 } | if init { 1 << 15 } else { 0 }
}

const fn bits(csd: [u32; 4], high: u8, low: u8) -> u32 {
    let mut value = 0;
    let mut bit = low;
    while bit <= high {
        let word = 3 - bit as usize / 32;
        value |= ((csd[word] >> (bit as usize % 32)) & 1) << (bit - low);
        bit += 1;
    }
    value
}

fn wait_ms(ms: usize) {
    let deadline = crate::timer::get_time_ms().saturating_add(ms);
    while crate::timer::get_time_ms() < deadline {
        core::hint::spin_loop();
    }
}

fn wait_10ms() {
    wait_ms(10);
}
