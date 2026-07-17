use user_lib::println;

use super::cli::CliArgs;
use super::config::LtpConfig;

pub fn print_plan(
    cli: &CliArgs,
    cfg: &LtpConfig,
    total_raw: usize,
    from_idx: Option<usize>,
    actual_start_idx: usize,
    filtered: usize,
    from_case: Option<&str>,
) {
    println!("[ltprunner] libc={}", cli.libc);
    println!("[ltprunner] ltproot={}", cli.ltproot);
    println!("[ltprunner] suites={}", cfg.ltp_suites.join(","));
    println!("[ltprunner] raw cases={}", total_raw);
    if !cfg.ltp_include.is_empty() {
        println!("[ltprunner] include={}", cfg.ltp_include.join(","));
    } else {
        println!("[ltprunner] include=none");
    }
    if !cfg.ltp_exclude.is_empty() {
        println!("[ltprunner] exclude={}", cfg.ltp_exclude.join(","));
    } else {
        println!("[ltprunner] exclude=none");
    }
    if !cfg.ltp_from.is_empty() {
        if let Some(idx) = from_idx {
            if let Some(name) = from_case {
                println!(
                    "[ltprunner] ltp_from={} matched case={} index={}",
                    cfg.ltp_from, name, idx
                );
            }
        } else {
            println!(
                "[ltprunner] ltp_from={} not found, starting from beginning",
                cfg.ltp_from
            );
        }
    }
    println!(
        "[ltprunner] filtered={} start_idx={}",
        filtered, actual_start_idx
    );
    println!("[ltprunner] group_timeout_secs={}", cli.group_timeout_secs);
    println!("[ltprunner] case_timeout_secs={}", cfg.case_timeout_secs);
}
