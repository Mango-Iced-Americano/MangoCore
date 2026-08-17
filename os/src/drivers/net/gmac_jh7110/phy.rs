use crate::hal::{get_clock_freq, get_time};

use super::mmio::{GmacMmio, MDIO_ADDR, MDIO_DATA};
use super::GmacJh7110Error;

const PHY_ADDRESS: u8 = 0;
const MII_BMCR: u8 = 0;
const MII_BMSR: u8 = 1;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;
const MII_SPEC_STATUS: u8 = 0x11;
const MII_EXT_ADDR: u8 = 0x1e;
const MII_EXT_DATA: u8 = 0x1f;

const MDIO_GBUSY: u32 = 1 << 0;
const MDIO_GOC_WRITE: u32 = 1 << 2;
const MDIO_GOC_READ: u32 = 0b11 << 2;
const MDIO_CLK_CSR_MASK: u32 = 0b1111 << 8;
const MDIO_CLK_CSR_150_250_MHZ: u32 = 0b0011 << 8;
const MDIO_PHY_SHIFT: u32 = 21;
const MDIO_REG_SHIFT: u32 = 16;
const MII_BMCR_ANENABLE: u16 = 1 << 12;
const MII_BMCR_POWERDOWN: u16 = 1 << 11;
const MII_BMCR_ANRESTART: u16 = 1 << 9;
const MII_BMSR_LINK_STATUS: u16 = 1 << 2;
const YT8531_LINK_STATUS: u16 = 1 << 10;
const YT8531_DUPLEX: u16 = 1 << 13;
const YT8531_SPEED_SHIFT: u16 = 14;
const YT8531_ID_MASK: u32 = 0xffff_fff0;
const YT8531_ID: u32 = 0x4f51_e91b;
const YT8531_CHIP_CONFIG: u16 = 0xa001;
const YT8531_RGMII_CONFIG1: u16 = 0xa003;
const YT8531_PAD_DRIVE_STRENGTH: u16 = 0xa010;
const YT8531_SYNCE_CONFIG: u16 = 0xa012;
const YT8531_CLOCK_GATING: u16 = 0x000c;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LinkState {
    pub(super) up: bool,
    pub(super) speed_mbps: u16,
    pub(super) full_duplex: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Yt8531Diagnostics {
    pub(super) chip_config: u16,
    pub(super) pad_drive_strength: u16,
    pub(super) synce_config: u16,
    pub(super) clock_gating: u16,
}

fn wait_idle(regs: GmacMmio) -> Result<(), GmacJh7110Error> {
    let start = get_time();
    let timeout = (get_clock_freq() / 10).max(1);
    while regs.read(MDIO_ADDR) & MDIO_GBUSY != 0 {
        if get_time().wrapping_sub(start) >= timeout {
            return Err(GmacJh7110Error::InvalidPhy(0));
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn mdio_command(regs: GmacMmio, phy: u8, register: u8, operation: u32) -> u32 {
    let inherited_clock = regs.read(MDIO_ADDR) & MDIO_CLK_CSR_MASK;
    let clock = if inherited_clock == 0 {
        MDIO_CLK_CSR_150_250_MHZ
    } else {
        inherited_clock
    };
    (u32::from(phy) << MDIO_PHY_SHIFT)
        | (u32::from(register) << MDIO_REG_SHIFT)
        | clock
        | operation
        | MDIO_GBUSY
}

fn mdio_read(regs: GmacMmio, phy: u8, register: u8) -> Result<u16, GmacJh7110Error> {
    wait_idle(regs)?;
    regs.write(MDIO_DATA, 0);
    regs.write(MDIO_ADDR, mdio_command(regs, phy, register, MDIO_GOC_READ));
    wait_idle(regs)?;
    Ok(regs.read(MDIO_DATA) as u16)
}

fn mdio_write(
    regs: GmacMmio,
    phy: u8,
    register: u8,
    value: u16,
) -> Result<(), GmacJh7110Error> {
    wait_idle(regs)?;
    regs.write(MDIO_DATA, u32::from(value));
    regs.write(MDIO_ADDR, mdio_command(regs, phy, register, MDIO_GOC_WRITE));
    wait_idle(regs)
}

fn ext_read(regs: GmacMmio, register: u16) -> Result<u16, GmacJh7110Error> {
    mdio_write(regs, PHY_ADDRESS, MII_EXT_ADDR, register)?;
    mdio_read(regs, PHY_ADDRESS, MII_EXT_DATA)
}

fn ext_write(regs: GmacMmio, register: u16, value: u16) -> Result<(), GmacJh7110Error> {
    mdio_write(regs, PHY_ADDRESS, MII_EXT_ADDR, register)?;
    mdio_write(regs, PHY_ADDRESS, MII_EXT_DATA, value)
}

pub(super) fn read_diagnostics(regs: GmacMmio) -> Result<Yt8531Diagnostics, GmacJh7110Error> {
    Ok(Yt8531Diagnostics {
        chip_config: ext_read(regs, YT8531_CHIP_CONFIG)?,
        pad_drive_strength: ext_read(regs, YT8531_PAD_DRIVE_STRENGTH)?,
        synce_config: ext_read(regs, YT8531_SYNCE_CONFIG)?,
        clock_gating: ext_read(regs, YT8531_CLOCK_GATING)?,
    })
}

pub(super) fn configure_yt8531(regs: GmacMmio) -> Result<u32, GmacJh7110Error> {
    let phy_id = (u32::from(mdio_read(regs, PHY_ADDRESS, MII_PHYSID1)?) << 16)
        | u32::from(mdio_read(regs, PHY_ADDRESS, MII_PHYSID2)?);
    if phy_id & YT8531_ID_MASK != YT8531_ID & YT8531_ID_MASK {
        return Err(GmacJh7110Error::InvalidPhy(phy_id));
    }

    // Step 1: Configure RGMII TX delays (0xA003): GE_TX=13, FE_TX=13.
    // These survive SW_RST, so set them before the reset.
    let rgmii = ext_read(regs, YT8531_RGMII_CONFIG1)?;
    let delays = (13u16) | (13u16 << 4);
    ext_write(regs, YT8531_RGMII_CONFIG1, (rgmii & !0x00ff) | delays)?;

    // Step 2: Soft-reset the PHY. All extension register settings (except
    // 0xA003 delays) are cleared, so everything below MUST be re-applied.
    let chip = ext_read(regs, YT8531_CHIP_CONFIG)?;
    ext_write(regs, YT8531_CHIP_CONFIG, chip | (1 << 15))?;
    // MDIO may be unreliable while the PHY is resetting; use a fixed delay
    // instead of polling the extension register (as the Linux motorcomm
    // driver does by polling MII BMSR).
    let start = get_time();
    let deadline = (get_clock_freq() / 10).max(1);
    while get_time().wrapping_sub(start) < deadline {
        core::hint::spin_loop();
    }

    // Step 3: Enable RXC clock output (active-low: clear bit 12).
    let cgr = ext_read(regs, YT8531_CLOCK_GATING)?;
    ext_write(regs, YT8531_CLOCK_GATING, cgr & !(1 << 12))?;

    // Step 4: Disable auto-sleep, keep PLL running.
    let slp = ext_read(regs, 0x0027)?;
    ext_write(regs, 0x0027, (slp & !(1 << 15)) | (1 << 14))?;

    // Step 5: Set RGMII_SEL and RXC_DLY_EN on 0xA001.
    let chip = ext_read(regs, YT8531_CHIP_CONFIG)?;
    ext_write(regs, YT8531_CHIP_CONFIG, chip | (1 << 13) | (1 << 8))?;

    // Step 6: Configure SYNCE clock source to PLL_125M for RGMII.
    let synce = ext_read(regs, YT8531_SYNCE_CONFIG)?;
    ext_write(regs, YT8531_SYNCE_CONFIG, (synce & !0xeu16) | 0x10u16)?;

    let mut bmcr = mdio_read(regs, PHY_ADDRESS, MII_BMCR)?;
    bmcr &= !MII_BMCR_POWERDOWN;
    bmcr |= MII_BMCR_ANENABLE;
    let _ = mdio_read(regs, PHY_ADDRESS, MII_BMSR)?;
    if mdio_read(regs, PHY_ADDRESS, MII_BMSR)? & MII_BMSR_LINK_STATUS == 0 {
        bmcr |= MII_BMCR_ANRESTART;
    }
    mdio_write(regs, PHY_ADDRESS, MII_BMCR, bmcr)?;
    Ok(phy_id)
}

pub(super) fn read_link_state(regs: GmacMmio) -> Result<LinkState, GmacJh7110Error> {
    let _ = mdio_read(regs, PHY_ADDRESS, MII_BMSR)?;
    let bmsr = mdio_read(regs, PHY_ADDRESS, MII_BMSR)?;
    let status = mdio_read(regs, PHY_ADDRESS, MII_SPEC_STATUS)?;
    let speed_mbps = match (status >> YT8531_SPEED_SHIFT) & 0b11 {
        0 => 10,
        1 => 100,
        2 => 1000,
        _ => 0,
    };
    Ok(LinkState {
        up: bmsr & MII_BMSR_LINK_STATUS != 0 && status & YT8531_LINK_STATUS != 0,
        speed_mbps,
        full_duplex: status & YT8531_DUPLEX != 0,
    })
}

pub(super) fn wait_initial_link(regs: GmacMmio) -> LinkState {
    let start = get_time();
    let timeout = get_clock_freq().saturating_mul(3);
    loop {
        if let Ok(link) = read_link_state(regs) {
            if link.up {
                return link;
            }
        }
        if get_time().wrapping_sub(start) >= timeout {
            return LinkState {
                up: false,
                speed_mbps: 1000,
                full_duplex: true,
            };
        }
        core::hint::spin_loop();
    }
}
