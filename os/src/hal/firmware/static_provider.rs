//! Static (compile-time) firmware provider.
//!
//! Used as fallback when no FDT or ACPI is available.
//! Reads platform memory configuration from existing compile-time constants.

/// Re-export the fallback memory constants from the arch config module.
/// These are used when no firmware description is available.
pub use crate::config::{FIRMWARE_RESERVED_REGIONS_FALLBACK, MEMORY_REGIONS_FALLBACK};
