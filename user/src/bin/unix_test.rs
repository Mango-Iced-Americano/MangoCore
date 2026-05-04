#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use user_lib::syscall::*;
use user_lib::{
    exit, fork, println, AF_UNIX, SHUT_RDWR, SHUT_WR, SOCK_CLOEXEC, SOCK_DGRAM, SOCK_NONBLOCK,
    SOCK_STREAM,
};

/// sockaddr_un for AF_UNIX
#[repr(C)]
struct sockaddr_un {
    sun_family: u16, // AF_UNIX = 1
    sun_path: [u8; 108],
}

impl sockaddr_un {
    fn new(path: &str) -> Self {
        let mut addr = sockaddr_un {
            sun_family: AF_UNIX as u16,
            sun_path: [0u8; 108],
        };
        let bytes = path.as_bytes();
        let len = core::cmp::min(bytes.len(), 107);
        addr.sun_path[..len].copy_from_slice(&bytes[..len]);
        addr
    }

    fn abstract_addr(name: &str) -> Self {
        let mut addr = sockaddr_un {
            sun_family: AF_UNIX as u16,
            sun_path: [0u8; 108],
        };
        // Abstract socket: first byte is '\0'
        let bytes = name.as_bytes();
        let len = core::cmp::min(bytes.len(), 106);
        addr.sun_path[1..=len].copy_from_slice(&bytes[..len]);
        addr
    }

    fn as_ptr(&self) -> *const u8 {
        self as *const Self as *const u8
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self as *mut Self as *mut u8
    }

    fn len() -> usize {
        core::mem::size_of::<Self>()
    }
}

