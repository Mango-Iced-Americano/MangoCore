extern crate alloc;
use alloc::vec::Vec;
pub const TEST_GROUPS: [(&str, &str); 13] = [("basic", "basic_testcode.sh"), ("busybox", "busybox_testcode.sh"), ("lua", "lua_testcode.sh"), ("libctest", "libctest_testcode.sh"), ("iozone", "iozone_testcode.sh"), ("unixbench", "unixbench_testcode.sh"), ("iperf", "iperf_testcode.sh"), ("libcbench", "libcbench_testcode.sh"), ("lmbench", "lmbench_testcode.sh"), ("netperf", "netperf_testcode.sh"), ("cyclictest", "cyclictest_testcode.sh"), ("ltp", "ltp_testcode.sh"), ("cpython", "cpython_testcode.sh")];
pub const DEFAULT_TIMEOUTS: [u64; 13] = [60, 120, 60, 120, 480, 900, 40, 120, 900, 90, 60, 24000, 600];
const DEFAULT_ORDER: [&str; 13] = ["basic", "busybox", "lua", "lmbench", "iozone", "libcbench", "netperf", "iperf", "libctest", "cyclictest", "ltp", "cpython", "unixbench"];
pub fn default_order() -> Vec<usize> { DEFAULT_ORDER.iter().filter_map(|name| TEST_GROUPS.iter().position(|(group, _)| group == name)).collect() }
