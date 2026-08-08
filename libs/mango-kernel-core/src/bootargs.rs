//! MangoCore bootargs / kernel command line parser.
//!
//! Parses `key=value` pairs from a flat command-line string (space-separated).
//! Supports comma-separated list values and flag-style keys (no `=`).
//!
//! # Design
//!
//! This module is **architecture-agnostic**: it only operates on `&str`.
//! How the command line string is obtained (DTB `/chosen/bootargs`, EFI,
//! compile-time constant, etc.) is the responsibility of the caller.
//!
//! # Format
//!
//! ```text
//! mango.mode=ktest mango.test=waitqueue,sched mango.test.repeat=100 verbose
//! ```
//!
//! Rules:
//! - Space-separated tokens
//! - `key=value` pairs (value may be empty)
//! - Comma-separated list values
//! - Bare tokens (no `=`) are treated as flag-style keys with value `""`
//! - No quoting, no escaping, no nested structures

use alloc::string::String;
use alloc::vec::Vec;

// ─────────────────────────────────────────────────────────
//  BootConfig — parsed kernel configuration from command line
// ─────────────────────────────────────────────────────────

/// Operating mode determined from `mango.mode=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    /// Normal boot: start user-mode init as usual.
    Normal,
    /// Kernel self-test mode: run L3 tests, then shutdown.
    Ktest,
    /// User-mode regression test mode (future).
    Regression,
}

/// Parsed kernel configuration from bootargs.
#[derive(Debug, Clone)]
pub struct BootConfig {
    pub mode: BootMode,
    /// Test groups to run (e.g. ["waitqueue", "sched"] or ["all"]).
    pub tests: Vec<String>,
    /// Number of times to repeat each test.
    pub repeat: usize,
    /// Global per-test timeout in milliseconds.
    pub timeout_ms: usize,
    /// Stop on first failure.
    pub failfast: bool,
    /// Trace groups to enable (e.g. ["waitqueue", "sched"]).
    pub trace_groups: Vec<String>,
    /// Path to init binary (for normal mode).
    pub init: String,
    /// Root selector for the volume mounted at /sdcard.
    pub root: String,
    /// Whether root was explicitly selected with root= or mango.root=.
    pub root_from_cmdline: bool,
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            mode: BootMode::Normal,
            tests: Vec::new(),
            repeat: 1,
            timeout_ms: 5000,
            failfast: false,
            trace_groups: Vec::new(),
            init: String::from("/init"),
            root: String::from("initramfs"),
            root_from_cmdline: false,
        }
    }
}

impl BootConfig {
    /// Parse configuration from a command-line string.
    pub fn from_cmdline(cmdline: &str) -> Self {
        let parsed = Cmdline::parse(cmdline);
        let mut cfg = Self::default();

        // mango.mode=
        match parsed.get("mango.mode") {
            Some("ktest") => cfg.mode = BootMode::Ktest,
            Some("regression") => cfg.mode = BootMode::Regression,
            _ => cfg.mode = BootMode::Normal,
        }

        // mango.test=
        let test_vals = parsed.get_list("mango.test");
        if !test_vals.is_empty() {
            cfg.tests = test_vals.iter().map(|s| String::from(*s)).collect();
        }

        // mango.test.repeat=N
        if let Some(n) = parsed.get_usize("mango.test.repeat") {
            cfg.repeat = n.max(1);
        }

        // mango.test.timeout_ms=N
        if let Some(n) = parsed.get_usize("mango.test.timeout_ms") {
            cfg.timeout_ms = n.max(100);
        }

        // mango.test.failfast=1
        cfg.failfast = parsed.get_bool("mango.test.failfast");

        // mango.trace=
        let trace_vals = parsed.get_list("mango.trace");
        if !trace_vals.is_empty() {
            cfg.trace_groups = trace_vals.iter().map(|s| String::from(*s)).collect();
        }

        // mango.init=
        if let Some(init) = parsed.get("mango.init") {
            cfg.init = String::from(init);
        }

        // Standard Linux root= takes precedence over the project-specific fallback.
        if let Some(root) = parsed.get("root") {
            cfg.root = String::from(root);
            cfg.root_from_cmdline = true;
        } else if let Some(root) = parsed.get("mango.root") {
            cfg.root = String::from(root);
            cfg.root_from_cmdline = true;
        }

        cfg
    }
}

// ─────────────────────────────────────────────────────────
//  Cmdline — low-level key=value parser
// ─────────────────────────────────────────────────────────

/// Parsed key-value pairs from a command line string.
#[derive(Debug, Clone)]
pub struct Cmdline {
    pairs: Vec<(String, String)>,
}

impl Cmdline {
    /// Parse a flat command-line string.
    pub fn parse(input: &str) -> Self {
        let pairs: Vec<_> = input
            .split_whitespace()
            .filter_map(|token| {
                if token.is_empty() {
                    return None;
                }
                if let Some((key, value)) = token.split_once('=') {
                    Some((String::from(key), String::from(value)))
                } else {
                    // Flag-style: key with empty value
                    Some((String::from(token), String::new()))
                }
            })
            .collect();
        Self { pairs }
    }

    /// Get the first value for a key, or `None`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Get all values for a key as a list.
    /// If the value contains commas, it is split; otherwise returns a single-element slice.
    pub fn get_list(&self, key: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == key)
            .flat_map(|(_, v)| {
                if v.is_empty() {
                    Vec::new()
                } else {
                    v.split(',').filter(|s| !s.is_empty()).collect()
                }
            })
            .collect()
    }

    /// Get value parsed as `usize`, or `None`.
    pub fn get_usize(&self, key: &str) -> Option<usize> {
        self.get(key).and_then(|v| v.parse().ok())
    }

    /// Get value parsed as `bool` ("1", "true", "yes" → true).
    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key)
            .map(|v| matches!(v, "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }

    /// Check if a key exists (even with empty value).
    pub fn has(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == key)
    }
}

// ─────────────────────────────────────────────────────────
//  Unit tests (L1 — run with cargo test on host)
// ─────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "bootargs_tests.rs"]
mod tests;
