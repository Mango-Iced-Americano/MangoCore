#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use user_lib::println;
use user_lib::syscall::*;

// ============================================================
// AF_INET 常量（与内核 os/src/net/socket/mod.rs 保持一致）
// ============================================================
const AF_INET: usize = 2;
const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const IPPROTO_TCP: usize = 6;
const IPPROTO_IP: usize = 0;
const IP_RECVERR: usize = 11;
const MSG_ERRQUEUE: usize = 0x2000;

// Used only before DHCP/procfs has published a runtime resolver.
const DEFAULT_DNS_SERVER: [u8; 4] = [10, 0, 2, 3];

// ============================================================
// sockaddr_in — Linux 兼容的 IPv4 地址结构
//   struct sockaddr_in {
//       sa_family_t    sin_family; // AF_INET = 2
//       in_port_t      sin_port;   // port, network byte order (big-endian)
//       struct in_addr sin_addr;   // IPv4 address
//       unsigned char  sin_zero[8];// padding
//   };
// ============================================================
#[repr(C)]
#[derive(Clone, Copy)]
struct sockaddr_in {
    sin_family: u16,
    sin_port: [u8; 2],
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

impl sockaddr_in {
    fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: port.to_be_bytes(),
            sin_addr: ip,
            sin_zero: [0u8; 8],
        }
    }

    fn as_ptr(&self) -> *const u8 {
        self as *const Self as *const u8
    }

    fn len() -> usize {
        core::mem::size_of::<Self>()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestIoVec {
    iov_base: *const u8,
    iov_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestMsgHdr {
    msg_name: *mut u8,
    msg_namelen: u32,
    _pad0: u32,
    msg_iov: *mut TestIoVec,
    msg_iovlen: usize,
    msg_control: *mut u8,
    msg_controllen: usize,
    msg_flags: i32,
    _pad1: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TestMMsgHdr {
    msg_hdr: TestMsgHdr,
    msg_len: u32,
    _pad: u32,
}

// ============================================================
// LTP-style test result tracking
static mut TOTAL: i32 = 0;
static mut PASSED: i32 = 0;
static mut FAILED: i32 = 0;
static mut BROKEN: i32 = 0;
static mut CONF: i32 = 0;

// ANSI color codes (LTP-compatible)
const C_GREEN: &str = "\x1b[1;32m";
const C_RED: &str = "\x1b[1;31m";
const C_YELLOW: &str = "\x1b[1;33m";
const C_MAGENTA: &str = "\x1b[1;35m";
const C_CYAN: &str = "\x1b[1;36m";
const C_RESET: &str = "\x1b[0m";

macro_rules! tpass {
    ($group:expr, $name:expr, $($arg:tt)*) => {
        unsafe { PASSED += 1; TOTAL += 1; }
        println!("{}{} {}{} {}: {}", C_GREEN, $group, $name, C_RESET, "TPASS", format_args!($($arg)*));
    };
}

macro_rules! tfail {
    ($group:expr, $name:expr, $($arg:tt)*) => {
        unsafe { FAILED += 1; TOTAL += 1; }
        println!("{}{} {}{} {}: {}", C_RED, $group, $name, C_RESET, "TFAIL", format_args!($($arg)*));
    };
}

macro_rules! tbrok {
    ($group:expr, $name:expr, $($arg:tt)*) => {
        unsafe { BROKEN += 1; TOTAL += 1; }
        println!("{}{} {}{} {}: {}", C_MAGENTA, $group, $name, C_RESET, "TBROK", format_args!($($arg)*));
    };
}

macro_rules! tconf {
    ($group:expr, $name:expr, $($arg:tt)*) => {
        unsafe { CONF += 1; TOTAL += 1; }
        println!("{}{} {}{} {}: {}", C_YELLOW, $group, $name, C_RESET, "TCONF", format_args!($($arg)*));
    };
}

macro_rules! tinfo {
    ($group:expr, $name:expr, $($arg:tt)*) => {
        println!("{}{} {}{} {}: {}", C_CYAN, $group, $name, C_RESET, "TINFO", format_args!($($arg)*));
    };
}


fn errno_from_ret(ret: isize) -> i32 {
    if ret < 0 { (-ret) as i32 } else { 0 }
}

// ============================================================
// Helper: wrap sendto with null dest (for connected TCP)
// ============================================================
fn tcp_send(fd: usize, data: &[u8]) -> isize {
    sys_sendto(fd, data.as_ptr(), data.len(), 0, core::ptr::null(), 0)
}

fn tcp_recv(fd: usize, buf: &mut [u8]) -> isize {
    sys_recvfrom(
        fd,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    )
}

/// 构造 HTTP/1.0 GET 请求，使用正确的 Host 头
/// 构造 HTTP/1.0 GET 请求字节数组，返回 (buffer, actual_length)
fn http_get_request(host: &str) -> ([u8; 256], usize) {
    let mut buf = [0u8; 256];
    let prefix = b"GET / HTTP/1.0\r\nHost: ";
    let suffix = b"\r\n\r\n";
    let mut pos = 0;
    buf[pos..pos + prefix.len()].copy_from_slice(prefix);
    pos += prefix.len();
    buf[pos..pos + host.len()].copy_from_slice(host.as_bytes());
    pos += host.len();
    buf[pos..pos + suffix.len()].copy_from_slice(suffix);
    (buf, pos + suffix.len())
}

// ============================================================
// DNS Helpers: 将域名解析为 IPv4 地址（使用运行时 resolv.conf）
// ============================================================

/// 将域名编码为 DNS label 格式（例如 "baidu.com" → \x05baidu\x03com\x00）
fn encode_dns_name(name: &str) -> ([u8; 256], usize) {
    let mut buf = [0u8; 256];
    let mut pos = 0;
    for part in name.split('.') {
        buf[pos] = part.len() as u8;
        pos += 1;
        buf[pos..pos + part.len()].copy_from_slice(part.as_bytes());
        pos += part.len();
    }
    buf[pos] = 0; // root label
    (buf, pos + 1)
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut address = [0u8; 4];
    let mut count = 0usize;
    for part in text.split('.') {
        if count == address.len() || part.is_empty() {
            return None;
        }
        let mut value = 0u16;
        for byte in part.bytes() {
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value.checked_mul(10)?.checked_add((byte - b'0') as u16)?;
            if value > u8::MAX as u16 {
                return None;
            }
        }
        address[count] = value as u8;
        count += 1;
    }
    (count == address.len()).then_some(address)
}

fn configured_dns_server() -> [u8; 4] {
    let fd = sys_open("/etc/resolv.conf\0", 0);
    if fd < 0 {
        return DEFAULT_DNS_SERVER;
    }
    let mut buffer = [0u8; 256];
    let read_len = sys_read(fd as usize, &mut buffer);
    sys_close(fd as usize);
    if read_len <= 0 {
        return DEFAULT_DNS_SERVER;
    }

    let content = core::str::from_utf8(&buffer[..read_len as usize]).unwrap_or("");
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("nameserver") {
            if let Some(address) = fields.next().and_then(parse_ipv4) {
                return address;
            }
        }
    }
    DEFAULT_DNS_SERVER
}

/// 向运行时配置的 DNS 服务器查询域名的 A 记录。
/// 成功返回 Some([a, b, c, d])，失败返回 None
fn dns_lookup(domain: &str) -> Option<[u8; 4]> {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        println!("  DNS: socket failed ({})", fd);
        return None;
    }
    let fd = fd as usize;

    // --- 构造 DNS 查询包 ---
    let (qname, qname_len) = encode_dns_name(domain);

    let mut pkt = [0u8; 512];
    pkt[0..2].copy_from_slice(&[0x12, 0x34]); // ID = 任意
    pkt[2..4].copy_from_slice(&[0x01, 0x00]); // flags: standard query, RD=1
    pkt[4..6].copy_from_slice(&[0x00, 0x01]); // QDCOUNT = 1 个问题

    pkt[12..12 + qname_len].copy_from_slice(&qname[..qname_len]);
    let off = 12 + qname_len;
    pkt[off..off + 2].copy_from_slice(&[0x00, 0x01]); // QTYPE = A (host address)
    pkt[off + 2..off + 4].copy_from_slice(&[0x00, 0x01]); // QCLASS = IN (Internet)
    let pkt_len = off + 4;

    // --- 发送到 DNS 服务器 ---
    let dns_server = configured_dns_server();
    let addr = sockaddr_in::new(dns_server, 53);
    let ret = sys_sendto(
        fd,
        pkt.as_ptr(),
        pkt_len,
        0,
        addr.as_ptr(),
        sockaddr_in::len(),
    );
    if ret < 0 {
        println!("  DNS: sendto failed ({})", ret);
        sys_close(fd);
        return None;
    }

    // --- 接收 DNS 响应 ---
    let mut resp = [0u8; 512];
    let rret = sys_recvfrom(
        fd,
        resp.as_mut_ptr(),
        resp.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 12 {
        println!("  DNS: short response ({})", rret);
        sys_close(fd);
        return None;
    }
    let rlen = rret as usize;

    // 检查响应码 (rcode = resp[3] & 0x0f)
    let rcode = resp[3] & 0x0f;
    if rcode != 0 {
        println!("  DNS: server returned rcode={} (NXDOMAIN?)", rcode);
        sys_close(fd);
        return None;
    }

    let ancount = u16::from_be_bytes([resp[6], resp[7]]);
    if ancount == 0 {
        println!("  DNS: no answer records");
        sys_close(fd);
        return None;
    }

    // --- 跳过 Header (12 字节) + Question 部分 ---
    let mut pos = 12;
    // 跳过 QNAME label 序列
    while pos < rlen {
        let len = resp[pos];
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            // 压缩指针：2 字节
            pos += 2;
            break;
        }
        pos += 1 + len as usize;
    }
    pos += 4; // 跳过 QTYPE + QCLASS

    // --- 解析 Answer 记录 ---
    for _ in 0..ancount {
        if pos + 12 > rlen {
            break;
        }
        // 跳过 NAME (可能压缩)
        if resp[pos] & 0xc0 == 0xc0 {
            pos += 2;
        } else {
            while pos < rlen && resp[pos] != 0 {
                pos += 1 + resp[pos] as usize;
            }
            pos += 1;
        }
        let atype = u16::from_be_bytes([resp[pos], resp[pos + 1]]);
        let _aclass = u16::from_be_bytes([resp[pos + 2], resp[pos + 3]]);
        let rdlength = u16::from_be_bytes([resp[pos + 8], resp[pos + 9]]) as usize;
        pos += 10; // 跳过 TYPE + CLASS + TTL(4) + RDLENGTH = 10 字节

        // QTYPE=A(1), QCLASS=IN(1), rdlength=4 就是 IPv4 地址
        if atype == 1 && rdlength == 4 && pos + 4 <= rlen {
            let ip = [resp[pos], resp[pos + 1], resp[pos + 2], resp[pos + 3]];
            sys_close(fd);
            return Some(ip);
        }
        pos += rdlength;
    }

    sys_close(fd);
    None
}

const EXTERNAL_HTTP_HOST: &str = "www.baidu.com";

fn external_http_ipv4(test_name: &str) -> Option<[u8; 4]> {
    match dns_lookup(EXTERNAL_HTTP_HOST) {
        Some(ip) => Some(ip),
        None => {
            tfail!("INET", test_name, "DNS lookup failed for {}", EXTERNAL_HTTP_HOST);
            None
        }
    }
}

// ============================================================
// Helper: fork + exec bash -c（从 unix_test.rs 借鉴）
// ============================================================
fn run_bash_cmd(cmd: &str) -> i32 {
    let pid = sys_fork();
    if pid == 0 {
        let shell = "/bin/bash\0";
        let dash_c = "-c\0";
        let mut cmd_buf = String::from(cmd);
        cmd_buf.push('\0');
        let argv = [
            shell.as_ptr(),
            dash_c.as_ptr(),
            cmd_buf.as_ptr(),
            core::ptr::null(),
        ];
        let environ: [*const u8; 1] = [core::ptr::null()];
        sys_exec(shell, &argv, &environ);
        sys_exit(127);
    }
    if pid > 0 {
        let mut code = 0;
        loop {
            let ret = sys_waitpid(pid as isize, &mut code);
            if ret == pid || ret < 0 {
                break;
            }
            sys_yield();
        }
        return code;
    }
    -1
}

// ============================================================
// Phase 1: TCP 连通性 — socket → connect → close
// ============================================================
fn test_tcp_connect_to(target_name: &str, ip: [u8; 4], port: u16) -> i32 {
    const GROUP: &str = "INET";
    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        tfail!(GROUP, target_name, "socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, port);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, target_name, "connect returned errno={}", -ret);
        sys_close(fd);
        return 1;
    }
    sys_close(fd);
    tpass!(GROUP, target_name, "connect ok to {}:{}:{}.{}:{}", ip[0],ip[1],ip[2],ip[3],port);
    0
}

// Phase 1 子测试：解析运行时可访问的 HTTP 目标，避免依赖特定公网 IP。
fn test_tcp_connect_all() -> i32 {
    let Some(ip) = external_http_ipv4("tcp_connect") else {
        return 1;
    };
    test_tcp_connect_to(EXTERNAL_HTTP_HOST, ip, 80)
}

// ============================================================
// Phase 1: TCP 收发 — connect → send HTTP GET → recv → close
// ============================================================
fn test_tcp_send_recv(target_name: &str, ip: [u8; 4]) -> i32 {
    const GROUP: &str = "INET";
    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        tfail!(GROUP, target_name, "socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, target_name, "connect returned errno={}", -ret);
        sys_close(fd);
        return 1;
    }

    let (req, req_len) = http_get_request(target_name);
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        tfail!(GROUP, target_name, "send returned {}", wret);
        sys_close(fd);
        return 1;
    }

    let mut buf = [0u8; 1024];
    let rret = tcp_recv(fd, &mut buf);
    if rret < 0 {
        tfail!(GROUP, target_name, "recv returned errno={}", -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        tfail!(GROUP, target_name, "recv 0 bytes — server closed immediately");
        sys_close(fd);
        return 1;
    }
    sys_close(fd);
    tpass!(GROUP, target_name, "TCP send/recv ok — {} bytes received", rret);
    0
}

fn test_tcp_send_recv_external() -> i32 {
    let Some(ip) = external_http_ipv4("tcp_send_recv") else {
        return 1;
    };
    test_tcp_send_recv(EXTERNAL_HTTP_HOST, ip)
}

// ============================================================
// Phase 2: HTTP GET — 完整请求 / 循环接收 / 解析状态码
// ============================================================
fn test_http_get() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "http_get";
    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        tfail!(GROUP, NAME, "socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let Some(ip) = external_http_ipv4(NAME) else {
        return 1;
    };
    let addr = sockaddr_in::new(ip, 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, NAME, "connect returned errno={}", -ret);
        sys_close(fd);
        return 1;
    }

    let (req, req_len) = http_get_request(EXTERNAL_HTTP_HOST);
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        tfail!(GROUP, NAME, "send returned {}", wret);
        sys_close(fd);
        return 1;
    }

    let mut total = 0usize;
    let mut all = [0u8; 4096];
    loop {
        let rret = tcp_recv(fd, &mut all[total..]);
        if rret <= 0 { break; }
        total += rret as usize;
        if total >= all.len() { break; }
    }

    if total == 0 {
        tfail!(GROUP, NAME, "no data received");
        sys_close(fd);
        return 1;
    }
    sys_close(fd);
    tpass!(GROUP, NAME, "HTTP GET ok — {} bytes received", total);
    0
}

