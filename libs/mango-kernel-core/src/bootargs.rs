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
    /// Root device path.
    pub root: String,
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
        } else if let Some(root) = parsed.get("mango.root") {
            cfg.root = String::from(root);
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
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn test_default_boot_mode_normal() {
        // Given: no explicit root or platform-derived root.
        let cfg = BootConfig::from_cmdline("");
        // When: the architecture-neutral command line is parsed.
        // Then: the embedded initramfs remains the safe root default.
        assert_eq!(cfg.mode, BootMode::Normal);
        assert_eq!(cfg.root, "initramfs");
    }

    #[test]
    fn test_ktest_mode() {
        let cfg = BootConfig::from_cmdline("mango.mode=ktest");
        assert_eq!(cfg.mode, BootMode::Ktest);
    }

    #[test]
    fn test_single_test_group() {
        let cfg = BootConfig::from_cmdline("mango.mode=ktest mango.test=waitqueue");
        assert_eq!(cfg.tests, vec![String::from("waitqueue")]);
    }

    #[test]
    fn test_multiple_test_groups_comma() {
        let cfg = BootConfig::from_cmdline("mango.test=waitqueue,sched,timer");
        assert_eq!(
            cfg.tests,
            vec![
                String::from("waitqueue"),
                String::from("sched"),
                String::from("timer"),
            ]
        );
    }

    #[test]
    fn test_all_tests() {
        let cfg = BootConfig::from_cmdline("mango.test=all");
        assert_eq!(cfg.tests, vec![String::from("all")]);
    }

    #[test]
    fn test_repeat_default_is_1() {
        let cfg = BootConfig::from_cmdline("");
        assert_eq!(cfg.repeat, 1);
    }

    #[test]
    fn test_repeat_custom() {
        let cfg = BootConfig::from_cmdline("mango.test.repeat=100");
        assert_eq!(cfg.repeat, 100);
    }

    #[test]
    fn test_repeat_clamped_to_min_1() {
        let cfg = BootConfig::from_cmdline("mango.test.repeat=0");
        assert_eq!(cfg.repeat, 1); // clamped
    }

    #[test]
    fn test_timeout_default() {
        let cfg = BootConfig::from_cmdline("");
        assert_eq!(cfg.timeout_ms, 5000);
    }

    #[test]
    fn test_timeout_custom() {
        let cfg = BootConfig::from_cmdline("mango.test.timeout_ms=10000");
        assert_eq!(cfg.timeout_ms, 10000);
    }

    #[test]
    fn test_timeout_clamped_to_min_100() {
        let cfg = BootConfig::from_cmdline("mango.test.timeout_ms=50");
        assert_eq!(cfg.timeout_ms, 100); // clamped
    }

    #[test]
    fn test_failfast_default_false() {
        let cfg = BootConfig::from_cmdline("");
        assert!(!cfg.failfast);
    }

    #[test]
    fn test_failfast_true() {
        let cfg = BootConfig::from_cmdline("mango.test.failfast=1");
        assert!(cfg.failfast);
    }

    #[test]
    fn test_failfast_true_alt() {
        let cfg = BootConfig::from_cmdline("mango.test.failfast=true");
        assert!(cfg.failfast);
    }

    #[test]
    fn test_trace_groups() {
        let cfg = BootConfig::from_cmdline("mango.trace=waitqueue,sched,timer");
        assert_eq!(
            cfg.trace_groups,
            vec![
                String::from("waitqueue"),
                String::from("sched"),
                String::from("timer"),
            ]
        );
    }

    #[test]
    fn test_init_override() {
        let cfg = BootConfig::from_cmdline("mango.init=/bin/bash");
        assert_eq!(cfg.init, "/bin/bash");
    }

    #[test]
    fn test_root_override() {
        let cfg = BootConfig::from_cmdline("mango.root=/dev/sda1");
        assert_eq!(cfg.root, "/dev/sda1");
    }

    #[test]
    fn test_standard_root_override() {
        let cfg = BootConfig::from_cmdline("root=/dev/sdb1");
        assert_eq!(cfg.root, "/dev/sdb1");
    }

    #[test]
    fn test_standard_root_takes_precedence_over_mango_root() {
        let cfg = BootConfig::from_cmdline("root=/dev/sdb1 mango.root=/dev/sda1");
        assert_eq!(cfg.root, "/dev/sdb1");
    }

    #[test]
    fn test_cmdline_parse_simple() {
        let cl = Cmdline::parse("key=value");
        assert_eq!(cl.get("key"), Some("value"));
    }

    #[test]
    fn test_cmdline_parse_flag() {
        let cl = Cmdline::parse("verbose");
        assert!(cl.has("verbose"));
        assert_eq!(cl.get("verbose"), Some(""));
    }

    #[test]
    fn test_cmdline_parse_multiple() {
        let cl = Cmdline::parse("a=1 b=2 c=3");
        assert_eq!(cl.get("a"), Some("1"));
        assert_eq!(cl.get("b"), Some("2"));
        assert_eq!(cl.get("c"), Some("3"));
    }

    #[test]
    fn test_cmdline_get_list() {
        let cl = Cmdline::parse("mango.test=waitqueue,sched");
        let list = cl.get_list("mango.test");
        assert_eq!(list, vec!["waitqueue", "sched"]);
    }

    #[test]
    fn test_cmdline_get_list_empty_value() {
        let cl = Cmdline::parse("mango.test=");
        let list = cl.get_list("mango.test");
        assert!(list.is_empty());
    }

    #[test]
    fn test_cmdline_get_usize() {
        let cl = Cmdline::parse("repeat=42");
        assert_eq!(cl.get_usize("repeat"), Some(42));
    }

    #[test]
    fn test_cmdline_get_usize_invalid() {
        let cl = Cmdline::parse("repeat=abc");
        assert_eq!(cl.get_usize("repeat"), None);
    }

    #[test]
    fn test_cmdline_get_bool_variants() {
        let cl = Cmdline::parse("a=1 b=true c=yes d=on e=0 f=false");
        assert!(cl.get_bool("a"));
        assert!(cl.get_bool("b"));
        assert!(cl.get_bool("c"));
        assert!(cl.get_bool("d"));
        assert!(!cl.get_bool("e"));
        assert!(!cl.get_bool("f"));
    }

    #[test]
    fn test_cmdline_empty_string() {
        let cl = Cmdline::parse("");
        assert_eq!(cl.get("anything"), None);
    }

    #[test]
    fn test_cmdline_missing_key() {
        let cl = Cmdline::parse("key=value");
        assert_eq!(cl.get("missing"), None);
    }

    #[test]
    fn test_complex_cmdline() {
        let cfg = BootConfig::from_cmdline(
            "mango.mode=ktest mango.test=waitqueue,sched mango.test.repeat=50 \
             mango.test.timeout_ms=3000 mango.test.failfast=1 mango.trace=sched \
             mango.init=/bin/sh mango.root=/dev/vda",
        );
        assert_eq!(cfg.mode, BootMode::Ktest);
        assert_eq!(
            cfg.tests,
            vec![String::from("waitqueue"), String::from("sched")]
        );
        assert_eq!(cfg.repeat, 50);
        assert_eq!(cfg.timeout_ms, 3000);
        assert!(cfg.failfast);
        assert_eq!(cfg.trace_groups, vec![String::from("sched")]);
        assert_eq!(cfg.init, "/bin/sh");
        assert_eq!(cfg.root, "/dev/vda");
    }
}
