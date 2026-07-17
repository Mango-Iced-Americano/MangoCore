#![no_std]
#![no_main]

extern crate alloc;

#[path = "ltprunner/mod.rs"]
mod ltp_runner;

use alloc::vec::Vec;
use ltp_runner::{
    get_time_ms, load_conf, parse_cli, parse_suite_file, precompute_env, print_plan, reap_orphans,
    run_case, LtpCase, LwExt4PerfDiag,
};
use user_lib::{getpgid, println};

const LTP_EXIT_TCONF: i32 = 32;

#[no_mangle]
fn main(_argc: usize, argv: &[&str]) -> i32 {
    let cli = parse_cli(argv);

    if cli.libc.is_empty() {
        println!("[ltprunner] error: --libc is required");
        return 1;
    }
    if cli.ltproot.is_empty() {
        println!("[ltprunner] error: --ltproot is required");
        return 1;
    }

    let cfg = load_conf(&cli.conf_path, &cli.libc);
    if cfg.ltp_suites.is_empty() {
        println!(
            "[ltprunner] error: ltp_suites is empty in config {}",
            cli.conf_path
        );
        return 2;
    }

    let own_pgid = getpgid(0);
    let penv = precompute_env(&cli.ltproot, &cli.tmpdir, &cli.libc);
    let mut raw_cases: Vec<LtpCase> = Vec::new();
    for suite in &cfg.ltp_suites {
        if suite.is_empty() {
            continue;
        }
        parse_suite_file(&cli.ltproot, suite, &mut raw_cases);
    }

    if raw_cases.is_empty() {
        println!("[ltprunner] warning: no cases found in any suite");
        return 0;
    }

    let from_idx: Option<usize> = if !cfg.ltp_from.is_empty() {
        let mut found: Option<usize> = None;
        for (i, case) in raw_cases.iter().enumerate() {
            if case.case_name == cfg.ltp_from {
                if let Some(first_index) = found {
                    println!(
                        "[ltprunner] warning: ltp_from={} matches multiple cases, using first at index {}",
                        cfg.ltp_from,
                        first_index
                    );
                } else {
                    found = Some(i);
                }
            }
        }
        found
    } else {
        Some(0)
    };

    let include_empty = cfg.ltp_include.is_empty();
    let from_idx_val = from_idx.unwrap_or(0);
    let mut from_case_name: Option<&str> = None;
    let mut filtered_indices: Vec<usize> = Vec::new();
    for (i, case) in raw_cases.iter().enumerate() {
        if i < from_idx_val {
            continue;
        }
        if i == from_idx_val {
            from_case_name = Some(&case.case_name);
        }
        if !include_empty && !cfg.ltp_include.iter().any(|e| e == &case.case_name) {
            continue;
        }
        if cfg.ltp_exclude.iter().any(|e| e == &case.case_name) {
            println!(
                "[ltprunner] skip excluded case {} in suite {}",
                case.case_name, case.suite
            );
            continue;
        }
        filtered_indices.push(i);
    }

    let filtered_count = filtered_indices.len();
    let actual_start_idx = filtered_indices.first().copied().unwrap_or(0);
    print_plan(
        &cli,
        &cfg,
        raw_cases.len(),
        from_idx,
        actual_start_idx,
        filtered_count,
        from_case_name,
    );

    let mut lwext4_perf = LwExt4PerfDiag::new(cfg.lwext4_perf_log);

    if filtered_count == 0 {
        println!("[ltprunner] no cases to run after filtering");
        return 0;
    }

    let start_ms = get_time_ms();
    let deadline_ms = start_ms + cli.group_timeout_secs * 1000;
    let mut executed: usize = 0;
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut skipped: usize = 0;
    let mut current_suite: &str = "";

    for &idx in &filtered_indices {
        let case = &raw_cases[idx];

        let now = get_time_ms();
        if now >= deadline_ms {
            println!(
                "[ltprunner] group deadline exceeded, stopping. executed={} passed={} failed={} remaining={}",
                executed, passed, failed, filtered_count - executed
            );
            skipped += filtered_count - executed;
            break;
        }

        if case.suite != current_suite {
            current_suite = &case.suite;
            println!("=== LTP SUITE {} ===", current_suite);
        }

        println!(
            "[ltprunner] #{} suite={} case={}",
            idx, case.suite, case.case_name
        );
        println!("RUN LTP CASE {}", case.case_name);

        let before = lwext4_perf.before_case();
        let ret = run_case(case, deadline_ms, own_pgid, &penv, cfg.case_timeout_secs);
        lwext4_perf.after_case(before, idx, ret);

        let label = if ret == 0 {
            passed += 1;
            "PASS"
        } else if ret == LTP_EXIT_TCONF {
            skipped += 1;
            "SKIP"
        } else {
            failed += 1;
            "FAIL"
        };
        println!("FAIL LTP CASE {} : {}", case.case_name, ret);
        println!("LTP CASE RESULT {} : {} ({})", case.case_name, label, ret);
        executed += 1;

        reap_orphans();
    }

    println!(
        "[ltprunner] done. executed={} passed={} failed={} skipped={} total_ms={} rate={} cases/min",
        executed, passed, failed, skipped,
        get_time_ms() - start_ms,
        (executed as u64 * 60000).checked_div(get_time_ms() - start_ms).unwrap_or(0),
    );

    0
}