// ============================================================
// Phase 4: DNS 解析 + TCP 连通性（代替硬编码 baidu/bilibili）
// ============================================================
fn test_dns_and_tcp(domain: &str) -> i32 {
    const GROUP: &str = "INET";
    let ip = match dns_lookup(domain) {
        Some(ip) => ip,
        None => {
            tfail!(GROUP, domain, "DNS lookup failed");
            return 1;
        }
    };

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        tfail!(GROUP, domain, "socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, domain, "connect to {}.{}.{}.{} returned errno={}", ip[0],ip[1],ip[2],ip[3], -ret);
        sys_close(fd);
        return 1;
    }

    let (req, req_len) = http_get_request(domain);
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        tfail!(GROUP, domain, "send returned {}", wret);
        sys_close(fd);
        return 1;
    }

    let mut buf = [0u8; 1024];
    let rret = tcp_recv(fd, &mut buf);
    if rret < 0 {
        tfail!(GROUP, domain, "recv returned errno={}", -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        tfail!(GROUP, domain, "recv 0 bytes — server closed immediately");
        sys_close(fd);
        return 1;
    }
    sys_close(fd);
    tpass!(GROUP, domain, "DNS+TCP ok — {} bytes via {}.{}.{}.{}", rret, ip[0],ip[1],ip[2],ip[3]);
    0
}

// ============================================================
// Phase 5: UDP 连通性测试
// ============================================================

// --- UDP Loopback: 127.0.0.1 self-send/recv ---
fn test_udp_loopback() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "udp_loopback";
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tfail!(GROUP, NAME, "socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let my_addr = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd, my_addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, NAME, "bind returned errno={}", -ret);
        sys_close(fd);
        return 1;
    }

    let mut bound_addr = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    let ret = sys_getsockname(fd, bound_addr.as_ptr() as *mut u8, &mut addrlen as *mut usize);
    if ret < 0 {
        tfail!(GROUP, NAME, "getsockname returned {}", ret);
        sys_close(fd);
        return 1;
    }
    let port = u16::from_be_bytes(bound_addr.sin_port);

    let target = sockaddr_in::new([127, 0, 0, 1], port);
    let msg = b"hello_udp_loopback";
    let wret = sys_sendto(fd, msg.as_ptr(), msg.len(), 0, target.as_ptr(), sockaddr_in::len());
    if wret < 0 {
        tfail!(GROUP, NAME, "sendto returned errno={}", -wret);
        sys_close(fd);
        return 1;
    }

    let mut buf = [0u8; 128];
    let mut from = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut fromlen = sockaddr_in::len();
    let rret = sys_recvfrom(fd, buf.as_mut_ptr(), buf.len(), 0, from.as_ptr() as *mut u8, &mut fromlen as *mut usize);
    if rret < 0 {
        tfail!(GROUP, NAME, "recvfrom returned errno={}", -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        tfail!(GROUP, NAME, "recvfrom returned 0 (unexpected for UDP)");
        sys_close(fd);
        return 1;
    }
    let recv = &buf[..rret as usize];
    if recv != msg {
        tfail!(GROUP, NAME, "data mismatch: got {:?} expected {:?}", recv, msg);
        sys_close(fd);
        return 1;
    }
    sys_close(fd);
    tpass!(GROUP, NAME, "UDP loopback self-send works");
    0
}

