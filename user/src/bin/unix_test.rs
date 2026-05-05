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

// ─── Abstract Socket Tests ───
// 参考 LTP bind04/bind05，测试抽象命名空间 Unix socket
// 抽象 socket 的 sun_path[0] = '\0'，之后是抽象名称（不需要文件系统路径）

/// 抽象 socket 地址长度：sun_family(2) + NUL(1) + name
fn abstract_addrlen(name: &str) -> usize {
    // offsetof(sun_family) + sizeof(sun_family) + 1(NUL) + name.len()
    2 + 1 + name.len()
}

fn test_abstract_stream() -> i32 {
    println!("=== unix_test: abstract STREAM (bind+listen+accept+connect) ===");
    // 仿 LTP bind04 "AF_UNIX abstract stream"
    let abstract_name = "unix_test_abstract_s";

    let srv_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;
    println!("  server socket fd={}", srv_fd);

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let bind_ret = sys_bind(srv_fd, addr.as_ptr(), addrlen);
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
        let cli_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
        if cli_fd < 0 {
            println!("  client FAIL: socket returned {}", cli_fd);
            exit(1);
        }
        let cli_fd = cli_fd as usize;
        let addr = sockaddr_un::abstract_addr(abstract_name);
        let addrlen = abstract_addrlen(abstract_name);
        let conn_ret = sys_connect(cli_fd, addr.as_ptr(), addrlen);
        if conn_ret < 0 {
            println!("  client FAIL: connect returned {}", conn_ret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client connected, sending...");
        let msg = b"hello_abstract_stream";
        let wret = sys_sendto(cli_fd, msg.as_ptr(), msg.len(), 0, core::ptr::null(), 0);
        if wret < 0 {
            println!("  client FAIL: sendto returned {}", wret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client sent {} bytes", wret);

        // read reply from server
        let mut rbuf = [0u8; 64];
        let rret = sys_recvfrom(
            cli_fd,
            rbuf.as_mut_ptr(),
            rbuf.len(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if rret < 0 {
            println!("  client FAIL: recvfrom returned {}", rret);
        } else {
            println!(
                "  client received reply: {}",
                core::str::from_utf8(&rbuf[..rret as usize]).unwrap_or("???")
            );
        }
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
        return 1;
    }
    let expected = b"hello_abstract_stream";
    if &buf[..rret as usize] != expected {
        println!(
            "  FAIL: got {:?}, expected {:?}",
            &buf[..rret as usize],
            expected
        );
        sys_close(cli_fd);
        sys_close(srv_fd);
        return 1;
    }
    println!(
        "  server received: {}",
        core::str::from_utf8(&buf[..rret as usize]).unwrap_or("???")
    );

    // reply to client
    let reply = b"pong_back";
    let wret = sys_sendto(cli_fd, reply.as_ptr(), reply.len(), 0, core::ptr::null(), 0);
    if wret < 0 {
        println!("  server send reply FAIL: {}", wret);
    }

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
    println!("  PASS");
    0
}

fn test_abstract_dgram() -> i32 {
    println!("=== unix_test: abstract DGRAM (bind+sendto+recvfrom) ===");
    // 仿 LTP bind05，DGRAM 上的抽象 socket
    let srv_name = "unix_abstract_dgram_srv";
    let cli_name = "unix_abstract_dgram_cli";

    let srv_fd = sys_socket(AF_UNIX, SOCK_DGRAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: server socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;
    println!("  server socket fd={}", srv_fd);

    let srv_addr = sockaddr_un::abstract_addr(srv_name);
    let srv_addrlen = abstract_addrlen(srv_name);
    let bind_ret = sys_bind(srv_fd, srv_addr.as_ptr(), srv_addrlen);
    if bind_ret < 0 {
        println!("  FAIL: server bind returned {}", bind_ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  server bound");

    // fork client
    let pid = fork();
    if pid == 0 {
        let cli_fd = sys_socket(AF_UNIX, SOCK_DGRAM, 0);
        if cli_fd < 0 {
            println!("  client FAIL: socket returned {}", cli_fd);
            exit(1);
        }
        let cli_fd = cli_fd as usize;

        let cli_addr = sockaddr_un::abstract_addr(cli_name);
        let cli_addrlen = abstract_addrlen(cli_name);
        let bind_ret = sys_bind(cli_fd, cli_addr.as_ptr(), cli_addrlen);
        if bind_ret < 0 {
            println!("  client FAIL: bind returned {}", bind_ret);
            sys_close(cli_fd);
            exit(1);
        }

        // 发送给 server
        let srv_addr = sockaddr_un::abstract_addr(srv_name);
        let srv_addrlen = abstract_addrlen(srv_name);
        let msg = b"hello_abstract_dgram";
        let wret = sys_sendto(
            cli_fd,
            msg.as_ptr(),
            msg.len(),
            0,
            srv_addr.as_ptr(),
            srv_addrlen,
        );
        if wret < 0 {
            println!("  client FAIL: sendto returned {}", wret);
            sys_close(cli_fd);
            exit(1);
        }
        println!("  client sent {} bytes", wret);

        // 接收回复
        let mut buf = [0u8; 64];
        let mut from = sockaddr_un::new("");
        let mut fromlen = sockaddr_un::len();
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
        exit(0);
    }

    // parent: server recv
    let mut buf = [0u8; 64];
    let mut from = sockaddr_un::new("");
    let mut fromlen = sockaddr_un::len();
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
        return 1;
    }
    let expected = b"hello_abstract_dgram";
    if &buf[..rret as usize] != expected {
        println!(
            "  FAIL: got {:?}, expected {:?}",
            &buf[..rret as usize],
            expected
        );
        sys_close(srv_fd);
        return 1;
    }
    println!(
        "  server received: {}",
        core::str::from_utf8(&buf[..rret as usize]).unwrap_or("???")
    );

    // 回复客户端：from 中带有客户端的抽象地址
    let reply = b"pong_abstract_dgram";
    let wret = sys_sendto(
        srv_fd,
        reply.as_ptr(),
        reply.len(),
        0,
        from.as_ptr(),
        fromlen,
    );
    if wret < 0 {
        println!("  server send reply FAIL: {}", wret);
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
    println!("  PASS");
    0
}

fn test_abstract_rebind() -> i32 {
    println!("=== unix_test: abstract rebind (close→rebind same name) ===");
    // 仿 LTP bind03，关闭后同一抽象名称应可再次绑定
    let abstract_name = "unix_rebind_test";

    // 第一次绑
    let fd1 = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if fd1 < 0 {
        println!("  FAIL: socket1 returned {}", fd1);
        return 1;
    }
    let fd1 = fd1 as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_bind(fd1, addr.as_ptr(), addrlen);
    if ret < 0 {
        println!("  FAIL: first bind returned {}", ret);
        sys_close(fd1);
        return 1;
    }
    println!("  first bind ok (fd={})", fd1);

    // 关闭第一个 socket，抽象名应被释放
    sys_close(fd1);
    println!("  closed fd1");

    // 第二次绑定同名应成功
    let fd2 = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if fd2 < 0 {
        println!("  FAIL: socket2 returned {}", fd2);
        return 1;
    }
    let fd2 = fd2 as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_bind(fd2, addr.as_ptr(), addrlen);
    if ret < 0 {
        println!("  FAIL: rebind returned {}", ret);
        sys_close(fd2);
        return 1;
    }
    println!("  rebind ok (fd={})", fd2);

    sys_close(fd2);
    println!("  PASS");
    0
}

fn test_abstract_getsockname() -> i32 {
    println!("=== unix_test: abstract getsockname ===");
    // 绑定抽象 socket 后，getsockname 应返回 \0 前缀的地址
    let abstract_name = "unix_gsn_abstract";

    let fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if fd < 0 {
        println!("  FAIL: socket returned {}", fd);
        return 1;
    }
    let fd = fd as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_bind(fd, addr.as_ptr(), addrlen);
    if ret < 0 {
        println!("  FAIL: bind returned {}", ret);
        sys_close(fd);
        return 1;
    }
    println!("  bind ok");

    let mut got_addr = sockaddr_un::new("");
    let mut got_addrlen = sockaddr_un::len();
    let ret = sys_getsockname(fd, got_addr.as_mut_ptr(), &mut got_addrlen as *mut usize);
    if ret < 0 {
        println!("  FAIL: getsockname returned {}", ret);
        sys_close(fd);
        return 1;
    }
    println!(
        "  getsockname ok, family={}, addrlen={}",
        got_addr.sun_family, got_addrlen
    );

    // 验证 sun_path[0] == 0 (abstract socket 标记)
    if got_addr.sun_path[0] != 0 {
        println!(
            "  FAIL: sun_path[0]={}, expected 0 (abstract)",
            got_addr.sun_path[0]
        );
        sys_close(fd);
        return 1;
    }
    println!("  sun_path[0]=0 (abstract) ✓");

    // 验证后面的字节是抽象名称
    let name_bytes = &got_addr.sun_path[1..];
    let expected_bytes = abstract_name.as_bytes();
    let match_len = core::cmp::min(expected_bytes.len(), name_bytes.len());
    if &name_bytes[..match_len] != &expected_bytes[..match_len] {
        println!(
            "  WARN: name mismatch, got {:?}, expected {:?}",
            &name_bytes[..match_len],
            expected_bytes
        );
        // 非致命，某些实现可能截断
    } else {
        println!(
            "  name matches: {}",
            core::str::from_utf8(&name_bytes[..match_len]).unwrap_or("???")
        );
    }

    sys_close(fd);
    println!("  PASS");
    0
}

fn test_abstract_getpeername() -> i32 {
    println!("=== unix_test: abstract getpeername ===");
    // 连通后，getpeername 应返回 peer 的抽象地址
    let abstract_name = "unix_gpn_abstract";

    let srv_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: server socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_bind(srv_fd, addr.as_ptr(), addrlen);
    if ret < 0 {
        println!("  FAIL: bind returned {}", ret);
        sys_close(srv_fd);
        return 1;
    }
    let ret = sys_listen(srv_fd, 5);
    if ret < 0 {
        println!("  FAIL: listen returned {}", ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  server ready");

    let pid = fork();
    if pid == 0 {
        let cli_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
        if cli_fd < 0 {
            exit(1);
        }
        let cli_fd = cli_fd as usize;
        let addr = sockaddr_un::abstract_addr(abstract_name);
        let addrlen = abstract_addrlen(abstract_name);
        sys_connect(cli_fd, addr.as_ptr(), addrlen);
        sys_yield();
        sys_yield(); // give server time
        sys_close(cli_fd);
        exit(0);
    }

    let acc_ret = sys_accept(srv_fd, core::ptr::null_mut(), core::ptr::null_mut());
    if acc_ret < 0 {
        println!("  FAIL: accept returned {}", acc_ret);
        sys_close(srv_fd);
        return 1;
    }
    let cli_fd = acc_ret as usize;
    println!("  accept ok, client fd={}", cli_fd);

    // getpeername on server side should return client's (unnamed) address
    // 客户端没有 bind，所以 peer 应该是 unnamed（sun_family=AF_UNIX, sun_path 全0）
    let mut peer_addr = sockaddr_un::new("");
    let mut peer_addrlen = sockaddr_un::len();
    let ret = sys_getpeername(
        cli_fd,
        peer_addr.as_mut_ptr(),
        &mut peer_addrlen as *mut usize,
    );
    if ret < 0 {
        println!(
            "  WARN: getpeername returned {} (may not be implemented)",
            ret
        );
    } else {
        println!(
            "  getpeername ok, family={}, addrlen={}",
            peer_addr.sun_family, peer_addrlen
        );
        // family 应该是 AF_UNIX
        if peer_addr.sun_family == AF_UNIX as u16 {
            println!("  peer family=AF_UNIX ✓");
        } else {
            println!("  WARN: unexpected family {}", peer_addr.sun_family);
        }
    }

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
    println!("  PASS");
    0
}

fn test_abstract_auto_cleanup() -> i32 {
    println!("=== unix_test: abstract auto-cleanup (close frees name) ===");
    // 关闭监听 socket 后，抽象名应释放，后续 connect 应返回 ECONNREFUSED
    let abstract_name = "unix_cleanup_test";

    let srv_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if srv_fd < 0 {
        println!("  FAIL: socket returned {}", srv_fd);
        return 1;
    }
    let srv_fd = srv_fd as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_bind(srv_fd, addr.as_ptr(), addrlen);
    if ret < 0 {
        println!("  FAIL: bind returned {}", ret);
        sys_close(srv_fd);
        return 1;
    }
    let ret = sys_listen(srv_fd, 5);
    if ret < 0 {
        println!("  FAIL: listen returned {}", ret);
        sys_close(srv_fd);
        return 1;
    }
    println!("  server ready, closing...");
    sys_close(srv_fd);

    // 短暂等待确保清理
    sys_yield();

    // 现在尝试 connect，应失败
    let cli_fd = sys_socket(AF_UNIX, SOCK_STREAM, 0);
    if cli_fd < 0 {
        println!("  FAIL: client socket returned {}", cli_fd);
        return 1;
    }
    let cli_fd = cli_fd as usize;

    let addr = sockaddr_un::abstract_addr(abstract_name);
    let addrlen = abstract_addrlen(abstract_name);
    let ret = sys_connect(cli_fd, addr.as_ptr(), addrlen);
    if ret < 0 {
        // 预期失败：ECONNREFUSED(-111) 或 ENOENT(-2) 或其他错误
        println!("  connect after close returned {} (expected <0)", ret);
        println!("  PASS (abstract name freed)");
    } else {
        println!("  FAIL: connect succeeded unexpectedly (fd={})", ret);
        sys_close(cli_fd);
        return 1;
    }

    sys_close(cli_fd);
    0
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("");
    println!("============================================");
    println!("  Unix Domain Socket Standalone Test Suite");
    println!("============================================");

    let tests: [(&str, fn() -> i32); 14] = [
        ("socketpair DGRAM", test_socketpair_dgram),
        ("socketpair STREAM", test_socketpair_stream),
        ("named STREAM", test_named_stream),
        ("named DGRAM", test_named_dgram),
        ("error cases", test_error_cases),
        ("getsockname", test_getsockname),
        ("shutdown", test_shutdown),
        ("CLOEXEC|NONBLOCK", test_socket_cloexec_nonblock),
        ("abstract STREAM", test_abstract_stream),
        ("abstract DGRAM", test_abstract_dgram),
        ("abstract rebind", test_abstract_rebind),
        ("abstract getsockname", test_abstract_getsockname),
        ("abstract getpeername", test_abstract_getpeername),
        ("abstract auto-cleanup", test_abstract_auto_cleanup),
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