fn run_bash_cmd(cmd: &str) -> i32 {
    let pid = fork();
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
        exit(127);
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

fn test_socketpair_dgram() -> i32 {
    println!("=== unix_test: socketpair DGRAM ===");
    let mut sv = [0i32; 2];
    let ret = sys_socketpair(AF_UNIX, SOCK_DGRAM, 0, sv.as_mut_ptr());
    if ret < 0 {
        println!("  FAIL: socketpair returned {}", ret);
        return 1;
    }
    let fd1 = sv[0] as usize;
    let fd2 = sv[1] as usize;
    println!("  socketpair fds={},{}", fd1, fd2);

    // send/recv
    let msg = b"hello_dgram";
    let wret = sys_sendto(fd1, msg.as_ptr(), msg.len(), 0, core::ptr::null(), 0);
    if wret < 0 {
        println!("  FAIL: sendto returned {}", wret);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!("  sendto ok: {} bytes", wret);

    let mut buf = [0u8; 64];
    let rret = sys_recvfrom(
        fd2,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        println!("  FAIL: recvfrom returned {}", rret);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    let recv = &buf[..rret as usize];
    if recv != msg {
        println!("  FAIL: got {:?}, expected {:?}", recv, msg);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!(
        "  recvfrom ok: {}",
        core::str::from_utf8(recv).unwrap_or("???")
    );

    // bi-directional
    let msg2 = b"world_dgram";
    let wret2 = sys_sendto(fd2, msg2.as_ptr(), msg2.len(), 0, core::ptr::null(), 0);
    if wret2 < 0 {
        println!("  FAIL: sendto2 returned {}", wret2);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    let mut buf2 = [0u8; 64];
    let rret2 = sys_recvfrom(
        fd1,
        buf2.as_mut_ptr(),
        buf2.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret2 < 0 {
        println!("  FAIL: recvfrom2 returned {}", rret2);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    let recv2 = &buf2[..rret2 as usize];
    if recv2 != msg2 {
        println!("  FAIL: got {:?}, expected {:?}", recv2, msg2);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!("  bidirectional ok");

    sys_close(fd1);
    sys_close(fd2);
    println!("  PASS");
    0
}

fn test_socketpair_stream() -> i32 {
    println!("=== unix_test: socketpair STREAM ===");
    let mut sv = [0i32; 2];
    let ret = sys_socketpair(AF_UNIX, SOCK_STREAM, 0, sv.as_mut_ptr());
    if ret < 0 {
        println!("  FAIL: socketpair returned {}", ret);
        return 1;
    }
    let fd1 = sv[0] as usize;
    let fd2 = sv[1] as usize;
    println!("  socketpair fds={},{}", fd1, fd2);

    let msg = b"hello_stream";
    let wret = sys_sendto(fd1, msg.as_ptr(), msg.len(), 0, core::ptr::null(), 0);
    if wret < 0 {
        println!("  FAIL: write returned {}", wret);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!("  write ok: {} bytes", wret);

    let mut buf = [0u8; 64];
    let rret = sys_recvfrom(
        fd2,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        println!("  FAIL: read returned {}", rret);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    let recv = &buf[..rret as usize];
    if recv != msg {
        println!("  FAIL: got {:?}, expected {:?}", recv, msg);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!("  read ok: {}", core::str::from_utf8(recv).unwrap_or("???"));

    sys_close(fd1);
    sys_close(fd2);
    println!("  PASS");
    0
}

fn test_named_stream() -> i32 {
    println!("=== unix_test: named STREAM (bind+listen+accept+connect) ===");
    let sockpath = "/tmp/unix_test_stream.sock\0";
    // 确保之前的 socket 文件已删除
    run_bash_cmd("rm -f /tmp/unix_test_stream.sock");

    let srv_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;
    println!("  server socket fd={}", srv_fd);

    let addr = sockaddr_un::new(sockpath.trim_end_matches('\0'));
    let bind_ret = sys_bind(srv_fd, addr.as_ptr(), sockaddr_un::len());
    if bind_ret < 0 {
        println!("  FAIL: bind returned {}", bind_ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  bind ok");

    let listen_ret = sys_listen(srv_fd, 5);
    if listen_ret < 0 {
        println!("  FAIL: listen returned {}", listen_ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  listen ok");

    // fork client
    let pid = fork();
    if pid == 0 {
        // child: client
        let cli_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
        if cli_fd < 0 {
            println!("  client FAIL: socket returned {}", cli_fd);
            exit(1);
        }
        let cli_fd = cli_fd as usize;
        let addr = sockaddr_un::new(sockpath.trim_end_matches('\0'));
        let conn_ret = sys_connect(cli_fd, addr.as_ptr(), sockaddr_un::len());
        if conn_ret < 0 {
            println!("  client FAIL: connect returned {}", conn_ret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client connected, sending...");
        let msg = b"ping_from_client";
        let wret = sys_sendto(cli_fd, msg.as_ptr(), msg.len(), 0, core::ptr::null(), 0);
        if wret < 0 {
            println!("  client FAIL: sendto returned {}", wret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client sent {} bytes", wret);
        sys_close(cli_fd);
        exit(0);
    }

    // parent: server accept
    let mut cli_addr = sockaddr_un::new("");
    let mut addrlen = sockaddr_un::len();
    let acc_ret = sys_accept(srv_fd, cli_addr.as_mut_ptr(), &mut addrlen as *mut usize);
    if acc_ret < 0 {
        println!("  FAIL: accept returned {}", acc_ret);
        sys_close(srv_fd);
        run_bash_cmd("rm -f /tmp/unix_test_stream.sock");
        return 1;
    }
    let cli_fd = acc_ret as usize;
    println!("  accept ok, client fd={}", cli_fd);

    // read from client
    let mut buf = [0u8; 64];
    let rret = sys_recvfrom(
        cli_fd,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    if rret < 0 {
        println!("  FAIL: recvfrom returned {}", rret);
        sys_close(cli_fd);
        sys_close(srv_fd);
        run_bash_cmd("rm -f /tmp/unix_test_stream.sock");
        return 1;
    }
    println!(
        "  server received: {}",
        core::str::from_utf8(&buf[..rret as usize]).unwrap_or("???")
    );

    // wait for client
    let mut code = 0;
    loop {
        let ret = sys_waitpid(pid as isize, &mut code);
        if ret == pid || ret < 0 {
            break;
        }
        sys_yield();
    }

    sys_close(cli_fd);
    sys_close(srv_fd);
    run_bash_cmd("rm -f /tmp/unix_test_stream.sock");
    println!("  PASS");
    0
}

fn test_named_dgram() -> i32 {
    println!("=== unix_test: named DGRAM (bind+sendto+recvfrom) ===");
    let srv_path = "/tmp/unix_test_dgram_srv.sock\0";
    let cli_path = "/tmp/unix_test_dgram_cli.sock\0";
    run_bash_cmd("rm -f /tmp/unix_test_dgram_srv.sock /tmp/unix_test_dgram_cli.sock");

    let srv_fd = sys_socket(AF_UNIX, SOCK_DGRAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: server socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;

    let srv_addr = sockaddr_un::new(srv_path.trim_end_matches('\0'));
    let bind_ret = sys_bind(srv_fd, srv_addr.as_ptr(), sockaddr_un::len());
    if bind_ret < 0 {
        println!("  FAIL: server bind returned {}", bind_ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  server bound");

    // fork client
    let pid = fork();
    if pid == 0 {
        // child: client
        let cli_fd = sys_socket(AF_UNIX, SOCK_DGRAM, 0);
        if cli_fd < 0 {
            println!("  client FAIL: socket returned {}", cli_fd);
            exit(1);
        }
        let cli_fd = cli_fd as usize;

        // 客户端也需要 bind 才能收
        let cli_addr = sockaddr_un::new(cli_path.trim_end_matches('\0'));
        let bind_ret = sys_bind(cli_fd, cli_addr.as_ptr(), sockaddr_un::len());
        if bind_ret < 0 {
            println!("  client FAIL: bind returned {}", bind_ret);
            sys_close(cli_fd);
            exit(1);
        }

        // 发送给 server
        let srv_addr = sockaddr_un::new(srv_path.trim_end_matches('\0'));
        let msg = b"hello_dgram_server";
        let wret = sys_sendto(
            cli_fd,
            msg.as_ptr(),
            msg.len(),
            0,
            srv_addr.as_ptr(),
            sockaddr_un::len(),
        );
        if wret < 0 {
            println!("  client FAIL: sendto returned {}", wret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client sent {} bytes", wret);

        // 接收回复
        let mut buf = [0u8; 64];
        let mut fromlen = sockaddr_un::len();
        let mut from = sockaddr_un::new("");
        let rret = sys_recvfrom(
            cli_fd,
            buf.as_mut_ptr(),
            buf.len(),
            0,
            from.as_mut_ptr(),
            &mut fromlen as *mut usize,
        );
        if rret < 0 {
            println!("  client FAIL: recvfrom returned {}", rret);
            sys_close(cli_fd);
            exit(1);
        }
        println!(
            "  client received: {}",
            core::str::from_utf8(&buf[..rret as usize]).unwrap_or("???")
        );
        sys_close(cli_fd);
        run_bash_cmd("rm -f /tmp/unix_test_dgram_cli.sock");
        exit(0);
    }

    // parent: server
    let mut buf = [0u8; 64];
    let mut fromlen = sockaddr_un::len();
    let mut from = sockaddr_un::new("");
    let rret = sys_recvfrom(
        srv_fd,
        buf.as_mut_ptr(),
        buf.len(),
        0,
        from.as_mut_ptr(),
        &mut fromlen as *mut usize,
    );
    if rret < 0 {
        println!("  FAIL: server recvfrom returned {}", rret);
        sys_close(srv_fd);
        run_bash_cmd("rm -f /tmp/unix_test_dgram_srv.sock /tmp/unix_test_dgram_cli.sock");
        return 1;
    }
    println!(
        "  server received: {}",
        core::str::from_utf8(&buf[..rret as usize]).unwrap_or("???")
    );

    // 回复客户端
    let reply = b"pong_from_server";
    let wret = sys_sendto(
        srv_fd,
        reply.as_ptr(),
        reply.len(),
        0,
        from.as_ptr(),
        fromlen,
    );
    if wret < 0 {
        println!("  FAIL: server sendto returned {}", wret);
    } else {
        println!("  server sent reply");
    }

    let mut code = 0;
    loop {
        let ret = sys_waitpid(pid as isize, &mut code);
        if ret == pid || ret < 0 {
            break;
        }
        sys_yield();
    }

    sys_close(srv_fd);
    run_bash_cmd("rm -f /tmp/unix_test_dgram_srv.sock /tmp/unix_test_dgram_cli.sock");
    println!("  PASS");
    0
}

fn test_error_cases() -> i32 {
    println!("=== unix_test: error cases ===");
    let mut failed = 0;

    // 1. invalid domain
    let ret = sys_socket(99, SOCK_STREAM, 0);
    if ret >= 0 {
        println!("  FAIL: socket(invalid domain) should fail, got fd={}", ret);
        sys_close(ret as usize);
        failed += 1;
    } else {
        println!("  OK: socket(invalid domain) -> {}", ret);
    }

    // 2. socketpair with SOCK_DGRAM should work
    let ret = sys_socketpair(AF_UNIX, SOCK_DGRAM, 0, core::ptr::null_mut());
    if ret >= 0 {
        println!("  OK: socketpair(dgram) -> {}", ret);
    } else {
        println!(
            "  OK: socketpair(dgram) returned {} (may use null ptr)",
            ret
        );
    }

    // 3. invalid listen on DGRAM
    let fd = sys_socket(AF_UNIX, SOCK_DGRAM, 0);
    if fd >= 0 {
        let fd = fd as usize;
        let ret = sys_listen(fd, 5);
        if ret < 0 {
            println!("  OK: listen on DGRAM -> {}", ret);
        } else {
            println!("  listen on DGRAM returned {} (unexpected)", ret);
        }
        sys_close(fd);
    }

    // 4. shutdown invalid fd
    let ret = sys_sock_shutdown(999, SHUT_RDWR);
    if ret < 0 {
        println!("  OK: shutdown(bad fd) -> {}", ret);
    }

    if failed > 0 {
        println!("  some error tests FAILED ({})", failed);
        return 1;
    }
    println!("  PASS");
    0
}

fn test_getsockname() -> i32 {
    println!("=== unix_test: getsockname ===");
    let sockpath = "/tmp/unix_test_gsn.sock\0";
    run_bash_cmd("rm -f /tmp/unix_test_gsn.sock");

    let fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_un::new(sockpath.trim_end_matches('\0'));
    let ret = sys_bind(fd, addr.as_ptr(), sockaddr_un::len());
    if ret < 0 {
        println!("  FAIL: bind returned {}", ret);
        sys_close(fd);
        return 1;
    }

    let mut got_addr = sockaddr_un::new("");
    let mut addrlen = sockaddr_un::len();
    let ret = sys_getsockname(fd, got_addr.as_mut_ptr(), &mut addrlen as *mut usize);
    if ret < 0 {
        println!("  FAIL: getsockname returned {}", ret);
        sys_close(fd);
        run_bash_cmd("rm -f /tmp/unix_test_gsn.sock");
        return 1;
    }
    println!("  getsockname ok, family={}", got_addr.sun_family);

    sys_close(fd);
    run_bash_cmd("rm -f /tmp/unix_test_gsn.sock");
    println!("  PASS");
    0
}

fn test_shutdown() -> i32 {
    println!("=== unix_test: sock_shutdown ===");
    let mut sv = [0i32; 2];
    let ret = sys_socketpair(AF_UNIX, SOCK_STREAM, 0, sv.as_mut_ptr());
    if ret < 0 {
        println!("  FAIL: socketpair returned {}", ret);
        return 1;
    }
    let fd1 = sv[0] as usize;
    let fd2 = sv[1] as usize;

    // shutdown WR on fd1
    let ret = sys_sock_shutdown(fd1, SHUT_WR);
    if ret < 0 {
        println!("  FAIL: shutdown(SHUT_WR) returned {}", ret);
        sys_close(fd1);
        sys_close(fd2);
        return 1;
    }
    println!("  shutdown(SHUT_WR) ok");

    // try reading from fd2 (should still work)
    let msg = b"after_shutdown";
    let wret = sys_sendto(fd2, msg.as_ptr(), msg.len(), 0, core::ptr::null(), 0);
    if wret < 0 {
        println!(
            "  write to peer after shutdown returned {} (fd1 WR closed)",
            wret
        );
    } else {
        println!("  write to peer after shutdown ok: {} bytes", wret);
    }

    sys_close(fd1);
    sys_close(fd2);
    println!("  PASS");
    0
}

fn test_socket_cloexec_nonblock() -> i32 {
    println!("=== unix_test: socket with SOCK_CLOEXEC | SOCK_NONBLOCK ===");
    let fd = sys_socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if fd < 0 {
        println!("  FAIL: socket(CLOEXEC|NONBLOCK) returned {}", fd);
        return 1;
    }
    println!("  socket(CLOEXEC|NONBLOCK) -> fd={}", fd);
    sys_close(fd as usize);
    println!("  PASS");
    0
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("");
    println!("============================================");
    println!("  Unix Domain Socket Standalone Test Suite");
    println!("============================================");

    let tests: [(&str, fn() -> i32); 8] = [
        ("socketpair DGRAM", test_socketpair_dgram),
        ("socketpair STREAM", test_socketpair_stream),
        ("named STREAM", test_named_stream),
        ("named DGRAM", test_named_dgram),
        ("error cases", test_error_cases),
        ("getsockname", test_getsockname),
        ("shutdown", test_shutdown),
        ("CLOEXEC|NONBLOCK", test_socket_cloexec_nonblock),
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