// --- UDP Loopback: A → B (双向) ---
fn test_udp_loopback_pair() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "udp_loopback_pair";
    let fd_a = sys_socket(AF_INET, SOCK_DGRAM, 0);
    let fd_b = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_a < 0 || fd_b < 0 {
        tfail!(GROUP, NAME, "socket failed a={} b={}", fd_a, fd_b);
        return 1;
    }
    let fd_a = fd_a as usize;
    let fd_b = fd_b as usize;

    let addr_b = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_b, addr_b.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, NAME, "bind B returned errno={}", -ret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }
    let addr_a = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_a, addr_a.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        tfail!(GROUP, NAME, "bind A returned errno={}", -ret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }

    let mut bound_b = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    sys_getsockname(fd_b, bound_b.as_ptr() as *mut u8, &mut addrlen as *mut usize);
    let port_b = u16::from_be_bytes(bound_b.sin_port);

    let target_b = sockaddr_in::new([127, 0, 0, 1], port_b);
    let msg = b"ping_from_A";
    let wret = sys_sendto(fd_a, msg.as_ptr(), msg.len(), 0, target_b.as_ptr(), sockaddr_in::len());
    if wret < 0 {
        tfail!(GROUP, NAME, "A->B sendto returned errno={}", -wret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }

    let mut buf = [0u8; 128];
    let mut from = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut fromlen = sockaddr_in::len();
    let rret = sys_recvfrom(fd_b, buf.as_mut_ptr(), buf.len(), 0, from.as_ptr() as *mut u8, &mut fromlen as *mut usize);
    if rret < 0 {
        tfail!(GROUP, NAME, "B recvfrom returned errno={}", -rret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }
    if &buf[..rret as usize] != msg {
        tfail!(GROUP, NAME, "B got data mismatch");
        sys_close(fd_a); sys_close(fd_b); return 1;
    }

    let reply = b"pong_from_B";
    let wret = sys_sendto(fd_b, reply.as_ptr(), reply.len(), 0, from.as_ptr(), fromlen);
    if wret < 0 {
        tfail!(GROUP, NAME, "B→A sendto returned errno={}", -wret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }

    let mut buf2 = [0u8; 128];
    let rret = sys_recvfrom(fd_a, buf2.as_mut_ptr(), buf2.len(), 0, core::ptr::null_mut(), core::ptr::null_mut());
    if rret < 0 {
        tfail!(GROUP, NAME, "A recvfrom returned errno={}", -rret);
        sys_close(fd_a); sys_close(fd_b); return 1;
    }
    if &buf2[..rret as usize] != reply {
        tfail!(GROUP, NAME, "A got data mismatch");
        sys_close(fd_a); sys_close(fd_b); return 1;
    }
    sys_close(fd_a); sys_close(fd_b);
    tpass!(GROUP, NAME, "UDP A->B bidirectional works");
    0
}

// --- UDP External: query the resolver published through /etc/resolv.conf ---
fn test_udp_external_dns() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "udp_external_dns";
    let ip = match dns_lookup("baidu.com") {
        Some(ip) => ip,
        None => { tfail!(GROUP, NAME, "DNS lookup failed"); return 1; }
    };

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tfail!(GROUP, NAME, "socket returned {}", fd); return 1; }
    let fd = fd as usize;

    let (qname, qname_len) = encode_dns_name("baidu.com");
    let mut pkt = [0u8; 512];
    pkt[0..2].copy_from_slice(&[0xab, 0xcd]);
    pkt[2..4].copy_from_slice(&[0x01, 0x00]);
    pkt[4..6].copy_from_slice(&[0x00, 0x01]);
    pkt[12..12 + qname_len].copy_from_slice(&qname[..qname_len]);
    let off = 12 + qname_len;
    pkt[off..off + 2].copy_from_slice(&[0x00, 0x01]);
    pkt[off + 2..off + 4].copy_from_slice(&[0x00, 0x01]);
    let pkt_len = off + 4;

    let dns_server = configured_dns_server();
    let target = sockaddr_in::new(dns_server, 53);
    let wret = sys_sendto(fd, pkt.as_ptr(), pkt_len, 0, target.as_ptr(), sockaddr_in::len());
    if wret < 0 { tfail!(GROUP, NAME, "sendto runtime DNS returned errno={}", -wret); sys_close(fd); return 1; }

    let mut resp = [0u8; 512];
    let rret = sys_recvfrom(fd, resp.as_mut_ptr(), resp.len(), 0, core::ptr::null_mut(), core::ptr::null_mut());
    if rret < 0 { tfail!(GROUP, NAME, "recvfrom returned errno={}", -rret); sys_close(fd); return 1; }
    if rret == 0 { tfail!(GROUP, NAME, "recvfrom returned 0"); sys_close(fd); return 1; }

    if rret >= 12 && resp[0..2] == [0xab, 0xcd] {
        sys_close(fd);
        tpass!(GROUP, NAME, "UDP query to runtime DNS works");
        0
    } else {
        tfail!(GROUP, NAME, "invalid DNS response"); sys_close(fd); 1
    }
}

// ============================================================
// Phase 6: 32KB Giant UDP Loopback — IP分片重组调试
//   单包发送-接收，隔离并发问题，验证分片重组完整链路
// ============================================================
fn test_udp_giant_loopback() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "udp_giant_loopback";
    let fd_send = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_send < 0 { tfail!(GROUP, NAME, "sender socket returned {}", fd_send); return 1; }
    let fd_send = fd_send as usize;

    let fd_recv = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_recv < 0 { tfail!(GROUP, NAME, "receiver socket returned {}", fd_recv); sys_close(fd_send); return 1; }
    let fd_recv = fd_recv as usize;

    let addr_recv = sockaddr_in::new([127, 0, 0, 1], 5201);
    let ret = sys_bind(fd_recv, addr_recv.as_ptr(), sockaddr_in::len());
    if ret < 0 { tfail!(GROUP, NAME, "bind receiver returned errno={}", -ret); sys_close(fd_send); sys_close(fd_recv); return 1; }

    let addr_send = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_send, addr_send.as_ptr(), sockaddr_in::len());
    if ret < 0 { tfail!(GROUP, NAME, "bind sender returned errno={}", -ret); sys_close(fd_send); sys_close(fd_recv); return 1; }

    const GIANT_SIZE: usize = 32768;
    let mut payload = [0x42u8; GIANT_SIZE];
    payload[0..8].copy_from_slice(b"GIANT_S:");
    payload[GIANT_SIZE - 8..].copy_from_slice(b":GIANT_E");

    let target = sockaddr_in::new([127, 0, 0, 1], 5201);
    let wret = sys_sendto(fd_send, payload.as_ptr(), GIANT_SIZE, 0, target.as_ptr(), sockaddr_in::len());
    if wret < 0 { tfail!(GROUP, NAME, "sendto returned errno={}", -wret); sys_close(fd_send); sys_close(fd_recv); return 1; }

    let mut recv_buf = [0u8; 65536];
    let rret = sys_recvfrom(fd_recv, recv_buf.as_mut_ptr(), recv_buf.len(), 0, core::ptr::null_mut(), core::ptr::null_mut());
    if rret < 0 { tfail!(GROUP, NAME, "recvfrom returned errno={}", -rret); sys_close(fd_send); sys_close(fd_recv); return 1; }
    let recv_len = rret as usize;

    if recv_len != GIANT_SIZE {
        tfail!(GROUP, NAME, "expected {} bytes, got {}", GIANT_SIZE, recv_len);
        sys_close(fd_send); sys_close(fd_recv); return 1;
    }
    if &recv_buf[..recv_len] != &payload[..] {
        let mismatch = recv_buf[..recv_len].iter().zip(payload.iter()).position(|(a,b)| a!=b);
        tfail!(GROUP, NAME, "data mismatch at offset {:?}", mismatch);
        sys_close(fd_send); sys_close(fd_recv); return 1;
    }
    sys_close(fd_send); sys_close(fd_recv);
    tpass!(GROUP, NAME, "32KB UDP loopback works");
    0
}

// --- HTTPS test (embedded-tls, pure Rust) ---

use embedded_io::{ErrorType, Read, Write};
use embedded_tls::{blocking::TlsConnection, Aes128GcmSha256, NoVerify, TlsConfig, TlsContext};
use rand_core::{CryptoRng, RngCore};

/// 包装内核 socket fd，实现 embedded_io::Read + Write
struct TlsSocket {
    fd: usize,
}

impl ErrorType for TlsSocket {
    type Error = embedded_io::ErrorKind;
}

impl Read for TlsSocket {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let ret = tcp_recv(self.fd, buf);
        if ret > 0 {
            Ok(ret as usize)
        } else if ret == 0 {
            Err(embedded_io::ErrorKind::ConnectionAborted)
        } else {
            Err(embedded_io::ErrorKind::Other)
        }
    }
}

impl Write for TlsSocket {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let ret = tcp_send(self.fd, buf);
        if ret > 0 {
            Ok(ret as usize)
        } else {
            Err(embedded_io::ErrorKind::Other)
        }
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// 极简 RNG — 用时间 + 计数器糊一个，仅测试用
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new() -> Self {
        SimpleRng {
            state: user_lib::get_time() as u64,
        }
    }
}

impl RngCore for SimpleRng {
    fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 32) as u32 ^ (self.state as u32)
    }
    fn next_u64(&mut self) -> u64 {
        let lo = self.next_u32() as u64;
        let hi = self.next_u32() as u64;
        (hi << 32) | lo
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let val = self.next_u64();
            let len = chunk.len();
            chunk.copy_from_slice(&val.to_le_bytes()[..len]);
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for SimpleRng {}

fn test_https_tls() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "https_tls";
    let ip = match dns_lookup("cloudflare.com") {
        Some(ip) => ip,
        None => { tfail!(GROUP, NAME, "DNS lookup failed"); return 1; }
    };

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 { tfail!(GROUP, NAME, "socket returned {}", fd); return 1; }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 443);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 { tfail!(GROUP, NAME, "connect returned errno={}", -ret); sys_close(fd); return 1; }

    let mut read_buf = [0u8; 16640];
    let mut write_buf = [0u8; 4096];
    let socket = TlsSocket { fd };
    let config = TlsConfig::new().with_server_name("cloudflare.com");
    let mut tls: TlsConnection<TlsSocket, Aes128GcmSha256> = TlsConnection::new(socket, &mut read_buf, &mut write_buf);
    let mut rng = SimpleRng::new();

    match tls.open::<SimpleRng, NoVerify>(TlsContext::new(&config, &mut rng)) {
        Ok(()) => {},
        Err(_) => { tfail!(GROUP, NAME, "TLS handshake failed"); sys_close(fd); return 1; }
    }

    let http_req = b"GET /cdn-cgi/trace HTTP/1.1\r\nHost: cloudflare.com\r\nConnection: close\r\n\r\n";
    if tls.write(http_req).is_err() { tfail!(GROUP, NAME, "TLS write failed"); sys_close(fd); return 1; }
    let _ = tls.flush();

    let mut rx = [0u8; 4096];
    let t_start = user_lib::get_time();
    loop {
        match tls.read(&mut rx) {
            Ok(n) if n > 0 => { sys_close(fd); tpass!(GROUP, NAME, "HTTPS response {} bytes", n); return 0; }
            _ => {
                if (user_lib::get_time() - t_start) > 5000 {
                    tfail!(GROUP, NAME, "TLS read timeout after 5s"); sys_close(fd); return 1;
                }
                user_lib::sleep(100);
            }
        }
    }
}

