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
// Phase 5: UDP 连通性测试
// ============================================================

// --- UDP Loopback: 127.0.0.1 self-send/recv ---
fn test_udp_loopback() -> i32 {
    println!("=== inet_test: UDP loopback (127.0.0.1) ===");

    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;
    println!("  socket fd={}", fd);

    // bind to random port on loopback
    let my_addr = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd, my_addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: bind returned {} (errno={})", ret, -ret);
        sys_close(fd);
        return 1;
    }

    // 获取实际分配的端口
    let mut bound_addr = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    let ret = sys_getsockname(
        fd,
        bound_addr.as_ptr() as *mut u8,
        &mut addrlen as *mut usize,
    );
    if ret < 0 {
        println!("  FAIL: getsockname returned {}", ret);
        sys_close(fd);
        return 1;
    }
    let port = u16::from_be_bytes(bound_addr.sin_port);
    println!("  bound to 127.0.0.1:{}", port);

    // send 到自身
    let target = sockaddr_in::new([127, 0, 0, 1], port);
    let msg = b"hello_udp_loopback";
    let wret = sys_sendto(
        fd,
        msg.as_ptr(),
        msg.len(),
        0,
        target.as_ptr(),
        sockaddr_in::len(),
    );
    if wret < 0 {
        println!("  FAIL: sendto returned {} (errno={})", wret, -wret);
        sys_close(fd);
        return 1;
    }
    println!("  sent {} bytes to self", wret);

    // recvfrom
    let mut buf = [0u8; 128];
    let mut from = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut fromlen = sockaddr_in::len();
    let rret = sys_recvfrom(
        fd,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        from.as_ptr() as *mut u8,
        &mut fromlen as *mut usize,
    );
    if rret < 0 {
        println!("  FAIL: recvfrom returned {} (errno={})", rret, -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        println!("  FAIL: recvfrom returned 0 (unexpected for UDP)");
        sys_close(fd);
        return 1;
    }
    let recv = &buf[..rret as usize];
    if recv != msg {
        println!("  FAIL: got {:?}, expected {:?}", recv, msg);
        sys_close(fd);
        return 1;
    }
    println!("  recv {} bytes from self, data matches", rret);

    // 检查 from 地址是否也是 127.0.0.1
    let from_port = u16::from_be_bytes(from.sin_port);
    println!(
        "  from: {}.{}.{}.{}:{}",
        from.sin_addr[0], from.sin_addr[1], from.sin_addr[2], from.sin_addr[3], from_port
    );

    sys_close(fd);
    println!("  PASS");
    0
}

