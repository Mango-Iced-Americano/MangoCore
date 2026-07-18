use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user_lib::{close, open, println, read, OpenFlags};

pub struct LtpCase {
    pub index: usize,
    pub suite: String,
    pub case_name: String,
    pub command: String,
}

fn parse_runtest_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let space_pos = trimmed.find(|c: char| c == ' ' || c == '\t')?;
    let case_name = String::from(trimmed[..space_pos].trim());
    let command = String::from(trimmed[space_pos..].trim());
    if case_name.is_empty() || command.is_empty() {
        return None;
    }
    Some((case_name, command))
}

pub fn parse_suite_file(ltproot: &str, suite: &str, cases: &mut Vec<LtpCase>) -> bool {
    let path = format!("{}/runtest/{}\0", ltproot, suite);
    let fd = open(&path, OpenFlags::RDONLY);
    if fd < 0 {
        println!(
            "[ltprunner] warning: suite '{}' not found at {}",
            suite,
            path.trim_end_matches('\0')
        );
        return false;
    }

    let mut content = Vec::new();
    let mut tmp_buf = [0u8; 1024];
    loop {
        let n = read(fd as usize, &mut tmp_buf);
        if n <= 0 {
            break;
        }
        content.extend_from_slice(&tmp_buf[..n as usize]);
    }
    let _ = close(fd as usize);

    let text = core::str::from_utf8(&content).unwrap_or("");
    let base_idx = cases.len();
    for line in text.lines() {
        if let Some((case_name, command)) = parse_runtest_line(line) {
            cases.push(LtpCase {
                index: base_idx + cases.len() - base_idx,
                suite: String::from(suite),
                case_name,
                command,
            });
        }
    }

    println!(
        "[ltprunner] suite '{}': parsed {} cases",
        suite,
        cases.len() - base_idx
    );
    true
}