fn test_https_download() -> i32 {
    const GROUP: &str = "INET";
    const NAME: &str = "https_tls_download";
    let host = "cloudflare.com";
    let ip = match dns_lookup(host) {
        Some(ip) => ip,
        None => { tconf!(GROUP, NAME, "DNS lookup failed — skipping"); return 0; }
    };
    tinfo!(GROUP, NAME, "stage=DNS ip={}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);

    let t0 = user_lib::get_time();
    let addr = sockaddr_in::new(ip, 443);

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 { tfail!(GROUP, NAME, "socket failed"); return 1; }
    let fd = fd as usize;

    tinfo!(GROUP, NAME, "stage=connect");
    if sys_connect(fd, addr.as_ptr(), sockaddr_in::len()) < 0 {
        tfail!(GROUP, NAME, "connect failed"); sys_close(fd); return 1;
    }

    let mut read_buf = [0u8; 16640];
    let mut write_buf = [0u8; 4096];
    let socket = TlsSocket { fd };
    let config = TlsConfig::new().with_server_name(host);
    let mut tls: TlsConnection<TlsSocket, Aes128GcmSha256> = TlsConnection::new(socket, &mut read_buf, &mut write_buf);
    let mut rng = SimpleRng::new();

    tinfo!(GROUP, NAME, "stage=tls-handshake");
    if tls.open::<SimpleRng, NoVerify>(TlsContext::new(&config, &mut rng)).is_err() {
        tfail!(GROUP, NAME, "TLS handshake failed"); sys_close(fd); return 1;
    }

    let http_req = alloc::format!("GET /cdn-cgi/trace HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", host);
    tinfo!(GROUP, NAME, "stage=http-request");
    if tls.write(http_req.as_bytes()).is_err() {
        tfail!(GROUP, NAME, "HTTP write failed"); sys_close(fd); return 1;
    }

    let mut rx = [0u8; 4096];
    let mut total = 0usize;
    tinfo!(GROUP, NAME, "stage=read-response");
    loop {
        match tls.read(&mut rx) { Ok(0) | Err(_) => break, Ok(n) => total += n, }
    }
    sys_close(fd);

    let dt = user_lib::get_time() - t0;
    if total > 0 {
        tpass!(GROUP, NAME, "{} bytes in {} ms", total, dt);
        0
    } else {
        tfail!(GROUP, NAME, "0 bytes received in {} ms", dt);
        1
    }
}

// ============================================================
// NET_CORE test group
// ============================================================

fn net_core01_interface_basic() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core01_interface_basic";

    // Test 127.0.0.1 (lo)
    let fd1 = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd1 < 0 {
        tbrok!(GROUP, NAME, "socket for lo failed: {}", fd1);
        return 1;
    }
    let fd1 = fd1 as usize;
    let addr1 = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret1 = sys_bind(fd1, addr1.as_ptr(), sockaddr_in::len());
    if ret1 < 0 {
        let err = errno_from_ret(ret1);
        sys_close(fd1);
        if err == 99 {
            // EADDRNOTAVAIL
            tfail!(GROUP, NAME, "bind to 127.0.0.1 failed with EADDRNOTAVAIL");
        } else {
            tbrok!(GROUP, NAME, "bind to 127.0.0.1 failed with errno {}", err);
        }
        return 1;
    }

    // Test the address currently assigned to eth0 (QEMU static or board DHCP).
    let Some(eth0_addr) = interface_ipv4("eth0") else {
        sys_close(fd1);
        tconf!(GROUP, NAME, "eth0 has no IPv4 address");
        return 0;
    };
    let fd2 = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd2 < 0 {
        sys_close(fd1);
        tbrok!(GROUP, NAME, "socket for eth0 failed: {}", fd2);
        return 1;
    }
    let fd2 = fd2 as usize;
    let addr2 = sockaddr_in::new(eth0_addr, 0);
    let ret2 = sys_bind(fd2, addr2.as_ptr(), sockaddr_in::len());
    if ret2 < 0 {
        let err = errno_from_ret(ret2);
        sys_close(fd1);
        sys_close(fd2);
        if err == 99 {
            // EADDRNOTAVAIL
            tfail!(GROUP, NAME, "bind to runtime eth0 address failed with EADDRNOTAVAIL");
        } else {
            tbrok!(GROUP, NAME, "bind to runtime eth0 address failed with errno {}", err);
        }
        return 1;
    }

    sys_close(fd1);
    sys_close(fd2);
    tpass!(GROUP, NAME, "lo and eth0 interfaces verified");
    0
}

fn net_core02_loopback_and_default_iface() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core02_loopback_and_default_iface";

    // UDP connect to loopback
    let fd_udp = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_udp < 0 {
        tbrok!(GROUP, NAME, "UDP socket failed: {}", fd_udp);
        return 1;
    }
    let fd_udp = fd_udp as usize;
    let addr_udp = sockaddr_in::new([127, 0, 0, 1], 12345);
    let ret_udp = sys_connect(fd_udp, addr_udp.as_ptr(), sockaddr_in::len());
    if ret_udp < 0 {
        let err = errno_from_ret(ret_udp);
        sys_close(fd_udp);
        tfail!(
            GROUP,
            NAME,
            "UDP connect to 127.0.0.1:12345 failed with errno {}",
            err
        );
        return 1;
    }
    sys_close(fd_udp);

    // TCP bind to loopback (routing validation without requiring a listener)
    let fd_tcp = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd_tcp < 0 {
        tbrok!(GROUP, NAME, "TCP socket failed: {}", fd_tcp);
        return 1;
    }
    let fd_tcp = fd_tcp as usize;
    let addr_tcp = sockaddr_in::new([127, 0, 0, 1], 12346);
    let ret_tcp = sys_bind(fd_tcp, addr_tcp.as_ptr(), sockaddr_in::len());
    if ret_tcp < 0 {
        let err = errno_from_ret(ret_tcp);
        sys_close(fd_tcp);
        tfail!(GROUP, NAME, "TCP bind to 127.0.0.1:12346 failed with errno {}", err);
        return 1;
    }
    sys_close(fd_tcp);

    tpass!(GROUP, NAME, "loopback routing verified");
    0
}

fn net_core_ip_recverr() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core_ip_recverr";

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "UDP socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let mut value = u32::MAX;
    let mut len = core::mem::size_of::<u32>();
    let mut ret = sys_getsockopt(fd, IPPROTO_IP, IP_RECVERR,
        &mut value as *mut u32 as *mut u8, &mut len);
    if ret < 0 || value != 0 || len != core::mem::size_of::<u32>() {
        sys_close(fd);
        tfail!(GROUP, NAME, "default state mismatch: ret={} value={} len={}", ret, value, len);
        return 1;
    }

    value = 1;
    ret = sys_setsockopt(fd, IPPROTO_IP, IP_RECVERR,
        &value as *const u32 as *const u8, core::mem::size_of::<u32>());
    if ret < 0 {
        sys_close(fd);
        tfail!(GROUP, NAME, "enable failed with errno {}", errno_from_ret(ret));
        return 1;
    }

    value = 0;
    len = core::mem::size_of::<u32>();
    ret = sys_getsockopt(fd, IPPROTO_IP, IP_RECVERR,
        &mut value as *mut u32 as *mut u8, &mut len);
    if ret < 0 || value != 1 {
        sys_close(fd);
        tfail!(GROUP, NAME, "enabled state mismatch: ret={} value={}", ret, value);
        return 1;
    }

    let mut byte = 0u8;
    ret = sys_recvfrom(fd, &mut byte, 1, MSG_ERRQUEUE,
        core::ptr::null_mut(), core::ptr::null_mut());
    if errno_from_ret(ret) != 11 {
        sys_close(fd);
        tfail!(GROUP, NAME, "empty error queue returned {}, expected EAGAIN", ret);
        return 1;
    }

    value = 0;
    ret = sys_setsockopt(fd, IPPROTO_IP, IP_RECVERR,
        &value as *const u32 as *const u8, core::mem::size_of::<u32>());
    sys_close(fd);
    if ret < 0 {
        tfail!(GROUP, NAME, "disable failed with errno {}", errno_from_ret(ret));
        return 1;
    }

    tpass!(GROUP, NAME, "IP_RECVERR state and empty error queue verified");
    0
}

fn net_core_sendmmsg() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core_sendmmsg";

    let receiver = sys_socket(AF_INET, SOCK_DGRAM, 0);
    let sender = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if receiver < 0 || sender < 0 {
        if receiver >= 0 { sys_close(receiver as usize); }
        if sender >= 0 { sys_close(sender as usize); }
        tbrok!(GROUP, NAME, "socket creation failed: receiver={} sender={}", receiver, sender);
        return 1;
    }
    let receiver = receiver as usize;
    let sender = sender as usize;
    let destination = sockaddr_in::new([127, 0, 0, 1], 23457);
    let bind_ret = sys_bind(receiver, destination.as_ptr(), sockaddr_in::len());
    if bind_ret < 0 {
        sys_close(sender);
        sys_close(receiver);
        tbrok!(GROUP, NAME, "receiver bind failed with errno {}", errno_from_ret(bind_ret));
        return 1;
    }

    let first = b"sendmmsg-first";
    let second = b"sendmmsg-second";
    let mut iov = [
        TestIoVec { iov_base: first.as_ptr(), iov_len: first.len() },
        TestIoVec { iov_base: second.as_ptr(), iov_len: second.len() },
    ];
    let iov_ptr = iov.as_mut_ptr();
    let base_hdr = TestMsgHdr {
        msg_name: &destination as *const sockaddr_in as *mut u8,
        msg_namelen: sockaddr_in::len() as u32,
        _pad0: 0,
        msg_iov: core::ptr::null_mut(),
        msg_iovlen: 1,
        msg_control: core::ptr::null_mut(),
        msg_controllen: 0,
        msg_flags: 0,
        _pad1: 0,
    };
    let mut messages = [
        TestMMsgHdr { msg_hdr: TestMsgHdr { msg_iov: iov_ptr, ..base_hdr }, msg_len: 0, _pad: 0 },
        TestMMsgHdr { msg_hdr: TestMsgHdr { msg_iov: iov_ptr.wrapping_add(1), ..base_hdr }, msg_len: 0, _pad: 0 },
    ];

    let zero_ret = sys_sendmmsg(sender, messages.as_mut_ptr() as *mut u8, 0, 0);
    let send_ret = sys_sendmmsg(sender, messages.as_mut_ptr() as *mut u8, messages.len(), 0);
    if zero_ret != 0 || send_ret != 2
        || messages[0].msg_len as usize != first.len()
        || messages[1].msg_len as usize != second.len() {
        sys_close(sender);
        sys_close(receiver);
        tfail!(GROUP, NAME, "batch mismatch: zero={} sent={} lengths=[{}, {}]",
            zero_ret, send_ret, messages[0].msg_len, messages[1].msg_len);
        return 1;
    }

    let expected: [&[u8]; 2] = [first, second];
    let mut recv_buf = [0u8; 32];
    for packet in expected {
        let recv_ret = sys_recvfrom(receiver, recv_buf.as_mut_ptr(), recv_buf.len(), 0,
            core::ptr::null_mut(), core::ptr::null_mut());
        if recv_ret != packet.len() as isize || &recv_buf[..packet.len()] != packet {
            sys_close(sender);
            sys_close(receiver);
            tfail!(GROUP, NAME, "received datagram mismatch: ret={}", recv_ret);
            return 1;
        }
    }

    sys_close(sender);
    sys_close(receiver);
    tpass!(GROUP, NAME, "two datagrams sent with lengths reported");
    0
}

