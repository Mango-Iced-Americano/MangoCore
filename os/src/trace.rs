use crate::timer::get_time_us;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// Number of trace entries in the ring buffer.
pub(crate) const TRACE_SIZE: usize = 2048;

/// Magic key: Ctrl+T (0x14). Pressing this key triggers a trace dump.
pub const MAGIC_KEY: u8 = 0x14;

/// ── Character stash: non-magic chars consumed by scheduler get stored here ──
/// TTY reads from stash first, then falls back to console_getchar().
/// This prevents the scheduler loop from swallowing user input.

/// A simple ring buffer for stashing characters read by the scheduler.
struct CharStash {
    buf: [u8; 128],
    head: usize,
    tail: usize,
}

impl CharStash {
    fn push(&mut self, ch: u8) {
        let next = (self.head + 1) % self.buf.len();
        self.buf[self.head] = ch;
        self.head = next;
        if next == self.tail {
            // buffer full — advance tail to drop oldest char
            self.tail = (self.tail + 1) % self.buf.len();
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            None
        } else {
            let ch = self.buf[self.tail];
            self.tail = (self.tail + 1) % self.buf.len();
            Some(ch)
        }
    }
}

/// Global character stash. Chars read by try_dump_from go here if not magic.
static CHAR_STASH: Mutex<CharStash> = Mutex::new(CharStash {
    buf: [0; 128],
    head: 0,
    tail: 0,
});

/// Pop a stashed character (consumed by TTY).
pub fn pop_stashed() -> Option<u8> {
    CHAR_STASH.lock().pop()
}

/// Stash a character for TTY to consume later.
pub fn stash_char(ch: u8) {
    CHAR_STASH.lock().push(ch);
}

/// Bit mask for syscall return trace events.
/// A trace entry with `tag & TRACE_RET_MASK != 0` is a return event;
/// the syscall ID is `tag & !TRACE_RET_MASK`.
pub const TRACE_RET_MASK: u64 = 0x8000_0000_0000_0000;

/// Dump lock: prevents re-entrant trace dumps.
static DUMP_LOCK: AtomicBool = AtomicBool::new(false);

/// A single trace event: timestamp (µs) + tag + six u64 payload fields.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceEntry {
    pub timestamp: u64,
    pub tag: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
    pub arg6: u64,
}

/// The inner ring buffer state, always accessed behind a Mutex.
struct RingInner {
    buf: [TraceEntry; TRACE_SIZE],
    write_pos: usize,
}

impl RingInner {
    fn push(&mut self, entry: TraceEntry) {
        let pos = self.write_pos % TRACE_SIZE;
        self.buf[pos] = entry;
        self.write_pos = self.write_pos.wrapping_add(1);
    }

    fn count(&self) -> usize {
        if self.write_pos >= TRACE_SIZE {
            TRACE_SIZE
        } else {
            self.write_pos
        }
    }

    fn start(&self) -> usize {
        if self.write_pos >= TRACE_SIZE {
            self.write_pos % TRACE_SIZE
        } else {
            0
        }
    }

    fn clear(&mut self) {
        self.write_pos = 0;
    }
}

/// Global trace ring buffer.
static TRACE: Mutex<RingInner> = Mutex::new(RingInner {
    buf: [TraceEntry {
        timestamp: 0,
        tag: 0,
        arg1: 0,
        arg2: 0,
        arg3: 0,
        arg4: 0,
        arg5: 0,
        arg6: 0,
    }; TRACE_SIZE],
    write_pos: 0,
});

/// Runtime trace on/off switch. Writable via /sys/kernel/tracing/tracing_on.
/// When false, trace events are silently dropped.
pub static TRACING_ON: AtomicBool = AtomicBool::new(true);

/// Count of events dropped because TRACING_ON was false or ring was full.
pub static TRACE_DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Resource scan events (buddy histogram, zombie grouping, heap trace)
pub const HTRACE_RESOURCE_BASE: u64 = 0xD000;



