use alloc::vec;
use alloc::vec::Vec;

use crate::kernel_tests::platform_fdt_fixture::vf2_mmc_snapshot;
use crate::kernel_tests::runner::KernelTest;
use crate::hal::device::DeviceManager;

use super::jh7110::discover_v1;
use super::mmio::{
    card_clock_divider, data_error, idmac_control, transfer_command, transfer_needs_stop, Response,
};
use super::sd::{command_word, csd_capacity_sectors};
use super::{block_first_sector, DwMshcError};

pub(crate) fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("dw_mshc::discovers_sdio1_only", discovers_sdio1_only),
        KernelTest::new("dw_mshc::encodes_commands", encodes_commands),
        KernelTest::new("dw_mshc::parses_csd_capacity", parses_csd_capacity),
        KernelTest::new("dw_mshc::rejects_malformed_bindings", rejects_malformed_bindings),
        KernelTest::new("dw_mshc::computes_dividers_and_bounds", computes_dividers_and_bounds),
    ]
}

fn discovers_sdio1_only() -> Result<(), &'static str> {
    let manager = DeviceManager::new(vf2_mmc_snapshot());
    let nodes = manager.find_enabled_by_compatible("snps,dw-mshc");
    if nodes.len() != 2 { return Err("fixture must contain both MMC nodes"); }
    let configs: Vec<_> = nodes.into_iter().filter_map(|node| discover_v1(node).ok()).collect();
    if configs.len() != 1 || configs[0].base != 0x1602_0000 { return Err("SDIO1 discovery did not fail closed"); }
    Ok(())
}

fn encodes_commands() -> Result<(), &'static str> {
    if command_word(0x3f, Response::R1, false, false) & 0x3f != 0x3f { return Err("command index escaped low six bits"); }
    if command_word(55, Response::R1, false, false) & 0x3f != 0x37 { return Err("CMD55 index truncated to SET_BLOCK_COUNT"); }
    if command_word(17, Response::R1, true, false) != 17 | (1 << 6) | (1 << 8) | (1 << 9) { return Err("R1 data command encoding changed"); }
    if command_word(2, Response::R2, false, false) & ((1 << 6) | (1 << 7) | (1 << 8)) != (1 << 6) | (1 << 7) | (1 << 8) { return Err("R2 encoding changed"); }
    if command_word(41, Response::R3, false, false) != 41 | (1 << 6) { return Err("R3 encoding changed"); }
    if command_word(24, Response::R1, true, false) & (1 << 10) != 0 { return Err("command_word must not set DAT_WR; write path ORs it explicitly"); }
    if transfer_command(1, false) != 17
        || transfer_command(8, false) != 18
        || transfer_command(1, true) != 24
        || transfer_command(8, true) != 25
        || transfer_needs_stop(1)
        || !transfer_needs_stop(8)
    {
        return Err("single and multi-block command selection changed");
    }
    if data_error(25, 1 << 8) != Some(DwMshcError::CommandTimeout(25))
        || data_error(18, 1 << 6) != Some(DwMshcError::ResponseCrc(18))
        || data_error(18, 1 << 7) != Some(DwMshcError::DataCrc)
        || data_error(18, 1 << 11) != Some(DwMshcError::FifoRun)
    {
        return Err("data error mapping changed");
    }
    if idmac_control(0, 2) != 0x8000_001a
        || idmac_control(1, 2) != 0x8000_0004
        || idmac_control(0, 1) != 0x8000_000c
    {
        return Err("IDMAC descriptor control flags changed");
    }
    Ok(())
}

fn rejects_malformed_bindings() -> Result<(), &'static str> {
    let manager = DeviceManager::new(vf2_mmc_snapshot());
    let node = manager.find_enabled_by_compatible("snps,dw-mshc").into_iter().find(|node| node.mmio_range(0).map(|range| range.base) == Some(0x1602_0000)).ok_or("missing SDIO1 fixture")?;
    let mut truncated = node.clone(); property(&mut truncated, "clocks").value.pop();
    let mut wrong_rate = node.clone(); property(&mut wrong_rate, "assigned-clock-rates").value = 25_000_000u32.to_be_bytes().to_vec();
    let mut wrong_mmio = node.clone(); wrong_mmio.mmio_ranges[0].base = 0x1601_0000;
    let mut wrong_width = node.clone(); property(&mut wrong_width, "bus-width").value = 8u32.to_be_bytes().to_vec();
    if [truncated, wrong_rate, wrong_mmio, wrong_width].iter().any(|candidate| discover_v1(candidate).is_ok()) { return Err("malformed binding was accepted"); }
    Ok(())
}

fn computes_dividers_and_bounds() -> Result<(), &'static str> {
    if card_clock_divider(50_000_000, 400_000) != Some(63) { return Err("400kHz divider incorrect"); }
    if card_clock_divider(50_000_000, 25_000_000) != Some(1) { return Err("25MHz divider incorrect"); }
    if block_first_sector(usize::MAX / 8 + 1).is_some() { return Err("block sector multiplication overflowed"); }
    Ok(())
}

fn parses_csd_capacity() -> Result<(), &'static str> {
    let mut v2 = [0u32; 4]; set_bits(&mut v2, 127, 126, 1); set_bits(&mut v2, 69, 48, 3);
    if csd_capacity_sectors(v2).map_err(|_| "v2 CSD rejected")? != 4096 { return Err("v2 capacity incorrect"); }
    let mut v1 = [0u32; 4]; set_bits(&mut v1, 83, 80, 9); set_bits(&mut v1, 73, 62, 1); set_bits(&mut v1, 49, 47, 0);
    if csd_capacity_sectors(v1).map_err(|_| "v1 CSD rejected")? != 8 { return Err("v1 capacity incorrect"); }
    Ok(())
}

fn set_bits(csd: &mut [u32; 4], high: u8, low: u8, value: u32) { for bit in low..=high { let word = 3 - bit as usize / 32; csd[word] |= ((value >> (bit - low)) & 1) << (bit as usize % 32); } }
fn property<'a>(node: &'a mut crate::hal::platform::info::DeviceInfo, name: &str) -> &'a mut crate::hal::platform::info::RawProperty { node.raw_properties.iter_mut().find(|property| property.name == name).ok_or(()).unwrap_or_else(|_| panic!("missing fixture property")) }