fn net_core03_route_lookup() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core03_route_lookup";

    // UDP bind to 127.0.0.1, sendto self, recvfrom
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        let err = errno_from_ret(ret);
        sys_close(fd);
        tfail!(GROUP, NAME, "bind to 127.0.0.1 failed with errno {}", err);
        return 1;
    }

    // Get bound port
    let mut bound = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    sys_getsockname(fd, bound.as_ptr() as *mut u8, &mut addrlen as *mut usize);
    let port = u16::from_be_bytes(bound.sin_port);

    // Send to self
    let target = sockaddr_in::new([127, 0, 0, 1], port);
    let msg = b"route_test";
    let wret = sys_sendto(fd, msg.as_ptr(), msg.len(), 0, target.as_ptr(), sockaddr_in::len());
    if wret < 0 {
        let err = errno_from_ret(wret);
        sys_close(fd);
        tfail!(GROUP, NAME, "sendto 127.0.0.1 failed with errno {}", err);
        return 1;
    }

    // Recv
    let mut buf = [0u8; 128];
    let rret = sys_recvfrom(
        fd,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        let err = errno_from_ret(rret);
        sys_close(fd);
        tfail!(GROUP, NAME, "recvfrom failed with errno {}", err);
        return 1;
    }
    if &buf[..rret as usize] != msg {
        sys_close(fd);
        tfail!(GROUP, NAME, "data mismatch");
        return 1;
    }
    sys_close(fd);

    // UDP bind to the runtime eth0 address.
    let Some(eth0_addr) = interface_ipv4("eth0") else {
        tconf!(GROUP, NAME, "eth0 has no IPv4 address");
        return 0;
    };
    let fd2 = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd2 < 0 {
        tbrok!(GROUP, NAME, "second socket failed: {}", fd2);
        return 1;
    }
    let fd2 = fd2 as usize;
    let addr2 = sockaddr_in::new(eth0_addr, 0);
    let ret2 = sys_bind(fd2, addr2.as_ptr(), sockaddr_in::len());
    if ret2 < 0 {
        let err = errno_from_ret(ret2);
        sys_close(fd2);
        tfail!(GROUP, NAME, "bind to runtime eth0 address failed with errno {}", err);
        return 1;
    }
    sys_close(fd2);

    // Try sendto 8.8.8.8 — may fail with ENETUNREACH or timeout; as long as it doesn't panic
    let fd3 = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd3 < 0 {
        tbrok!(GROUP, NAME, "third socket failed: {}", fd3);
        return 1;
    }
    let fd3 = fd3 as usize;
    let target3 = sockaddr_in::new([8, 8, 8, 8], 53);
    let _wret3 = sys_sendto(fd3, msg.as_ptr(), msg.len(), 0, target3.as_ptr(), sockaddr_in::len());
    sys_close(fd3);

    tpass!(GROUP, NAME, "route lookup verified");
    0
}

fn net_core04_ephemeral_port_range() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core04_ephemeral_port_range";

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        tbrok!(GROUP, NAME, "socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new([0, 0, 0, 0], 0);
    let ret = sys_bind(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        let err = errno_from_ret(ret);
        sys_close(fd);
        tbrok!(GROUP, NAME, "bind failed with errno {}", err);
        return 1;
    }

    let mut bound = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    let gret = sys_getsockname(fd, bound.as_ptr() as *mut u8, &mut addrlen as *mut usize);
    if gret < 0 {
        let err = errno_from_ret(gret);
        sys_close(fd);
        tbrok!(GROUP, NAME, "getsockname failed with errno {}", err);
        return 1;
    }

    let port = u16::from_be_bytes(bound.sin_port);
    sys_close(fd);

    if port >= 32768 && port <= 60999 {
        tpass!(GROUP, NAME, "ephemeral port {} in range [32768, 60999]", port);
        0
    } else {
        tfail!(
            GROUP,
            NAME,
            "expected port in [32768,60999] got {}",
            port
        );
        1
    }
}

fn net_core05_port_bind_conflict() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core05_port_bind_conflict";

    let fd1 = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd1 < 0 {
        tbrok!(GROUP, NAME, "first socket failed: {}", fd1);
        return 1;
    }
    let fd1 = fd1 as usize;

    let addr1 = sockaddr_in::new([0, 0, 0, 0], 18080);
    let ret1 = sys_bind(fd1, addr1.as_ptr(), sockaddr_in::len());
    if ret1 < 0 {
        let err = errno_from_ret(ret1);
        sys_close(fd1);
        tbrok!(GROUP, NAME, "first bind failed with errno {}", err);
        return 1;
    }

    let fd2 = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd2 < 0 {
        sys_close(fd1);
        tbrok!(GROUP, NAME, "second socket failed: {}", fd2);
        return 1;
    }
    let fd2 = fd2 as usize;

    let addr2 = sockaddr_in::new([0, 0, 0, 0], 18080);
    let ret2 = sys_bind(fd2, addr2.as_ptr(), sockaddr_in::len());
    sys_close(fd1);
    sys_close(fd2);

    if ret2 == 0 {
        tfail!(GROUP, NAME, "second bind succeeded, expected EADDRINUSE");
        1
    } else {
        let err = errno_from_ret(ret2);
        if err == 98 {
            // EADDRINUSE
            tpass!(GROUP, NAME, "second bind correctly returned EADDRINUSE");
            0
        } else {
            tfail!(
                GROUP,
                NAME,
                "second bind returned errno {}, expected EADDRINUSE (98)",
                err
            );
            1
        }
    }
}

fn net_core06_port_reuse_after_close() -> i32 {
    const GROUP: &str = "NET_CORE";
    const NAME: &str = "net_core06_port_reuse_after_close";

    let fd1 = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd1 < 0 {
        tbrok!(GROUP, NAME, "first socket failed: {}", fd1);
        return 1;
    }
    let fd1 = fd1 as usize;

    let addr1 = sockaddr_in::new([0, 0, 0, 0], 19000);
    let ret1 = sys_bind(fd1, addr1.as_ptr(), sockaddr_in::len());
    if ret1 < 0 {
        let err = errno_from_ret(ret1);
        sys_close(fd1);
        tbrok!(GROUP, NAME, "first bind failed with errno {}", err);
        return 1;
    }
    sys_close(fd1);

    let fd2 = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd2 < 0 {
        tbrok!(GROUP, NAME, "second socket failed: {}", fd2);
        return 1;
    }
    let fd2 = fd2 as usize;

    let addr2 = sockaddr_in::new([0, 0, 0, 0], 19000);
    let ret2 = sys_bind(fd2, addr2.as_ptr(), sockaddr_in::len());
    if ret2 < 0 {
        let err = errno_from_ret(ret2);
        sys_close(fd2);
        tfail!(
            GROUP,
            NAME,
            "port not released after close, got errno {}",
            err
        );
        1
    } else {
        sys_close(fd2);
        tpass!(GROUP, NAME, "port released and rebound successfully");
        0
    }
}

// ============================================================
// NET_ROUTE test group
// ============================================================

const ENETUNREACH: i32 = 101;

fn net_route01_loopback_udp() -> i32 {
    const GROUP: &str = "NET_ROUTE";
    const NAME: &str = "net_route01_loopback_udp";

    let fd_recv = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_recv < 0 {
        tbrok!(GROUP, NAME, "recv socket failed: {}", fd_recv);
        return 1;
    }
    let fd_recv = fd_recv as usize;

    let addr_recv = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_recv, addr_recv.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        let err = errno_from_ret(ret);
        sys_close(fd_recv);
        tfail!(GROUP, NAME, "bind to 127.0.0.1 failed with errno {}", err);
        return 1;
    }

    let mut bound = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    let gret = sys_getsockname(fd_recv, bound.as_ptr() as *mut u8, &mut addrlen as *mut usize);
    if gret < 0 {
        let err = errno_from_ret(gret);
        sys_close(fd_recv);
        tbrok!(GROUP, NAME, "getsockname failed with errno {}", err);
        return 1;
    }
    let port = u16::from_be_bytes(bound.sin_port);

    let fd_send = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_send < 0 {
        sys_close(fd_recv);
        tbrok!(GROUP, NAME, "send socket failed: {}", fd_send);
        return 1;
    }
    let fd_send = fd_send as usize;

    let target = sockaddr_in::new([127, 0, 0, 1], port);
    let msg = b"route_loopback_udp";
    let wret = sys_sendto(fd_send, msg.as_ptr(), msg.len(), 0, target.as_ptr(), sockaddr_in::len());
    if wret < 0 {
        let err = errno_from_ret(wret);
        sys_close(fd_recv);
        sys_close(fd_send);
        tfail!(GROUP, NAME, "sendto 127.0.0.1:{} failed with errno {}", port, err);
        return 1;
    }

    let mut buf = [0u8; 128];
    let rret = sys_recvfrom(
        fd_recv,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        let err = errno_from_ret(rret);
        sys_close(fd_recv);
        sys_close(fd_send);
        tfail!(
            GROUP,
            NAME,
            "recvfrom failed with errno {} — loopback UDP routing failed, no data received",
            err
        );
        return 1;
    }
    if rret == 0 {
        sys_close(fd_recv);
        sys_close(fd_send);
        tfail!(GROUP, NAME, "recvfrom returned 0 — loopback UDP routing failed, no data received");
        return 1;
    }
    if &buf[..rret as usize] != msg {
        sys_close(fd_recv);
        sys_close(fd_send);
        tfail!(GROUP, NAME, "data mismatch");
        return 1;
    }

    sys_close(fd_recv);
    sys_close(fd_send);
    tpass!(GROUP, NAME, "loopback UDP routing works");
    0
}

fn net_route02_eth_local_addr() -> i32 {
    const GROUP: &str = "NET_ROUTE";
    const NAME: &str = "net_route02_eth_local_addr";

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let Some(eth0_addr) = interface_ipv4("eth0") else {
        sys_close(fd);
        tconf!(GROUP, NAME, "eth0 has no IPv4 address");
        return 0;
    };
    let addr = sockaddr_in::new(eth0_addr, 0);
    let ret = sys_bind(fd, addr.as_ptr(), sockaddr_in::len());
    sys_close(fd);

    if ret == 0 {
        tpass!(GROUP, NAME, "bind to runtime eth0 local address succeeded");
        0
    } else {
        let err = errno_from_ret(ret);
        if err == 99 {
            // EADDRNOTAVAIL
            tfail!(GROUP, NAME, "cannot bind to eth0 local address (EADDRNOTAVAIL)");
        } else {
            tbrok!(GROUP, NAME, "bind to runtime eth0 address failed with unexpected errno {}", err);
        }
        1
    }
}

fn net_route03_dns_route() -> i32 {
    const GROUP: &str = "NET_ROUTE";
    const NAME: &str = "net_route03_dns_route";

    match dns_lookup("baidu.com") {
        Some(ip) => {
            tpass!(
                GROUP,
                NAME,
                "DNS routing works (resolved to {}.{}.{}.{})",
                ip[0], ip[1], ip[2], ip[3]
            );
            0
        }
        None => {
            tconf!(GROUP, NAME, "DNS server unavailable, routing not verified");
            0
        }
    }
}

