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
    assert!(cfg.root_from_cmdline);
}

#[test]
fn test_explicit_initramfs_root_remains_distinguishable_from_default() {
    // Given: an explicit root= selector matching the default root name.
    let cfg = BootConfig::from_cmdline("root=initramfs");
    // When: boot arguments are parsed.
    // Then: the explicit selector is retained independently of its value.
    assert_eq!(cfg.root, "initramfs");
    assert!(cfg.root_from_cmdline);
}

#[test]
fn test_missing_root_selector_is_not_explicit() {
    // Given: command-line arguments without either root selector.
    let cfg = BootConfig::from_cmdline("mango.mode=normal");
    // When: boot arguments are parsed.
    // Then: initramfs remains the default, but no selector was requested.
    assert_eq!(cfg.root, "initramfs");
    assert!(!cfg.root_from_cmdline);
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
