use user_lib::{close, open, println, read, OpenFlags};

const ANOTHER_EXT4_PATH: &str = "/sys/kernel/stats/another_ext4\0";
const PAGECACHE_PATH: &str = "/sys/kernel/stats/pagecache\0";
const BLOCKIO_PATH: &str = "/sys/kernel/stats/blockio\0";
const SNAPSHOT_INTERVAL_MS: u64 = 60_000;

#[derive(Clone, Copy, Default)]
struct Snapshot {
    prepare_calls: u64,
    prepare_cycles: u64,
    inode_read_calls: u64,
    inode_read_cycles: u64,
    extent_query_calls: u64,
    extent_query_cycles: u64,
    allocation_calls: u64,
    allocation_cycles: u64,
    inode_persist_calls: u64,
    inode_persist_cycles: u64,
    lock_wait_calls: u64,
    lock_wait_cycles: u64,
    lock_hold_calls: u64,
    lock_hold_cycles: u64,
    prepare_failures: u64,
    pc_writeback_calls: u64,
    pc_writeback_pages: u64,
    pc_writeback_cycles: u64,
    block_read_requests: u64,
    block_write_requests: u64,
}

pub(super) struct Session<'a> {
    run_id: &'a str,
    backend: &'a str,
    previous: Snapshot,
    last_emit_ms: u64,
    sequence: usize,
}

impl<'a> Session<'a> {
    pub(super) fn begin(run_id: &'a str, backend: &'a str, now_ms: u64) -> Option<Self> {
        let snapshot = Snapshot::read()?;
        let mut session = Self {
            run_id,
            backend,
            previous: snapshot,
            last_emit_ms: now_ms,
            sequence: 0,
        };
        session.emit("before", snapshot);
        Some(session)
    }

    pub(super) fn emit_if_due(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_emit_ms) >= SNAPSHOT_INTERVAL_MS {
            self.emit_current("interval", now_ms);
        }
    }

    pub(super) fn emit_timeout(&mut self, now_ms: u64) {
        self.emit_current("timeout", now_ms);
    }

    fn emit_current(&mut self, reason: &str, now_ms: u64) {
        match Snapshot::read() {
            Some(snapshot) => self.emit(reason, snapshot),
            None => println!(
                "[ext4-ab] diag-unavailable run_id={} backend={} reason={}",
                self.run_id, self.backend, reason
            ),
        }
        self.last_emit_ms = now_ms;
    }

    fn emit(&mut self, reason: &str, current: Snapshot) {
        let delta = current.delta(self.previous);
        println!(
            "[ext4-ab] diag-delta run_id={} backend={} seq={} reason={} prep_calls={} prep_cycles={} inode_read_calls={} inode_read_cycles={} extent_query_calls={} extent_query_cycles={} allocation_calls={} allocation_cycles={} inode_persist_calls={} inode_persist_cycles={} lock_wait_calls={} lock_wait_cycles={} lock_hold_calls={} lock_hold_cycles={} prepare_failures={} pc_wb_calls={} pc_wb_pages={} pc_wb_cycles={} blk_read_reqs={} blk_write_reqs={}",
            self.run_id,
            self.backend,
            self.sequence,
            reason,
            delta.prepare_calls,
            delta.prepare_cycles,
            delta.inode_read_calls,
            delta.inode_read_cycles,
            delta.extent_query_calls,
            delta.extent_query_cycles,
            delta.allocation_calls,
            delta.allocation_cycles,
            delta.inode_persist_calls,
            delta.inode_persist_cycles,
            delta.lock_wait_calls,
            delta.lock_wait_cycles,
            delta.lock_hold_calls,
            delta.lock_hold_cycles,
            delta.prepare_failures,
            delta.pc_writeback_calls,
            delta.pc_writeback_pages,
            delta.pc_writeback_cycles,
            delta.block_read_requests,
            delta.block_write_requests,
        );
        self.previous = current;
        self.sequence = self.sequence.saturating_add(1);
    }
}

impl Snapshot {
    fn read() -> Option<Self> {
        let mut another_ext4 = [0u8; 4096];
        let mut pagecache = [0u8; 2048];
        let mut blockio = [0u8; 512];
        let another_ext4 = read_file(ANOTHER_EXT4_PATH, &mut another_ext4)?;
        let pagecache = read_file(PAGECACHE_PATH, &mut pagecache)?;
        let blockio = read_file(BLOCKIO_PATH, &mut blockio)?;
        Self::from_sysfs(another_ext4, pagecache, blockio)
    }

