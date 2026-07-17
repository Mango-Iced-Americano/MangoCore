use user_lib::{close, open, read, OpenFlags};

const FEATURES_PATH: &str = "/sys/kernel/stats/features\0";
const LWEXT4_PATH: &str = "/sys/kernel/stats/lwext4\0";
pub(super) const COUNTER_COUNT: usize = 24;
pub(super) const COUNTER_KEYS: [&str; COUNTER_COUNT] = [
    "lwext4_find_calls",
    "lwext4_find_cycles",
    "lwext4_probe_type_calls",
    "lwext4_probe_type_cycles",
    "lwext4_get_inode_id_calls",
    "lwext4_get_inode_id_enoint",
    "lwext4_get_inode_id_cycles",
    "lwext4_metadata_cold",
    "lwext4_metadata_hot",
    "lwext4_metadata_cold_cycles",
    "lwext4_file_open_calls",
    "lwext4_file_open_cycles",
    "lwext4_file_size_calls",
    "lwext4_file_close_calls",
    "lwext4_file_close_cycles",
    "lwext4_dir_entries_calls",
    "lwext4_dir_entries_cycles",
    "lwext4_create_pre_check",
    "lwext4_logical_size_calls",
    "lwext4_logical_size_cycles",
    "lwext4_ensure_pc_calls",
    "lwext4_find_cache_hit",
    "lwext4_find_cache_miss",
    "lwext4_ensure_pc_creates",
];

#[derive(Clone, Copy)]
pub struct Snapshot {
    values: [u64; COUNTER_COUNT],
}

#[derive(Clone, Copy)]
pub(super) enum SnapshotError {
    Read,
    StrictParse,
}

pub(super) fn perf_diag_enabled() -> bool {
    let mut buffer = [0; 128];
    let Some(contents) = read_file(FEATURES_PATH, &mut buffer) else {
        return false;
    };
    contents.split(|byte| *byte == b'\n').any(|line| {
        let Some(equals) = line.iter().position(|byte| *byte == b'=') else {
            return false;
        };
        trim_ascii(&line[..equals]) == b"perf_diag" && trim_ascii(&line[equals + 1..]) == b"true"
    })
}

pub(super) fn read_lwext4_snapshot() -> Result<Snapshot, SnapshotError> {
    let mut buffer = [0; 2048];
    let contents = read_file(LWEXT4_PATH, &mut buffer).ok_or(SnapshotError::Read)?;
    parse_snapshot(contents).ok_or(SnapshotError::StrictParse)
}

impl Snapshot {
    pub(super) fn wrapping_delta(self, before: Self) -> [u64; COUNTER_COUNT] {
        let mut deltas = [0; COUNTER_COUNT];
        for index in 0..COUNTER_COUNT {
            deltas[index] = self.values[index].wrapping_sub(before.values[index]);
        }
        deltas
    }
}

fn read_file<'a>(path: &str, buffer: &'a mut [u8]) -> Option<&'a [u8]> {
    let fd = open(path, OpenFlags::RDONLY);
    if fd < 0 {
        return None;
    }
    let mut length = 0;
    let result = loop {
        if length == buffer.len() {
            break None;
        }
        let count = read(fd as usize, &mut buffer[length..]);
        if count < 0 {
            break None;
        }
        if count == 0 {
            break if length == 0 {
                None
            } else {
                Some(&buffer[..length])
            };
        }
        let Ok(count) = usize::try_from(count) else {
            break None;
        };
        if count > buffer.len() - length {
            break None;
        }
        length += count;
    };
    let _ = close(fd as usize);
    result
}

fn parse_snapshot(contents: &[u8]) -> Option<Snapshot> {
    let mut values = [0; COUNTER_COUNT];
    let mut seen = [false; COUNTER_COUNT];
    for raw_line in contents.split(|byte| *byte == b'\n') {
        let line = trim_ascii(raw_line);
        if line.is_empty() {
            continue;
        }
        let equals = line.iter().position(|byte| *byte == b'=')?;
        let key = trim_ascii(&line[..equals]);
        let Some(index) = COUNTER_KEYS.iter().position(|name| name.as_bytes() == key) else {
            continue;
        };
        if seen[index] {
            return None;
        }
        values[index] = parse_u64(trim_ascii(&line[equals + 1..]))?;
        seen[index] = true;
    }
    if seen.iter().all(|value| *value) {
        Some(Snapshot { values })
    } else {
        None
    }
}

fn parse_u64(input: &[u8]) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let mut value = 0u64;
    for byte in input {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    Some(value)
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t' | b'\r')) {
        bytes = &bytes[1..];
    }
    while matches!(bytes.last(), Some(b' ' | b'\t' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}