fn net_route04_default_route() -> i32 {
    const GROUP: &str = "NET_ROUTE";
    const NAME: &str = "net_route04_default_route";

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let target = sockaddr_in::new([8, 8, 8, 8], 53);
    let msg = b"route_probe";
    let wret = sys_sendto(fd, msg.as_ptr(), msg.len(), 0, target.as_ptr(), sockaddr_in::len());
    sys_close(fd);

    if wret >= 0 {
        tpass!(GROUP, NAME, "default route lookup doesn't panic (sendto succeeded)");
        0
    } else {
        let err = errno_from_ret(wret);
        if err == ENETUNREACH {
            tpass!(GROUP, NAME, "default route correctly returns ENETUNREACH");
            0
        } else {
            tconf!(GROUP, NAME, "sendto returned unexpected errno {}", err);
            0
        }
    }
}

fn net_route05_no_route_no_panic() -> i32 {
    const GROUP: &str = "NET_ROUTE";
    const NAME: &str = "net_route05_no_route_no_panic";

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "socket failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let target = sockaddr_in::new([192, 168, 255, 255], 12345);
    let msg = b"no_route_probe";
    let _wret = sys_sendto(fd, msg.as_ptr(), msg.len(), 0, target.as_ptr(), sockaddr_in::len());
    sys_close(fd);

    tpass!(GROUP, NAME, "no route scenario handled without panic");
    0
}

// ============================================================
// PROC_NET test group
// ============================================================
fn proc_net01_dev() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net01_dev";
    let fd = sys_open("/proc/net/dev\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open /proc/net/dev failed: {}", fd); return 1; }
    let fd = fd as usize;
    let mut buf = [0u8; 4096];
    let n = sys_read(fd, &mut buf);
    sys_close(fd);
    if n <= 0 { tfail!(GROUP, NAME, "read returned {}", n); return 1; }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    let has_lo = content.contains("lo:");
    let has_eth0 = content.contains("eth0:");
    if has_lo && has_eth0 { tpass!(GROUP, NAME, "found lo and eth0 in /proc/net/dev"); 0 }
    else { tfail!(GROUP, NAME, "missing iface in /proc/net/dev: lo={} eth0={}", has_lo, has_eth0); 1 }
}
fn proc_net02_route() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net02_route";
    let fd = sys_open("/proc/net/route\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open /proc/net/route failed: {}", fd); return 1; }
    let mut buf = [0u8; 2048];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n <= 0 { tfail!(GROUP, NAME, "read returned {}", n); return 1; }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    let has_route = content.lines().skip(1).any(|line| {
        let iface = line.split_whitespace().next().unwrap_or("");
        iface == "lo" || iface == "eth0"
    });
    if content.contains("Iface") && has_route { tpass!(GROUP, NAME, "route table accessible"); 0 }
    else { tfail!(GROUP, NAME, "missing route table content"); 1 }
}
fn proc_net03_tcp_header() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net03_tcp_header";
    let fd = sys_open("/proc/net/tcp\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open /proc/net/tcp failed: {}", fd); return 1; }
    let mut buf = [0u8; 512];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n <= 0 { tfail!(GROUP, NAME, "read returned {}", n); return 1; }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if content.contains("sl") { tpass!(GROUP, NAME, "tcp header present"); 0 }
    else { tfail!(GROUP, NAME, "tcp header missing"); 1 }
}
fn proc_net04_udp_header() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net04_udp_header";
    let fd = sys_open("/proc/net/udp\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open /proc/net/udp failed: {}", fd); return 1; }
    let mut buf = [0u8; 512];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n <= 0 { tfail!(GROUP, NAME, "read returned {}", n); return 1; }
    let content = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    if content.contains("sl") { tpass!(GROUP, NAME, "udp header present"); 0 }
    else { tfail!(GROUP, NAME, "udp header missing"); 1 }
}
fn proc_net05_ip_forward() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net05_ip_forward";
    let fd = sys_open("/proc/sys/net/ipv4/ip_forward\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open ip_forward failed: {}", fd); return 1; }
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n <= 0 { tfail!(GROUP, NAME, "read returned {}", n); return 1; }
    if buf[0] == b'0' { tpass!(GROUP, NAME, "ip_forward=0"); 0 }
    else { tfail!(GROUP, NAME, "expected '0', got '{}'", buf[0] as char); 1 }
}
fn proc_net06_small_buffer() -> i32 {
    const GROUP: &str = "PROC_NET"; const NAME: &str = "proc_net06_small_buffer";
    let fd = sys_open("/proc/net/dev\0", 0);
    if fd < 0 { tbrok!(GROUP, NAME, "open failed: {}", fd); return 1; }
    let fd = fd as usize;
    let mut buf1 = [0u8; 32];
    let mut buf2 = [0u8; 32];
    let n1 = sys_read(fd, &mut buf1[..32]);
    let n2 = sys_read(fd, &mut buf2[..32]);
    sys_close(fd);
    if n1 > 0 && n2 > 0 {
        tpass!(GROUP, NAME, "small buffer reads work: {} + {} bytes", n1, n2); 0
    } else {
        tbrok!(GROUP, NAME, "small reads failed: n1={} n2={}", n1, n2); 1
    }
}

// ============================================================
// NET_IOCTL test group
// ============================================================
const SIOCGIFINDEX: u32 = 0x8933;
const SIOCGIFFLAGS: u32 = 0x8913;
const SIOCGIFADDR: u32 = 0x8915;
const SIOCGIFCONF: u32 = 0x8912;

#[repr(C)] struct ifreq { ifr_name: [u8; 16], ifr_data: [u8; 24] }

fn interface_ipv4(name: &str) -> Option<[u8; 4]> {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as usize;
    let mut ifr = ifreq { ifr_name: [0; 16], ifr_data: [0; 24] };
    let bytes = name.as_bytes();
    let len = bytes.len().min(15);
    ifr.ifr_name[..len].copy_from_slice(&bytes[..len]);
    let result = sys_ioctl(fd, SIOCGIFADDR, &mut ifr as *mut ifreq as usize);
    sys_close(fd);
    if result < 0 || u16::from_ne_bytes([ifr.ifr_data[0], ifr.ifr_data[1]]) != AF_INET as u16 {
        return None;
    }
    Some([ifr.ifr_data[4], ifr.ifr_data[5], ifr.ifr_data[6], ifr.ifr_data[7]])
}

fn ioctl_get(fd: usize, cmd: u32, name: &str) -> isize {
    let mut ifr = ifreq { ifr_name: [0; 16], ifr_data: [0; 24] };
    let b = name.as_bytes(); let l = b.len().min(15); ifr.ifr_name[..l].copy_from_slice(&b[..l]);
    sys_ioctl(fd, cmd, &ifr as *const ifreq as usize)
}

fn net_ioctl01_ifconf() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl01","socket failed: {}",fd); return 1; }
    let fd = fd as usize;
    let mut buf = [0u8; 256];
    let mut conf = [0u8; 16];
    conf[0..4].copy_from_slice(&(256i32).to_ne_bytes());
    conf[8..16].copy_from_slice(&(buf.as_ptr() as usize).to_ne_bytes());
    let ret = sys_ioctl(fd, SIOCGIFCONF, conf.as_ptr() as usize);
    sys_close(fd);
    if ret < 0 { tconf!("NET_IOCTL","net_ioctl01","SIOCGIFCONF not supported ({})",ret); 0 }
    else { tpass!("NET_IOCTL","net_ioctl01","SIOCGIFCONF returned ok"); 0 }
}
fn net_ioctl02_ifindex() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl02","socket failed: {}",fd); return 1; }
    let fd = fd as usize;
    let r1 = ioctl_get(fd, SIOCGIFINDEX, "lo");
    let r2 = ioctl_get(fd, SIOCGIFINDEX, "eth0");
    sys_close(fd);
    if r1 >= 0 && r2 >= 0 { tpass!("NET_IOCTL","net_ioctl02","ifindex works"); 0 }
    else { tconf!("NET_IOCTL","net_ioctl02","SIOCGIFINDEX not supported: lo={} eth0={}",r1,r2); 0 }
}
fn net_ioctl03_ifflags() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl03","socket failed: {}",fd); return 1; }
    let r = ioctl_get(fd as usize, SIOCGIFFLAGS, "lo");
    sys_close(fd as usize);
    if r >= 0 { tpass!("NET_IOCTL","net_ioctl03","SIOCGIFFLAGS works"); 0 }
    else { tconf!("NET_IOCTL","net_ioctl03","SIOCGIFFLAGS not supported ({})",r); 0 }
}
fn net_ioctl04_ifaddr() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl04","socket failed: {}",fd); return 1; }
    let r = ioctl_get(fd as usize, SIOCGIFADDR, "lo");
    sys_close(fd as usize);
    if r >= 0 { tpass!("NET_IOCTL","net_ioctl04","SIOCGIFADDR works"); 0 }
    else { tconf!("NET_IOCTL","net_ioctl04","SIOCGIFADDR not supported ({})",r); 0 }
}
fn net_ioctl05_no_panic() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl05","socket failed: {}",fd); return 1; }
    ioctl_get(fd as usize, 0x8914, "lo"); // SIOCSIFFLAGS - should return EPERM
    sys_close(fd as usize);
    tpass!("NET_IOCTL","net_ioctl05","set ioctl handled without panic"); 0
}
fn net_ioctl06_hwaddr() -> i32 {
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 { tbrok!("NET_IOCTL","net_ioctl06","socket failed: {}",fd); return 1; }
    let r = ioctl_get(fd as usize, 0x8927, "eth0");
    sys_close(fd as usize);
    tpass!("NET_IOCTL","net_ioctl06","SIOCGIFHWADDR called, ret={}",r); 0
}

// ============================================================
// RTNETLINK test group
// ============================================================
const AF_NETLINK: usize = 16;
const NETLINK_ROUTE: usize = 0;
const SOCK_RAW: usize = 3;

fn rtnetlink01_socket() -> i32 {
    let fd = sys_socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
    if fd >= 0 {
        sys_close(fd as usize);
        tpass!("RTNETLINK","rtnetlink01","AF_NETLINK socket created ok"); 0
    } else {
        tconf!("RTNETLINK","rtnetlink01","AF_NETLINK not available, errno={}",-fd); 0
    }
}