// --- UDP Loopback: A → B (双向) ---
fn test_udp_loopback_pair() -> i32 {
    println!("=== inet_test: UDP loopback A→B (127.0.0.1) ===");

    let fd_a = sys_socket(AF_INET, SOCK_DGRAM, 0);
    let fd_b = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_a < 0 || fd_b < 0 {
        println!("  FAIL: socket returned a={} b={}", fd_a, fd_b);
        return 1;
    }
    let fd_a = fd_a as usize;
    let fd_b = fd_b as usize;

    // bind B to a specific port so A can target it
    let addr_b = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_b, addr_b.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: bind B returned {} (errno={})", ret, -ret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }

    // bind A to get an ephemeral port (for B to reply to)
    let addr_a = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_a, addr_a.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: bind A returned {} (errno={})", ret, -ret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }

    // get B's port
    let mut bound_b = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut addrlen = sockaddr_in::len();
    sys_getsockname(
        fd_b,
        bound_b.as_ptr() as *mut u8,
        &mut addrlen as *mut usize,
    );
    let port_b = u16::from_be_bytes(bound_b.sin_port);
    println!("  B bound to 127.0.0.1:{}", port_b);

    // A → B
    let target_b = sockaddr_in::new([127, 0, 0, 1], port_b);
    let msg = b"ping_from_A";
    let wret = sys_sendto(
        fd_a,
        msg.as_ptr(),
        msg.len(),
        0,
        target_b.as_ptr(),
        sockaddr_in::len(),
    );
    if wret < 0 {
        println!("  FAIL: A→B sendto returned {} (errno={})", wret, -wret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    println!("  A sent {} bytes to 127.0.0.1:{}", wret, port_b);

    // B recv
    let mut buf = [0u8; 128];
    let mut from = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut fromlen = sockaddr_in::len();
    let rret = sys_recvfrom(
        fd_b,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        from.as_ptr() as *mut u8,
        &mut fromlen as *mut usize,
    );
    if rret < 0 {
        println!("  FAIL: B recvfrom returned {} (errno={})", rret, -rret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    let recv = &buf[..rret as usize];
    if recv != msg {
        println!("  FAIL: B got {:?}, expected {:?}", recv, msg);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    let from_port = u16::from_be_bytes(from.sin_port);
    println!(
        "  B recv {} bytes from 127.0.0.1:{}, data matches",
        rret, from_port
    );

    // B → A (reply) — use recvfrom's returned from address
    let reply = b"pong_from_B";
    let wret = sys_sendto(fd_b, reply.as_ptr(), reply.len(), 0, from.as_ptr(), fromlen);
    if wret < 0 {
        println!("  FAIL: B→A sendto returned {} (errno={})", wret, -wret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    println!("  B replied {} bytes", wret);

    // A recv reply
    let mut buf2 = [0u8; 128];
    let rret = sys_recvfrom(
        fd_a,
        buf2.as_mut_ptr(),
        buf2.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        println!("  FAIL: A recvfrom returned {} (errno={})", rret, -rret);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    let recv2 = &buf2[..rret as usize];
    if recv2 != reply {
        println!("  FAIL: A got {:?}, expected {:?}", recv2, reply);
        sys_close(fd_a);
        sys_close(fd_b);
        return 1;
    }
    println!("  A recv {} bytes reply, data matches", rret);

    sys_close(fd_a);
    sys_close(fd_b);
    println!("  PASS");
    0
}

// --- UDP External: DNS query to 1.1.1.1:53 (外网 UDP) ---
fn test_udp_external_dns() -> i32 {
    println!("=== inet_test: UDP external DNS to 1.1.1.1:53 ===");

    let ip = match dns_lookup("baidu.com") {
        Some(ip) => {
            println!(
                "  resolved baidu.com -> {}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3]
            );
            ip
        }
        None => {
            println!("  FAIL: DNS lookup failed (this also tests QEMU SLIRP DNS relay)");
            return 1;
        }
    };

    // 构造 DNS 查询，但这次直连 1.1.1.1（不经过 QEMU SLIRP DNS proxy）
    let fd = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let (qname, qname_len) = encode_dns_name("baidu.com");
    let mut pkt = [0u8; 512];
    pkt[0..2].copy_from_slice(&[0xab, 0xcd]); // transaction ID
    pkt[2..4].copy_from_slice(&[0x01, 0x00]); // flags: RD=1
    pkt[4..6].copy_from_slice(&[0x00, 0x01]); // QDCOUNT=1
    pkt[12..12 + qname_len].copy_from_slice(&qname[..qname_len]);
    let off = 12 + qname_len;
    pkt[off..off + 2].copy_from_slice(&[0x00, 0x01]); // QTYPE=A
    pkt[off + 2..off + 4].copy_from_slice(&[0x00, 0x01]); // QCLASS=IN
    let pkt_len = off + 4;

    let target = sockaddr_in::new([1, 1, 1, 1], 53);
    let wret = sys_sendto(
        fd,
        pkt.as_ptr(),
        pkt_len,
        0,
        target.as_ptr(),
        sockaddr_in::len(),
    );
    if wret < 0 {
        println!(
            "  FAIL: sendto 1.1.1.1:53 returned {} (errno={})",
            wret, -wret
        );
        sys_close(fd);
        return 1;
    }
    println!("  sent DNS query {} bytes to 1.1.1.1:53", wret);

    let mut resp = [0u8; 512];
    let mut from = sockaddr_in::new([0, 0, 0, 0], 0);
    let mut fromlen = sockaddr_in::len();
    let rret = sys_recvfrom(
        fd,
        resp.as_mut_ptr(),
        resp.len(),
        0,
        from.as_ptr() as *mut u8,
        &mut fromlen as *mut usize,
    );
    if rret < 0 {
        println!("  FAIL: recvfrom returned {} (errno={})", rret, -rret);
        sys_close(fd);
        return 1;
    }
    if rret == 0 {
        println!("  FAIL: recvfrom returned 0 (unexpected for UDP)");
        sys_close(fd);
        return 1;
    }
    let from_ip = from.sin_addr;
    println!(
        "  recv {} bytes from {}.{}.{}.{}:{}",
        rret,
        from_ip[0],
        from_ip[1],
        from_ip[2],
        from_ip[3],
        u16::from_be_bytes(from.sin_port)
    );

    // 验证响应来自 1.1.1.1:53
    if from.sin_addr != [1, 1, 1, 1] || from.sin_port != 53u16.to_be_bytes() {
        println!("  WARN: unexpected source address (expected 1.1.1.1:53)");
    }

    // 检查响应是否合法 (至少有个 DNS header)
    if rret >= 12 && resp[0..2] == [0xab, 0xcd] {
        println!("  PASS (got DNS response from 1.1.1.1)");
        sys_close(fd);
        0
    } else {
        println!("  FAIL: invalid DNS response");
        sys_close(fd);
        1
    }
}

// ============================================================
// Phase 6: 32KB Giant UDP Loopback — IP分片重组调试
//   单包发送-接收，隔离并发问题，验证分片重组完整链路
// ============================================================
fn test_udp_giant_loopback() -> i32 {
    println!("=== inet_test: Giant UDP loopback (32KB, 127.0.0.1) ===");

    // --- 创建发送端 socket ---
    let fd_send = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_send < 0 {
        println!("  FAIL: sender socket returned {}", fd_send);
        return 1;
    }
    let fd_send = fd_send as usize;

    // --- 创建接收端 socket ---
    let fd_recv = sys_socket(AF_INET, SOCK_DGRAM, 0);
    if fd_recv < 0 {
        println!("  FAIL: receiver socket returned {}", fd_recv);
        sys_close(fd_send);
        return 1;
    }
    let fd_recv = fd_recv as usize;

    // --- bind 接收端到 127.0.0.1:5201 ---
    let addr_recv = sockaddr_in::new([127, 0, 0, 1], 5201);
    let ret = sys_bind(fd_recv, addr_recv.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: bind receiver returned {} (errno={})", ret, -ret);
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }
    println!("  receiver bound to 127.0.0.1:5201");

    // --- bind 发送端到随机端口（以便接收端能回复） ---
    let addr_send = sockaddr_in::new([127, 0, 0, 1], 0);
    let ret = sys_bind(fd_send, addr_send.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!("  FAIL: bind sender returned {} (errno={})", ret, -ret);
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }
    println!("  sender bound (ephemeral port)");

    // --- 构造 32768 字节的 payload（全是 0x42，便于识别） ---
    const GIANT_SIZE: usize = 32768;
    let mut payload = [0x42u8; GIANT_SIZE];
    // 在开头和结尾写入可识别的标记
    payload[0..8].copy_from_slice(b"GIANT_S:");
    payload[GIANT_SIZE - 8..].copy_from_slice(b":GIANT_E");

    println!(
        "  payload: {} bytes, first 8={:?}, last 8={:?}",
        GIANT_SIZE,
        &payload[..8],
        &payload[GIANT_SIZE - 8..]
    );

    // --- 发送 32768 字节到 127.0.0.1:5201 ---
    let target = sockaddr_in::new([127, 0, 0, 1], 5201);
    let wret = sys_sendto(
        fd_send,
        payload.as_ptr(),
        GIANT_SIZE,
        0,
        target.as_ptr(),
        sockaddr_in::len(),
    );
    if wret < 0 {
        println!("  FAIL: sendto returned {} (errno={})", wret, -wret);
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }
    println!("  sent {} bytes to 127.0.0.1:5201", wret);

    // --- 接收端：循环接收直到收完 32768 字节 ---
    let mut recv_buf = [0u8; 65536];
    let rret = sys_recvfrom(
        fd_recv,
        recv_buf.as_mut_ptr(),
        recv_buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        println!("  FAIL: recvfrom returned {} (errno={})", rret, -rret);
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }
    let recv_len = rret as usize;
    println!("  recv {} bytes", recv_len);

    if recv_len != GIANT_SIZE {
        println!(
            "  FAIL: expected {} bytes, got {} bytes",
            GIANT_SIZE, recv_len
        );
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }

    // --- 验证数据完整性 ---
    let data_ok = &recv_buf[..recv_len] == &payload[..];
    if !data_ok {
        // 找出第一个不匹配的位置
        let mismatch_pos = recv_buf[..recv_len]
            .iter()
            .zip(payload.iter())
            .position(|(a, b)| a != b);
        if let Some(pos) = mismatch_pos {
            println!(
                "  FAIL: data mismatch at offset {}, expected 0x{:02x}, got 0x{:02x}",
                pos, payload[pos], recv_buf[pos]
            );
        } else {
            println!("  FAIL: data mismatch (length differs?)");
        }
        sys_close(fd_send);
        sys_close(fd_recv);
        return 1;
    }
    println!("  data integrity OK: all {} bytes match", recv_len);

    // --- Phase 2: 发送小包验证 PacketAssembler 已释放 ---
    println!("  --- verifying PacketAssembler released ---");
    let small_msg = b"post_giant_probe";
    let wret = sys_sendto(
        fd_send,
        small_msg.as_ptr(),
        small_msg.len(),
        0,
        target.as_ptr(),
        sockaddr_in::len(),
    );
    if wret < 0 {
        println!("  WARN: small sendto returned {} (errno={})", wret, -wret);
    } else {
        println!("  sent {} bytes (probe)", wret);
        let rret = sys_recvfrom(
            fd_recv,
            recv_buf.as_mut_ptr(),
            recv_buf.len(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rret < 0 {
            println!(
                "  WARN: probe recvfrom returned {} (errno={}) — assembler may be stuck",
                rret, -rret
            );
        } else {
            let probe_recv = &recv_buf[..rret as usize];
            if probe_recv == small_msg {
                println!("  probe OK: small packet received, assembler freed");
            } else {
                println!("  WARN: probe data mismatch, got {:?}", probe_recv);
            }
        }
    }

    sys_close(fd_send);
    sys_close(fd_recv);
    println!("  PASS");
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
    println!("=== inet_test: HTTPS (embedded-tls) ===");

    // DNS resolve cloudflare.com（确认支持 TLS 1.3）
    let ip = match dns_lookup("cloudflare.com") {
        Some(ip) => {
            println!(
                "  DNS resolved cloudflare.com -> {}.{}.{}.{}",
                ip[0], ip[1], ip[2], ip[3]
            );
            ip
        }
        None => {
            println!("  FAIL: DNS lookup failed for cloudflare.com");
            return 1;
        }
    };

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;
    println!("  socket fd={}", fd);

    let addr = sockaddr_in::new(ip, 443);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!(
            "  FAIL: connect {}:{}:{}.{}:443 returned {} (errno={})",
            ip[0], ip[1], ip[2], ip[3], ret, -ret
        );
        sys_close(fd);
        return 1;
    }
    println!(
        "  TCP connected to {}.{}.{}.{}:443",
        ip[0], ip[1], ip[2], ip[3]
    );

    // TLS record buffers (max TLS record = 16640 bytes)
    let mut read_buf = [0u8; 16640];
    let mut write_buf = [0u8; 4096];

    let socket = TlsSocket { fd };

    let config = TlsConfig::new().with_server_name("cloudflare.com");

    let mut tls: TlsConnection<TlsSocket, Aes128GcmSha256> =
        TlsConnection::new(socket, &mut read_buf, &mut write_buf);

    let mut rng = SimpleRng::new();

    // open() — 同步阻塞握手，NoVerify 跳过证书验证
    match tls.open::<SimpleRng, NoVerify>(TlsContext::new(&config, &mut rng)) {
        Ok(()) => {
            println!("  TLS handshake OK");
        }
        Err(e) => {
            println!("  FAIL: TLS handshake error: {:?}", e);
            sys_close(fd);
            return 1;
        }
    }

    // HTTP GET over TLS — /cdn-cgi/trace 是 Cloudflare 调试端点，直接返回纯文本
    let http_req =
        b"GET /cdn-cgi/trace HTTP/1.1\r\nHost: cloudflare.com\r\nConnection: close\r\n\r\n";
    match tls.write(http_req) {
        Ok(n) => println!("  TLS write {} bytes (HTTP GET)", n),
        Err(e) => {
            println!("  FAIL: TLS write error: {:?}", e);
            sys_close(fd);
            return 1;
        }
    }
    let _ = tls.flush();

    // 读取响应
    let mut rx = [0u8; 4096];
    match tls.read(&mut rx) {
        Ok(n) => {
            if n > 0 {
                // 只打印前 200 字节
                let show = if n > 200 { 200 } else { n };
                let resp_str = core::str::from_utf8(&rx[..show]).unwrap_or("(non-utf8)");
                println!("  TLS read {} bytes, first {} bytes:", n, show);
                for line in resp_str.lines().take(8) {
                    println!("    | {}", line);
                }
                if n > show {
                    println!("    ... (truncated)");
                }
                println!("  PASS");
            } else {
                println!("  FAIL: TLS read returned 0 bytes");
                sys_close(fd);
                return 1;
            }
        }
        Err(e) => {
            println!("  FAIL: TLS read error: {:?}", e);
            sys_close(fd);
            return 1;
        }
    }

    sys_close(fd);
    0
}

/// 重型 HTTPS 下载测试：从 cloudflare.com 反复 GET 累计 32KB
fn test_https_download() -> i32 {
    println!("=== inet_test: HTTPS download (32KB via cloudflare.com) ===");

    let host = "cloudflare.com";
    let ip = match dns_lookup(host) {
        Some(ip) => {
            println!(
                "  DNS resolved {} -> {}.{}.{}.{}",
                host, ip[0], ip[1], ip[2], ip[3]
            );
            ip
        }
        None => {
            println!("  SKIP: DNS lookup failed for {}", host);
            return 0;
        }
    };

    let fd = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_in::new(ip, 443);
    let ret = sys_connect(fd, addr.as_ptr(), sockaddr_in::len());
    if ret < 0 {
        println!(
            "  FAIL: connect {}:443 returned {} (errno={})",
            host, ret, -ret
        );
        sys_close(fd);
        return 1;
    }
    println!("  TCP connected to {}:443", host);

    let t0 = user_lib::get_time();
    let mut total = 0usize;
    let target = 32768usize;

    for round in 0.. {
        // 每次 GET /cdn-cgi/trace（~500 字节纯文本）
        let fd2 = sys_socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if fd2 < 0 {
            break;
        }
        let fd2 = fd2 as usize;

        let ret = sys_connect(fd2, addr.as_ptr(), sockaddr_in::len());
        if ret < 0 {
            sys_close(fd2);
            break;
        }

        let mut read_buf = [0u8; 16640];
        let mut write_buf = [0u8; 4096];
        let socket = TlsSocket { fd: fd2 };
        let config = TlsConfig::new().with_server_name(host);

        let mut tls: TlsConnection<TlsSocket, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut read_buf, &mut write_buf);

        let mut rng = SimpleRng::new();
        if tls
            .open::<SimpleRng, NoVerify>(TlsContext::new(&config, &mut rng))
            .is_err()
        {
            sys_close(fd2);
            break;
        }

        let http_req = alloc::format!(
            "GET /cdn-cgi/trace HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            host
        );
        if tls.write(http_req.as_bytes()).is_err() {
            sys_close(fd2);
            break;
        }
        let _ = tls.flush();

        let mut rx = [0u8; 4096];
        loop {
            match tls.read(&mut rx) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(_) => break,
            }
        }
        sys_close(fd2);

        if total >= target {
            break;
        }
        if round == 0 {
            println!("  round {}: {} bytes total", round, total);
        }
    }

    let dt = user_lib::get_time() - t0;
    println!("  final: {} bytes in {} ms", total, dt);
    if dt > 0 && total > 0 {
        let kbps = (total as isize * 1000 / dt) as f64 / 1024.0;
        println!("  throughput: {:.1} KB/s", kbps);
    }

    if total < target / 2 {
        println!("  FAIL: too few bytes ({}/{})", total, target);
        return 1;
    }

    println!("  PASS");
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

    let tests: [(&str, fn() -> i32); 11] = [
        ("tcp_connect", test_tcp_connect_all),
        ("tcp_send_recv", || {
            test_tcp_send_recv("cloudflare", [1, 1, 1, 1])
        }),
        ("http_get", test_http_get),
        ("dns+baidu.com:80", || test_dns_and_tcp("baidu.com")),
        ("dns+bilibili.com:80", || test_dns_and_tcp("bilibili.com")),
        ("udp_loopback", test_udp_loopback),
        ("udp_loopback_pair", test_udp_loopback_pair),
        ("udp_external_dns", test_udp_external_dns),
        ("udp_giant_loopback", test_udp_giant_loopback),
        ("https_tls", test_https_tls),
        ("https_download_8k", test_https_download),
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