/// Decode a tag to a human-readable short label.
pub(crate) fn tag_name(tag: u64) -> &'static str {
    // Syscall IDs — try the syscall name table first (works for any ID range)
    let name = crate::syscall::syscall_name(tag as usize);
    if name != "unknown" {
        return name;
    }
    // Custom trace tags
    match tag {
        0xA001 => "accept:found",
        0xA002 => "accept:removed",
        0xA003 => "accept:added",
        0xC000 => "connect:enter",
        0xC001 => "connect:REFUSED",
        0xC002 => "connect:EAGAIN",
        0xC003 => "connect:ok",
        0xC004 => "try_connect:CLOSED",
        0xC005 => "try_connect:ESTAB",
        // pselect/accept/connect debug (0xB range)
        0xB000 => "pselect:loop",
        0xB001 => "rdy:enter",
        0xB002 => "rdy:handler",
        0xB003 => "rdy:connected",
        0xB004 => "rdy:result",
        0xB010 => "accept:scan",
        0xB011 => "accept:sstate",
        0xB012 => "accept:found",
        0xB020 => "conn:connect",
        0xB021 => "conn:inited",
        0xB030 => "conn:trycn",
        0xB031 => "conn:state",
        0xB032 => "conn:result",
        0xB033 => "conn:poll",
        0xB034 => "conn:lsten",
        0xB035 => "conn:sset",
        0xB036 => "poll:progressed",
        0xB040 => "a4:enter",
        0xB041 => "a4:ret",
        0xB042 => "a4:dispatched",
        _ => "?",
    }
}

/// Print a single trace entry. Handles both syscall entry and return events.
fn print_entry(entry: &TraceEntry) {
    let sec = entry.timestamp / 1_000_000;
    let us = entry.timestamp % 1_000_000;
    if entry.tag & TRACE_RET_MASK != 0 {
        // Syscall return trace — show the syscall name + ":ret".
        let sys_id = (entry.tag & !TRACE_RET_MASK) as usize;
        let name = crate::syscall::syscall_name(sys_id);
        let ret = entry.arg1 as isize;
        let is_err = entry.arg2 != 0;
        if is_err {
            crate::println!("[trace] [{}.{:06}] {}:ret -> {} (err)", sec, us, name, ret,);
        } else {
            crate::println!(
                "[trace] [{}.{:06}] {}:ret -> 0x{:X}",
                sec,
                us,
                name,
                entry.arg1,
            );
        }
    } else {
        // Normal entry — show tag name + raw args.
        let name = tag_name(entry.tag);
        crate::println!(
            "[trace] [{}.{:06}] {}(0x{:04X}) a1=0x{:X} a2=0x{:X} a3=0x{:X} a4=0x{:X} a5=0x{:X} a6=0x{:X}",
            sec,
            us,
            name,
            entry.tag,
            entry.arg1,
            entry.arg2,
            entry.arg3,
            entry.arg4,
            entry.arg5,
            entry.arg6,
        );
    }
}

/// Dump all entries to serial, oldest first.
fn inner_dump() {
    let ring = TRACE.lock();
    let count = ring.count();
    let start = ring.start();

    crate::println!("[trace] ── dump start ({} entries) ──", count);
    crate::println!("[trace] Legend: tag=NAME(hex)  a1..a5=args");
    for i in 0..count {
        let entry = &ring.buf[(start + i) % TRACE_SIZE];
        print_entry(entry);
    }
    crate::println!("[trace] ── dump end ──");
}

fn inner_clear() {
    match TRACE.try_lock() {
        Some(mut ring) => ring.clear(),
        None => {
            // If we can't acquire the lock, it means a dump is in progress.
            // We can safely skip clearing since the dump will read the old entries anyway.
        }
    }
}

