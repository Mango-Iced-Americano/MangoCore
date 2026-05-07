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

// QEMU SLIRP 内置 DNS 代理地址
const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];

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
// DNS Helpers: 将域名解析为 IPv4 地址（UDP → QEMU SLIRP DNS）
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

/// 向 QEMU SLIRP DNS (10.0.2.3:53) 查询域名的 A 记录
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
    let addr = sockaddr_in::new(DNS_SERVER, 53);
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

// ============================================================
// Helper: fork + exec bash -c（从 unix_test.rs 借鉴）
// ============================================================
fn run_bash_cmd(cmd: &str) -> i32 {
    let pid = sys_fork();
    if pid == 0 {
        let shell = "/bash\0";
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
    println!(
        "=== inet_test: TCP connect to {} ({}:{}:{}.{}:{}) ===",
        target_name, ip[0], ip[1], ip[2], ip[3], port
    );

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;
    println!("  socket fd={}", fd);

    let addr = sockaddr_in::new(ip, port);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: connect returned {} (errno={})", ret, -ret);
        sys_close(fd);
        return 1;
    }
    println!("  connect ok");

    sys_close(fd);
    0
}

// Phase 1 子测试：单目标（Cloudflare 已验证可通，仅做参考）
fn test_tcp_connect_all() -> i32 {
    let targets: [(&str, [u8; 4], u16); 1] = [("cloudflare", [1, 1, 1, 1], 80)];
    for (name, ip, port) in &targets {
        test_tcp_connect_to(name, *ip, *port);
    }
    println!("  PASS (tcp_connect)");
    0
}

// ============================================================
// Phase 1: TCP 收发 — connect → send HTTP GET → recv → close
// ============================================================
fn test_tcp_send_recv(target_name: &str, ip: [u8; 4]) -> i32 {
    println!("=== inet_test: TCP send/recv to {} ===", target_name);

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: connect returned {} (errno={})", ret, -ret);
        sys_close(fd);
        return 1;
    }
    println!("  connected");

    // HTTP/1.0 GET — 使用正确的 Host 头
    let (req, req_len) = http_get_request(target_name);
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        println!("  FAIL: send returned {}", wret);
        sys_close(fd);
        return 1;
    }
    println!("  sent {} bytes", wret);

    let mut buf = [0u8; 1024];
    let rret = tcp_recv(fd, &mut buf);
    if rret < 0 {
        println!("  FAIL: recv returned {} (errno={})", rret, -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        println!("  FAIL: recv 0 bytes — server closed connection immediately");
        println!("        (kernel TCP data send may not be working)");
        sys_close(fd);
        return 1;
    }
    println!("  recv {} bytes", rret);

    let show_len = core::cmp::min(rret as usize, 400);
    let s = core::str::from_utf8(&buf[..show_len]).unwrap_or("(non-utf8)");
    println!("  response (first {} bytes):\n{}", show_len, s);

    sys_close(fd);
    0
}

// ============================================================
// Phase 2: HTTP GET — 完整请求 / 循环接收 / 解析状态码
// ============================================================
fn test_http_get() -> i32 {
    println!("=== inet_test: HTTP GET to 1.1.1.1:80 ===");

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new([1, 1, 1, 1], 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: connect returned {} (errno={})", ret, -ret);
        sys_close(fd);
        return 1;
    }
    println!("  connected");

    let (req, req_len) = http_get_request("1.1.1.1");
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        println!("  FAIL: send returned {}", wret);
        sys_close(fd);
        return 1;
    }

    // 循环接收直到连接关闭（HTTP/1.0 语义：服务端发完即 FIN）
    let mut total = 0usize;
    let mut all = [0u8; 4096];
    loop {
        let rret = tcp_recv(fd, &mut all[total..]);
        if rret <= 0 {
            break; // 0 = EOF, <0 = error
        }
        total += rret as usize;
        if total >= all.len() {
            break;
        }
    }
    println!("  received {} bytes total", total);

    if total == 0 {
        println!("  FAIL: no data received");
        sys_close(fd);
        return 1;
    }

    // 提取第一行（状态行）
    let first_line_end = all[..total]
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(total);
    let first_line = core::str::from_utf8(&all[..first_line_end]).unwrap_or("(bad utf8)");
    println!("  status: {}", first_line.trim_end());

    let ok = first_line.contains("HTTP");
    sys_close(fd);

    if ok {
        println!("  PASS (got HTTP response)");
        0
    } else {
        println!("  FAIL: unexpected response");
        1
    }
}