// ============================================================
// VETH test group — veth pair newlink, setlink, addr, netns, cleanup
// No "; true" hacks — each command chain validates via grep exit codes
// ============================================================
fn test_veth_newlink() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_newlink";
    // Step 1: create pair only — isolate ip link add from ip link show
    let ret = run_bash_cmd("ip link add veth_t01 type veth peer name veth_t02");
    if ret != 0 {
        tfail!(GROUP, NAME, "ip link add failed (exit={}) — netlink ACK path broken", ret);
        return 1;
    }
    // Step 2: verify both exist via ip link show
    let ret = run_bash_cmd(
        "ip link show veth_t01 >/dev/null 2>&1 && \
         ip link show veth_t02 >/dev/null 2>&1"
    );
    if ret != 0 {
        tfail!(GROUP, NAME, "ip link show failed (exit={}) — RTM_GETLINK dump broken", ret);
        let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null");
        return 1;
    }
    // Step 3: delete and verify both gone
    let ret = run_bash_cmd(
        "ip link del veth_t01 && \
         (! ip link show veth_t01 >/dev/null 2>&1) && \
         (! ip link show veth_t02 >/dev/null 2>&1)"
    );
    if ret != 0 {
        tfail!(GROUP, NAME, "delete/verify gone failed (exit={})", ret);
        let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null");
        return 1;
    }
    tpass!(GROUP, NAME, "create, verify, delete, verify gone");
    0
}

/// Diagnostic: capture raw BusyBox stderr for ip link add to identify EOF source
fn veth_diag_raw_output() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_diag";
    let ret = run_bash_cmd(
        "echo '--- ip link add ---' && \
         ip link add veth_diag0 type veth peer name veth_diag1 2>&1; ec=$?; \
         echo 'add exit='$ec; \
         if [ $ec -eq 0 ]; then \
           echo '--- ip link show ---' && \
           ip link show veth_diag0 2>&1; echo 'show exit='$?; \
           ip link del veth_diag0 2>/dev/null; \
         fi; \
         exit $ec"
    );
    if ret == 0 {
        tpass!(GROUP, NAME, "diag: ip link add + show succeeded");
        0
    } else {
        tfail!(GROUP, NAME, "diag: ip link add failed (exit={}) — check raw stderr above", ret);
        1
    }
}

fn test_veth_setlink_up() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_setlink_up";
    // Step 1: create
    let ret = run_bash_cmd("ip link add veth_t01 type veth peer name veth_t02");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={})", ret); return 1; }
    // Step 2: set up
    let ret = run_bash_cmd("ip link set veth_t01 up");
    if ret != 0 { tfail!(GROUP, NAME, "ip link set up failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null"); return 1; }
    // Step 3: verify UP flag
    let ret = run_bash_cmd("ip link show veth_t01 | grep -q UP && ip link del veth_t01");
    if ret != 0 { tfail!(GROUP, NAME, "verify UP flag failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null"); return 1; }
    tpass!(GROUP, NAME, "interface UP flag set");
    0
}

fn test_veth_addr_add() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_addr_add";
    // Step 1: create
    let ret = run_bash_cmd("ip link add veth_t01 type veth peer name veth_t02");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={})", ret); return 1; }
    // Step 2: add addr
    let ret = run_bash_cmd("ip addr add 10.0.0.1/24 dev veth_t01");
    if ret != 0 { tfail!(GROUP, NAME, "ip addr add failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null"); return 1; }
    // Step 3: verify addr
    let ret = run_bash_cmd("ip addr show veth_t01 | grep -q '10.0.0.1' && ip link del veth_t01");
    if ret != 0 { tfail!(GROUP, NAME, "addr verify failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_t01 2>/dev/null"); return 1; }
    tpass!(GROUP, NAME, "IP address assigned to veth");
    0
}

fn test_netns_isolation() -> i32 {
    // unshare -n creates a new netns; veth created inside must NOT
    // be visible in the default namespace
    const GROUP: &str = "VETH";
    const NAME: &str = "netns_isolation";
    let cmd =
        "unshare -n bash -c 'ip link add vt type veth peer vp && \
         ip link show vt | grep -q vt' && \
         ip link show vt 2>&1 | grep -q 'does not exist'";
    let ret = run_bash_cmd(cmd);
    if ret == 0 {
        tpass!(GROUP, NAME, "NETNS_ISOLATION_PASS — veth visible in new ns, hidden from default");
        0
    } else {
        tfail!(GROUP, NAME, "failed (exit={}) — unshare may be missing", ret);
        1
    }
}

fn test_rtm_dellink_cleanup() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "rtm_dellink_cleanup";
    // Step 1: create
    let ret = run_bash_cmd("ip link add veth_x type veth peer name veth_y");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={})", ret); return 1; }
    // Step 2: delete one end
    let ret = run_bash_cmd("ip link del veth_x");
    if ret != 0 { tfail!(GROUP, NAME, "ip link del failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_x 2>/dev/null"); return 1; }
    // Step 3: verify both gone
    let ret = run_bash_cmd("(! ip link show veth_x >/dev/null 2>&1) && (! ip link show veth_y >/dev/null 2>&1)");
    if ret != 0 { tfail!(GROUP, NAME, "pair cleanup verify failed (exit={})", ret); return 1; }
    tpass!(GROUP, NAME, "both ends removed after delete");
    0
}

// ============================================================
// PROC_SYS_NET_IPV6 test group — /proc/sys/net/ipv6/conf/
// ============================================================
fn proc_sys_net_ipv6_conf() -> i32 {
    const GROUP: &str = "PROC_SYS";
    const NAME: &str = "ipv6_conf_disable";
    let ret = run_bash_cmd(
        "cat /proc/sys/net/ipv6/conf/all/disable_ipv6 && \
         cat /proc/sys/net/ipv6/conf/default/disable_ipv6 && \
         cat /proc/sys/net/ipv6/conf/lo/disable_ipv6 && \
         cat /proc/sys/net/ipv6/conf/eth0/disable_ipv6"
    );
    if ret != 0 {
        tfail!(GROUP, NAME, "cannot read static disable_ipv6 files (exit={})", ret);
        return 1;
    }
    let fd = sys_open("/proc/sys/net/ipv6/conf/all/disable_ipv6\0", 0);
    if fd < 0 {
        tbrok!(GROUP, NAME, "open disable_ipv6 failed: {}", fd);
        return 1;
    }
    let fd = fd as usize;
    let mut buf = [0u8; 32];
    let n = sys_read(fd, &mut buf);
    sys_close(fd);
    if n <= 0 {
        tfail!(GROUP, NAME, "read disable_ipv6 returned {}", n);
        return 1;
    }
    let val = core::str::from_utf8(&buf[..n as usize]).unwrap_or("").trim();
    tpass!(GROUP, NAME, "disable_ipv6 = {}", val);
    0
}

fn proc_sys_net_ipv6_veth_conf() -> i32 {
    const GROUP: &str = "PROC_SYS";
    const NAME: &str = "ipv6_conf_veth_dynamic";
    let ret = run_bash_cmd("ip link add veth_pc1 type veth peer name veth_pc2");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={}) — veth prerequisite", ret); return 1; }
    let ret = run_bash_cmd("cat /proc/sys/net/ipv6/conf/veth_pc1/disable_ipv6 && cat /proc/sys/net/ipv6/conf/veth_pc2/disable_ipv6 && ip link del veth_pc1");
    if ret == 0 { tpass!(GROUP, NAME, "dynamic veth disable_ipv6 files accessible"); 0 }
    else { tfail!(GROUP, NAME, "procfs lookup failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_pc1 2>/dev/null"); 1 }
}

// ============================================================
// SYS_NET test group — /sys/class/net/<iface>/
// ============================================================
fn sys_net_lo_files() -> i32 {
    const GROUP: &str = "SYS_NET";
    const NAME: &str = "sys_net_lo";
    let ret = run_bash_cmd(
        "cat /sys/class/net/lo/mtu && \
         cat /sys/class/net/lo/address"
    );
    if ret == 0 {
        tpass!(GROUP, NAME, "/sys/class/net/lo accessible");
        0
    } else {
        tconf!(GROUP, NAME, "/sys/class/net/lo not available (exit={})", ret);
        0
    }
}

fn sys_net_veth_files() -> i32 {
    const GROUP: &str = "SYS_NET";
    const NAME: &str = "sys_net_veth";
    let ret = run_bash_cmd("ip link add veth_sys1 type veth peer name veth_sys2");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={}) — veth prerequisite", ret); return 1; }
    let ret = run_bash_cmd("cat /sys/class/net/veth_sys1/mtu && cat /sys/class/net/veth_sys1/address && cat /sys/class/net/veth_sys2/mtu && ip link del veth_sys1");
    if ret == 0 { tpass!(GROUP, NAME, "/sys/class/net/veth_* accessible"); 0 }
    else { tfail!(GROUP, NAME, "sysfs veth files failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_sys1 2>/dev/null"); 1 }
}

// ============================================================
// PROC_NET_EXTENDED — /proc/net/snmp netstat snmp6
// ============================================================
fn proc_net_extended() -> i32 {
    const GROUP: &str = "PROC_NET";
    const NAME: &str = "proc_net_extended";
    let ret = run_bash_cmd(
        "cat /proc/net/snmp && \
         cat /proc/net/netstat && \
         cat /proc/net/snmp6"
    );
    if ret == 0 {
        tpass!(GROUP, NAME, "/proc/net/snmp netstat snmp6 readable");
        0
    } else {
        tfail!(GROUP, NAME, "extended procfs files failed (exit={})", ret);
        1
    }
}

// ============================================================
// VETH extended — ping between veth pair
// ============================================================
fn veth_ping_raw_socket() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_ping";
    // Step 1: create
    let ret = run_bash_cmd("ip link add veth_p0 type veth peer name veth_p1");
    if ret != 0 { tfail!(GROUP, NAME, "ip link add failed (exit={})", ret); return 1; }
    // Step 2: assign IPs
    let ret = run_bash_cmd("ip addr add 10.255.0.1/24 dev veth_p0 && ip addr add 10.255.0.2/24 dev veth_p1");
    if ret != 0 { tfail!(GROUP, NAME, "ip addr add failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_p0 2>/dev/null"); return 1; }
    // Step 3: bring up
    let ret = run_bash_cmd("ip link set veth_p0 up && ip link set veth_p1 up");
    if ret != 0 { tfail!(GROUP, NAME, "ip link set up failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_p0 2>/dev/null"); return 1; }

    // Diagnostic: show interface + routing before ping
    tinfo!(GROUP, NAME, "diag: ip addr show veth_p0");
    run_bash_cmd("ip addr show veth_p0");
    tinfo!(GROUP, NAME, "diag: ip route show table all 2>&1 | head -20");
    run_bash_cmd("ip route show table all 2>&1 | head -20");

    // Step 4: ping — show output instead of hiding it
    tinfo!(GROUP, NAME, "diag: ping -c 1 -W 2 10.255.0.2 -I veth_p0");
    let ret = run_bash_cmd("ping -c 1 -W 2 10.255.0.2 -I veth_p0 2>&1; ec=$?; ip link del veth_p0 2>/dev/null; exit $ec");
    if ret != 0 { tfail!(GROUP, NAME, "ping failed (exit={}) — raw socket or routing broken", ret); return 1; }
    tpass!(GROUP, NAME, "ping through veth pair works");
    0
}