    fn from_sysfs(another_ext4: &[u8], pagecache: &[u8], blockio: &[u8]) -> Option<Self> {
        let mut snapshot = Self::default();
        let mut found = false;
        for line in another_ext4.split(|byte| *byte == b'\n') {
            if value(line, b"enabled") != Some(1) {
                continue;
            }
            found = true;
            snapshot.prepare_calls = snapshot.prepare_calls.checked_add(value(line, b"calls")?)?;
            snapshot.prepare_cycles = snapshot
                .prepare_cycles
                .checked_add(value(line, b"elapsed_cycles")?)?;
            snapshot.inode_read_calls = snapshot
                .inode_read_calls
                .checked_add(value(line, b"inode_read_calls")?)?;
            snapshot.inode_read_cycles = snapshot
                .inode_read_cycles
                .checked_add(value(line, b"inode_read_cycles")?)?;
            snapshot.extent_query_calls = snapshot
                .extent_query_calls
                .checked_add(value(line, b"extent_query_calls")?)?;
            snapshot.extent_query_cycles = snapshot
                .extent_query_cycles
                .checked_add(value(line, b"extent_query_cycles")?)?;
            snapshot.allocation_calls = snapshot
                .allocation_calls
                .checked_add(value(line, b"allocation_calls")?)?;
            snapshot.allocation_cycles = snapshot
                .allocation_cycles
                .checked_add(value(line, b"allocation_cycles")?)?;
            snapshot.inode_persist_calls = snapshot
                .inode_persist_calls
                .checked_add(value(line, b"inode_persist_calls")?)?;
            snapshot.inode_persist_cycles = snapshot
                .inode_persist_cycles
                .checked_add(value(line, b"inode_persist_cycles")?)?;
            snapshot.lock_wait_calls = snapshot
                .lock_wait_calls
                .checked_add(value(line, b"lock_wait_calls")?)?;
            snapshot.lock_wait_cycles = snapshot
                .lock_wait_cycles
                .checked_add(value(line, b"lock_wait_cycles")?)?;
            snapshot.lock_hold_calls = snapshot
                .lock_hold_calls
                .checked_add(value(line, b"lock_hold_calls")?)?;
            snapshot.lock_hold_cycles = snapshot
                .lock_hold_cycles
                .checked_add(value(line, b"lock_hold_cycles")?)?;
            snapshot.prepare_failures = snapshot
                .prepare_failures
                .checked_add(value(line, b"failures")?)?;
        }
        if !found {
            return None;
        }
        snapshot.pc_writeback_calls = value(pagecache, b"pc_wb_calls")?;
        snapshot.pc_writeback_pages = value(pagecache, b"pc_wb_pages")?;
        snapshot.pc_writeback_cycles = value(pagecache, b"pc_wb_cycles")?;
        snapshot.block_read_requests = value(blockio, b"blk_vread_reqs")?;
        snapshot.block_write_requests = value(blockio, b"blk_vwrite_reqs")?;
        Some(snapshot)
    }

    fn delta(self, before: Self) -> Self {
        Self {
            prepare_calls: self.prepare_calls.wrapping_sub(before.prepare_calls),
            prepare_cycles: self.prepare_cycles.wrapping_sub(before.prepare_cycles),
            inode_read_calls: self.inode_read_calls.wrapping_sub(before.inode_read_calls),
            inode_read_cycles: self
                .inode_read_cycles
                .wrapping_sub(before.inode_read_cycles),
            extent_query_calls: self
                .extent_query_calls
                .wrapping_sub(before.extent_query_calls),
            extent_query_cycles: self
                .extent_query_cycles
                .wrapping_sub(before.extent_query_cycles),
            allocation_calls: self.allocation_calls.wrapping_sub(before.allocation_calls),
            allocation_cycles: self
                .allocation_cycles
                .wrapping_sub(before.allocation_cycles),
            inode_persist_calls: self
                .inode_persist_calls
                .wrapping_sub(before.inode_persist_calls),
            inode_persist_cycles: self
                .inode_persist_cycles
                .wrapping_sub(before.inode_persist_cycles),
            lock_wait_calls: self.lock_wait_calls.wrapping_sub(before.lock_wait_calls),
            lock_wait_cycles: self.lock_wait_cycles.wrapping_sub(before.lock_wait_cycles),
            lock_hold_calls: self.lock_hold_calls.wrapping_sub(before.lock_hold_calls),
            lock_hold_cycles: self.lock_hold_cycles.wrapping_sub(before.lock_hold_cycles),
            prepare_failures: self.prepare_failures.wrapping_sub(before.prepare_failures),
            pc_writeback_calls: self
                .pc_writeback_calls
                .wrapping_sub(before.pc_writeback_calls),
            pc_writeback_pages: self
                .pc_writeback_pages
                .wrapping_sub(before.pc_writeback_pages),
            pc_writeback_cycles: self
                .pc_writeback_cycles
                .wrapping_sub(before.pc_writeback_cycles),
            block_read_requests: self
                .block_read_requests
                .wrapping_sub(before.block_read_requests),
            block_write_requests: self
                .block_write_requests
                .wrapping_sub(before.block_write_requests),
        }
    }
}

fn read_file<'a>(path: &str, buffer: &'a mut [u8]) -> Option<&'a [u8]> {
    let fd = open(path, OpenFlags::RDONLY);
    if fd < 0 {
        return None;
    }
    let mut length = 0;
    loop {
        if length == buffer.len() {
            let _ = close(fd as usize);
            return None;
        }
        let count = read(fd as usize, &mut buffer[length..]);
        if count <= 0 {
            let _ = close(fd as usize);
            return if count == 0 && length > 0 {
                Some(&buffer[..length])
            } else {
                None
            };
        }
        let count = usize::try_from(count).ok()?;
        length = length.checked_add(count)?;
    }
}

fn value(contents: &[u8], wanted: &[u8]) -> Option<u64> {
    for token in contents.split(|byte| byte.is_ascii_whitespace()) {
        let equals = token.iter().position(|byte| *byte == b'=')?;
        if &token[..equals] == wanted {
            return parse_u64(&token[equals + 1..]);
        }
    }
    None
}

fn parse_u64(input: &[u8]) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let mut result = 0u64;
    for byte in input {
        if !byte.is_ascii_digit() {
            return None;
        }
        result = result
            .checked_mul(10)?
            .checked_add(u64::from(*byte - b'0'))?;
    }
    Some(result)
}