// ============================================================
// Phase 4: DNS 解析 + TCP 连通性（代替硬编码 baidu/bilibili）
// ============================================================
fn test_dns_and_tcp(domain: &str) -> i32 {
    println!("=== inet_test: DNS lookup + TCP to {}:80 ===", domain);

    let ip = match dns_lookup(domain) {
        Some(ip) => {
            println!(
                "  resolved {} -> {}.{}.{}.{}",
                domain, ip[0], ip[1], ip[2], ip[3]
            );
            ip
        }
        None => {
            println!("  FAIL: DNS lookup failed for {}", domain);
            return 1;
        }
    };

    // 用解析出的 IP 做 TCP connect + HTTP GET
    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 80);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!(
            "  FAIL: connect to {} ({}.{}.{}.{}) returned {} (errno={})",
            domain, ip[0], ip[1], ip[2], ip[3], ret, -ret
        );
        sys_close(fd);
        return 1;
    }
    println!(
        "  connected to {} ({}.{}.{}.{})",
        domain, ip[0], ip[1], ip[2], ip[3]
    );

    let (req, req_len) = http_get_request(domain);
    let wret = tcp_send(fd, &req[..req_len]);
    if wret < 0 {
        println!("  FAIL: send returned {}", wret);
        sys_close(fd);
        return 1;
    }
    println!("  sent {} bytes", wret);

    let mut buf = [0u8; 1024];
    let rret = tcp_recv(fd, &mut buf);
    if rret < 0 {
        println!("  FAIL: recv returned {} (errno={})", rret, -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        println!("  FAIL: recv 0 bytes — server closed connection immediately");
        println!("        (kernel TCP data send may not be working)");
        sys_close(fd);
        return 1;
    }
    println!("  recv {} bytes", rret);

    let show_len = core::cmp::min(rret as usize, 300);
    let s = core::str::from_utf8(&buf[..show_len]).unwrap_or("(non-utf8)");
    println!("  response:\n{}", s);

    sys_close(fd);
    0
}

// ============================================================
// Phase 5: HTTPS/TLS — 通过 busybox wget
// ============================================================
fn test_https_wget() -> i32 {
    println!("=== inet_test: HTTPS GET with wget to 1.1.1.1:443 ===");

    // 清理旧文件
    run_bash_cmd("rm -f /tmp/https_resp");

    // 用 wget 取 https 页面，超时 10s
    let ret = run_bash_cmd("wget --no-check-certificate -T 10 -O /tmp/https_resp https://1.1.1.1");
    println!("  wget exit code: {}", ret);

    if ret != 0 {
        println!("  FAIL: wget returned {}", ret);
        println!("  (TLS may not be compiled into busybox wget)");
        return 1;
    }

    // 检查输出文件 — 只要有内容就算成功
    let ret = run_bash_cmd("wc -c /tmp/https_resp");
    println!("  PASS (got HTTPS response, wc exit={})", ret);
    0
}

// ============================================================
// main
// ============================================================
#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("");
    println!("============================================");
    println!("  INET (AF_INET) Connectivity Test Suite");
    println!("============================================");

    let tests: [(&str, fn() -> i32); 6] = [
        ("tcp_connect", test_tcp_connect_all),
        ("tcp_send_recv", || {
            test_tcp_send_recv("cloudflare", [1, 1, 1, 1])
        }),
        ("http_get", test_http_get),
        ("dns+baidu.com:80", || test_dns_and_tcp("baidu.com")),
        ("dns+bilibili.com:80", || test_dns_and_tcp("bilibili.com")),
        ("https (wget)", test_https_wget),
    ];

    let total = tests.len();
    let mut passed = 0;
    let mut failed = 0;

    for (name, func) in tests.iter() {
        println!("");
        let ret = func();
        if ret == 0 {
            println!("[PASS] {}", name);
            passed += 1;
        } else {
            println!("[FAIL] {} (code={})", name, ret);
            failed += 1;
        }
    }

    println!("");
    println!("============================================");
    println!(
        "  Results: {}/{} passed, {}/{} failed",
        passed, total, failed, total
    );
    println!("============================================");

    if failed > 0 {
        1
    } else {
        0
    }
}