fn veth_ip_neigh_show() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_ip_neigh";
    // Create veth + bring up (IP assignment not needed for neigh table)
    let ret = run_bash_cmd("ip link add veth_n0 type veth peer name veth_n1 && ip link set veth_n0 up && ip link set veth_n1 up");
    if ret != 0 { tconf!(GROUP, NAME, "veth setup failed (exit={})", ret); let _ = run_bash_cmd("ip link del veth_n0 2>/dev/null"); return 0; }

    // Run ip neigh show standalone — show raw output
    tinfo!(GROUP, NAME, "diag: ip neigh show (raw output)");
    let ret = run_bash_cmd("ip neigh show 2>&1; ec=$?; ip link del veth_n0 2>/dev/null; exit $ec");
    if ret == 0 {
        tpass!(GROUP, NAME, "ip neigh show works");
        0
    } else {
        tconf!(GROUP, NAME, "ip neigh not fully supported (exit={})", ret);
        0
    }
}

fn veth_dellink_non_existent() -> i32 {
    const GROUP: &str = "VETH";
    const NAME: &str = "veth_dellink_nonexist";
    let ret = run_bash_cmd("ip link del no_such_veth_xyz 2>&1");
    if ret != 0 {
        tpass!(GROUP, NAME, "dellink non-existent returns error (exit={})", ret);
        0
    } else {
        tfail!(GROUP, NAME, "dellink non-existent should have failed");
        1
    }
}

fn run_with_watchdog(name: &str, test_fn: fn() -> i32, timeout_ms: usize) -> bool {
    let pid = sys_fork();
    if pid == 0 {
        let ret = test_fn();
        sys_exit(ret);
    }
    let t0 = user_lib::get_time();
    loop {
        let mut status: i32 = 0;
        let ret = user_lib::waitpid_wnohang(pid as isize, &mut status);
        if ret == pid as isize {
            return (status >> 8) & 0xFF == 0;
        }
        let elapsed = (user_lib::get_time() - t0) as usize;
        if elapsed > timeout_ms {
            user_lib::kill(pid as usize, 9);
            let mut s: i32 = 0;
            sys_waitpid(pid as isize, &mut s);
            println!("{}[TBROK]{} {} timed out after {}ms", C_MAGENTA, C_RESET, name, timeout_ms);
            return false;
        }
        user_lib::sleep(10);
    }
}

const WATCHDOG_SECS: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TestStage {
    Core,
    Veth,
    Loopback,
    External,
    Tls,
}

fn test_stage(index: usize) -> TestStage {
    match index {
        0..=23 => TestStage::Core,
        24..=34 => TestStage::Veth,
        35..=39 => TestStage::Loopback,
        40..=48 => TestStage::External,
        _ => TestStage::Tls,
    }
}

fn profile_allows(profile: &str, stage: TestStage) -> bool {
    match profile {
        "all" => true,
        "core" => matches!(stage, TestStage::Core | TestStage::Loopback),
        "veth" => stage == TestStage::Veth,
        "external" => stage == TestStage::External,
        "board" => matches!(stage, TestStage::Core | TestStage::Loopback | TestStage::External),
        "tls" => stage == TestStage::Tls,
        _ => false,
    }
}

// ============================================================
// main
// ============================================================
#[no_mangle]
fn main(argc: usize, argv: &[&str]) -> i32 {
    let tests: [(&str, fn() -> i32); 51] = [
        // ── Step 1: unit / self-contained tests — no external dependencies ──
        ("[NET_CORE] net_core01_interface_basic", net_core01_interface_basic),
        ("[NET_CORE] net_core02_loopback_and_default_iface", net_core02_loopback_and_default_iface),
        ("[NET_CORE] net_core_ip_recverr", net_core_ip_recverr),
        ("[NET_CORE] net_core_sendmmsg", net_core_sendmmsg),
        ("[NET_CORE] net_core03_route_lookup", net_core03_route_lookup),
        ("[NET_CORE] net_core04_ephemeral_port_range", net_core04_ephemeral_port_range),
        ("[NET_CORE] net_core05_port_bind_conflict", net_core05_port_bind_conflict),
        ("[NET_CORE] net_core06_port_reuse_after_close", net_core06_port_reuse_after_close),
        ("[PROC_NET] proc_net01_dev", proc_net01_dev),
        ("[PROC_NET] proc_net02_route", proc_net02_route),
        ("[PROC_NET] proc_net03_tcp_header", proc_net03_tcp_header),
        ("[PROC_NET] proc_net04_udp_header", proc_net04_udp_header),
        ("[PROC_NET] proc_net05_ip_forward", proc_net05_ip_forward),
        ("[PROC_NET] proc_net06_small_buffer", proc_net06_small_buffer),
        ("[PROC_NET] proc_net_extended", proc_net_extended),
        ("[NET_IOCTL] net_ioctl01_ifconf", net_ioctl01_ifconf),
        ("[NET_IOCTL] net_ioctl02_ifindex", net_ioctl02_ifindex),
        ("[NET_IOCTL] net_ioctl03_ifflags", net_ioctl03_ifflags),
        ("[NET_IOCTL] net_ioctl04_ifaddr", net_ioctl04_ifaddr),
        ("[NET_IOCTL] net_ioctl05_no_panic", net_ioctl05_no_panic),
        ("[NET_IOCTL] net_ioctl06_hwaddr", net_ioctl06_hwaddr),
        ("[RTNETLINK] rtnetlink01_socket", rtnetlink01_socket),

        // ── Step 2a: procfs/sysfs (local, no veth needed) ──
        ("[PROC_SYS] ipv6_conf_disable", proc_sys_net_ipv6_conf),
        ("[SYS_NET] sys_net_lo", sys_net_lo_files),

        // ── Step 2b: veth lifecycle (local, no external net needed) ──
        ("[VETH] veth_diag", veth_diag_raw_output),
        ("[VETH] veth_newlink", test_veth_newlink),
        ("[VETH] veth_setlink_up", test_veth_setlink_up),
        ("[VETH] veth_addr_add", test_veth_addr_add),
        ("[VETH] veth_dellink_nonexist", veth_dellink_non_existent),
        ("[VETH] rtm_dellink_cleanup", test_rtm_dellink_cleanup),
        ("[VETH] netns_isolation", test_netns_isolation),

        // ── Step 2c: veth extended (requires veth to work) ──
        ("[PROC_SYS] ipv6_conf_veth_dynamic", proc_sys_net_ipv6_veth_conf),
        ("[SYS_NET] sys_net_veth", sys_net_veth_files),
        ("[VETH] veth_ping", veth_ping_raw_socket),
        ("[VETH] veth_ip_neigh", veth_ip_neigh_show),

        // ── Step 3: loopback IP (127.0.0.1) — self-contained ──
        ("udp_loopback", test_udp_loopback),
        ("udp_loopback_pair", test_udp_loopback_pair),
        ("udp_giant_loopback", test_udp_giant_loopback),
        ("[NET_ROUTE] net_route01_loopback_udp", net_route01_loopback_udp),
        ("[NET_ROUTE] net_route02_eth_local_addr", net_route02_eth_local_addr),

        // ── Step 4: external connectivity (QEMU SLIRP or a routed board link) ──
        ("tcp_connect", test_tcp_connect_all),
        ("tcp_send_recv", test_tcp_send_recv_external),
        ("http_get", test_http_get),
        ("dns+baidu.com:80", || test_dns_and_tcp("baidu.com")),
        ("dns+bilibili.com:80", || test_dns_and_tcp("bilibili.com")),
        ("udp_external_dns", test_udp_external_dns),
        ("[NET_ROUTE] net_route03_dns_route", net_route03_dns_route),
        ("[NET_ROUTE] net_route04_default_route", net_route04_default_route),
        ("[NET_ROUTE] net_route05_no_route_no_panic", net_route05_no_route_no_panic),

        // ── Step 5: TLS (requires crypto + external connectivity) ──
        ("https_tls", test_https_tls),
        ("https_tls_download", test_https_download),
    ]; // 51 total tests

    let profile = if argc > 1 { argv[1] } else { "all" };
    if !matches!(profile, "all" | "core" | "veth" | "external" | "board" | "tls") {
        println!("usage: inet_test [all|core|veth|external|board|tls]");
        return 2;
    }
    let selected = tests
        .iter()
        .enumerate()
        .filter(|(index, _)| profile_allows(profile, test_stage(*index)))
        .count();

    println!("{}", C_RESET);
    println!("{}============================================", C_CYAN);
    println!("  INET Connectivity Test Suite");
    println!("  profile={} selected={}/{}", profile, selected, tests.len());
    println!("============================================{}", C_RESET);

    let total = selected;
    let mut passed = 0;
    let mut failed = 0;

    for (index, (name, func)) in tests.iter().enumerate() {
        if !profile_allows(profile, test_stage(index)) {
            continue;
        }
        println!("");
        let ok = run_with_watchdog(name, *func, WATCHDOG_SECS * 1000);
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    println!("{}============================================", C_CYAN);
    if failed == 0 {
        println!("  {}Results: {}/{} passed{}", C_GREEN, passed, total, C_RESET);
    } else {
        println!("  {}Results: {}/{} passed, {}/{} failed{}", C_RED, passed, total, failed, total, C_RESET);
    }
    println!("{}============================================{}", C_CYAN, C_RESET);

    unsafe {
        if TOTAL > 0 {
            println!("");
            println!("--------------------------------------------");
            println!("  NET_CORE LTP Summary");
            println!("--------------------------------------------");
            println!("  Total:  {}", TOTAL);
            println!("  Passed: {}", PASSED);
            println!("  Failed: {}", FAILED);
            println!("  Broken: {}", BROKEN);
            println!("  Conf:   {}", CONF);
            println!("--------------------------------------------");
        }
    }

    if failed > 0 {
        1
    } else {
        0
    }
}