/// Format the ring buffer into a String for sysfs read.
/// Held lock during formatting; capped to `max_entries` to bound allocation
/// (each entry ≈160 bytes; 512 entries ≈ 80KB, well within kernel heap).
pub(crate) fn dump_to_string(max_entries: usize) -> alloc::string::String {
    let ring = TRACE.lock();
    let count = ring.count();
    let start = ring.start();
    let dump_count = count.min(max_entries);
    // If capped, start from the most recent entries.
    let offset = count.saturating_sub(dump_count);

    let cap = dump_count.saturating_mul(160).min(65536);
    let mut s = alloc::string::String::with_capacity(cap);

    use core::fmt::Write;
    let _ = writeln!(s, "# trace buffer: {} entries (ring size {}), showing {}", count, TRACE_SIZE, dump_count);
    let _ = writeln!(s, "# format: timestamp_us tag=HEX a1..a6");

    for i in 0..dump_count {
        let e = &ring.buf[(start + offset + i) % TRACE_SIZE];
        let sec = e.timestamp / 1_000_000;
        let us = e.timestamp % 1_000_000;
        let name = tag_name(e.tag);
        let _ = writeln!(
            s,
            "[{}.{:06}] {:16} tag=0x{:04X} a1=0x{:X} a2=0x{:X} a3=0x{:X} a4=0x{:X} a5=0x{:X} a6=0x{:X}",
            sec, us,
            name,
            e.tag,
            e.arg1, e.arg2, e.arg3, e.arg4, e.arg5, e.arg6,
        );
    }

    s
}

// ── Public API ────────────────────────────────────────────────

pub fn init() {
    inner_clear();
}

pub fn clear_ring() {
    inner_clear();
}

pub fn event(tag: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64, arg6: u64) {
    if !TRACING_ON.load(Ordering::Relaxed) {
        TRACE_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let entry = TraceEntry {
        timestamp: get_time_us() as u64,
        tag,
        arg1,
        arg2,
        arg3,
        arg4,
        arg5,
        arg6,
    };
    match TRACE.try_lock() {
        Some(mut ring) => {
            // Ring overwrite: an old entry is being evicted for this new one.
            if ring.write_pos >= TRACE_SIZE {
                TRACE_DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            ring.push(entry);
        }
        None => {
            // Lock contention (dump in progress).
            TRACE_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Dump the trace buffer. `source` is a short label shown in the header
/// (e.g. "tty", "idle", "syscall") so you can tell where the dump came from.
pub fn dump_from(source: &str) {
    if DUMP_LOCK
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return; // already dumping or a prior dump is in progress
    }
    let ring = TRACE.lock();
    let count = ring.count();
    let start = ring.start();
    core::mem::drop(ring);

    crate::println!(
        "[trace] ── dump start ({} entries) [trigger: {}] ──",
        count,
        source
    );
    crate::println!("[trace] Legend: tag=NAME(hex)  a1..a5=args");
    let ring = TRACE.lock();
    for i in 0..count {
        let entry = &ring.buf[(start + i) % TRACE_SIZE];
        print_entry(entry);
    }
    crate::println!("[trace] ── dump end [trigger: {}] ──", source);
    DUMP_LOCK.store(false, Ordering::Release);

    // Shut down the machine after dumping traces.
    crate::println!("[trace] Shutting down...");
    crate::hal::shutdown();
}

/// Deprecated: use dump_from() instead.
pub fn dump() {
    dump_from("?");
}

/// Check if a character is the magic trace-dump key.
/// If it matches, dumps the trace buffer with the given source label.
/// Returns `true` if the key was consumed (caller should discard this char).
pub fn check_magic_key(ch: u8, source: &str) -> bool {
    if ch == MAGIC_KEY {
        dump_from(source);
        true
    } else {
        false
    }
}

/// Poll the console for a magic key byte (non-blocking).
/// If found, dumps the trace buffer with the given source label.
/// If NOT found, stashes the char so TTY can consume it later.
pub fn try_dump_from(source: &str) -> bool {
    let ch = crate::hal::console_getchar() as u8;
    if ch == 0xFF {
        return false;
    }
    if check_magic_key(ch, source) {
        return true;
    }
    // Not magic — stash it so TTY can pick it up later.
    stash_char(ch);
    false
}

#[macro_export]
macro_rules! trace_event {
    ($tag:expr, $a1:expr, $a2:expr, $a3:expr, $a4:expr, $a5:expr, $a6:expr) => {
        if matches!(option_env!("TRACE"), Some("1" | "on" | "true" | "trace")) {
            $crate::trace::event(
                $tag as u64,
                $a1 as u64,
                $a2 as u64,
                $a3 as u64,
                $a4 as u64,
                $a5 as u64,
                $a6 as u64,
            )
        }
    };
}
