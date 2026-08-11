extern crate alloc;
use alloc::format;
use crate::runner::config::{LtpLibc, LtpRunner, RuntimeConfig};
use crate::runner::groups::catalog::TEST_GROUPS;
use crate::runner::ltp::{inline::run_ltp_binaries, suite::run_ltp_suite_runner};
use user_lib::{chdir, chroot, close, exec, exit, fork, get_time, getpgid, kill, open, println, setpgid, sleep, waitpid, waitpid_wnohang, OpenFlags, SIGKILL};
pub fn run_group_in_dir(environ: &[*const u8], dir: &str, group: &str, script: &str, timeout: u64, diag: bool) {
    let suffix = if dir.contains("musl") { "musl" } else if group == "cpython" { "isolated" } else { "glibc" };
    println!("#### OS COMP TEST GROUP START {}-{} ####", group, suffix);
    let pid = fork(); if pid == 0 { let _ = setpgid(0, 0); if chdir(dir) < 0 { exit(126); } let shell = "/bin/sh\0"; let command = if diag { format!("echo 0 > /sys/kernel/stats/stats_on; echo memory_io > /sys/kernel/stats/profile; echo 1 > /sys/kernel/stats/reset; echo 1 > /sys/kernel/stats/stats_on; /bin/sh ./{}; status=$?; echo 0 > /sys/kernel/stats/stats_on; echo '[initproc] [diag] === stats {}-{} ==='; cat /sys/kernel/stats/blockio; echo '[initproc] [diag] === stats {}-{} end ==='; exit $status\0", script, group, suffix, group, suffix) } else { format!("./{}\0", script) }; let dash_c = "-c\0"; if diag { exec(shell, &[shell.as_ptr(), dash_c.as_ptr(), command.as_ptr(), core::ptr::null()], environ); } else { exec(shell, &[shell.as_ptr(), command.as_ptr(), core::ptr::null()], environ); } exit(127); }
    let start = get_time() as u64; let mut status = 0;
    while pid > 0 && waitpid_wnohang(pid, &mut status) == 0 { if (get_time() as u64).saturating_sub(start) >= timeout * 1000 { let pgid = getpgid(pid as usize); if pgid > 0 { let _ = kill(!(pgid as usize) + 1, SIGKILL); } let _ = kill(pid as usize, SIGKILL); let _ = waitpid(pid as usize, &mut status); break; } sleep(100); }
    println!("#### OS COMP TEST GROUP END {}-{} ####", group, suffix); println!("[initproc] done {} in {} exit_code={}", script, dir.trim_end_matches('\0'), status);
}
/// Run a group chrooted into the SD root: child binds /proc,/sys,/dev into
/// `chroot_root` first, then chroot + chdir(`work_dir`) + exec the script, so
/// the script's absolute SD-root paths resolve inside the Debian userland.
pub fn run_group_chrooted(environ: &[*const u8], chroot_root: &str, work_dir: &str, group: &str, script: &str, timeout: u64, diag: bool) {
    let suffix = "glibc";
    println!("#### OS COMP TEST GROUP START {}-{} ####", group, suffix);
    let pid = fork(); if pid == 0 { let _ = setpgid(0, 0);
        if !crate::runner::vf2_mounts::bind_pseudo_filesystems_in(chroot_root) {
            println!("[test-runner] {}: pseudo-fs bind into {} incomplete; continuing (script self-mounts /proc)", group, chroot_root.trim_end_matches('\0'));
        }
        if chdir("/\0") < 0 { println!("[test-runner] {}: chdir / before chroot failed", group); exit(126); }
        if chroot(chroot_root) < 0 { println!("[test-runner] {}: chroot {} failed", group, chroot_root.trim_end_matches('\0')); exit(126); }
        if chdir("/\0") < 0 { println!("[test-runner] {}: chdir / after chroot failed", group); exit(126); }
        if chdir(work_dir) < 0 { println!("[test-runner] {}: chdir {} failed", group, work_dir.trim_end_matches('\0')); exit(126); }
        println!("[test-runner] {}: chroot+chdir({}) done, cwd_marker A", group, work_dir.trim_end_matches('\0'));
        let fd = open("./buildstorm_testcode.sh\0", OpenFlags::RDONLY); println!("[test-runner] {}: open ./buildstorm_testcode.sh ret={}", group, fd); if fd >= 0 { let _ = close(fd as usize); }
        let shell = "/bin/bash\0"; let command = if diag { format!("echo 0 > /sys/kernel/stats/stats_on; echo memory_io > /sys/kernel/stats/profile; echo 1 > /sys/kernel/stats/reset; echo 1 > /sys/kernel/stats/stats_on; /bin/bash ./{}; status=$?; echo 0 > /sys/kernel/stats/stats_on; echo '[initproc] [diag] === stats {}-{} ==='; cat /sys/kernel/stats/blockio; echo '[initproc] [diag] === stats {}-{} end ==='; exit $status\0", script, group, suffix, group, suffix)         } else if group == "buildstorm" || group == "cagent" {
        format!("{}\0", r#####"echo "[buildstorm] STEP1 mount"
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28
export CARGO_NET_OFFLINE=true
case "$(uname -m 2>/dev/null)" in
  loongarch64) AXARCH=loongarch64; AXTGT=loongarch64-unknown-linux-musl ;;
  riscv64)     AXARCH=riscv64;     AXTGT=riscv64gc-unknown-linux-musl ;;
  *)           echo "BUILDSTORM_ARCH fail machine=$(uname -m 2>/dev/null)"; exit 1 ;;
esac
echo "[buildstorm] STEP2 toolchain-check"
if rustc --version && cargo --version; then
    echo "BUILDSTORM_TOOLCHAIN ok"
else
    echo "BUILDSTORM_TOOLCHAIN fail"
fi
rm -rf /tmp/minibuild
echo "[buildstorm] STEP3 minibuild-cargo-new"
cargo new --vcs none /tmp/minibuild
echo "[buildstorm] STEP4 minibuild-cargo-build"
echo "[buildstorm] BUILD_START $(cut -d' ' -f1 /proc/uptime)"
( cd /tmp/minibuild && cargo build )
echo "[buildstorm] BUILD_END $(cut -d' ' -f1 /proc/uptime)"
echo "[buildstorm] STEP5 minibuild-run"
if [ "$(/tmp/minibuild/target/debug/minibuild)" = "Hello, world!" ]; then
    echo "BUILDSTORM_MINIBUILD ok"
else
    echo "BUILDSTORM_MINIBUILD fail"
fi
echo "[buildstorm] STEP6 cd-tgoskits"
cd /work/tgoskits 2>/dev/null || {
    echo "BUILDSTORM_COMPILE mode=multi ok=false elapsed_s=0 cores=$(nproc) bytes=0 arch=$AXARCH"
    echo "#### OS COMP TEST GROUP END buildstorm ####"
    exit 1
}
if ! rm -rf "target/$AXTGT" || [ -e "target/$AXTGT" ] || [ -L "target/$AXTGT" ]; then
    echo "BUILDSTORM_PRECLEAN fail target=$AXTGT"
    exit 1
fi
echo "BUILDSTORM_PRECLEAN ok target=$AXTGT"
echo "[buildstorm] STEP7 prebuild-xtask"
echo "----- pre-build tg-xtask (untimed) -----"
cargo build -p tg-xtask 2>&1 || true
echo "[buildstorm] STEP8 arceos-build"
echo "----- build arceos-helloworld (timed, arch=$AXARCH) -----"
echo "BUILDSTORM_BEGIN mode=multi"
T0=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
{ timeout 14400 cargo xtask arceos build -p arceos-helloworld --arch "$AXARCH" 2>&1; \
  echo $? > /work/.build.rc; } | tee /work/buildstorm.build.out
RC=$(cat /work/.build.rc 2>/dev/null || echo 1); rm -f /work/.build.rc
T1=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
ELAPSED=$(awk "BEGIN{printf \"%.2f\", (\"$T1\"+0)-(\"$T0\"+0)}" 2>/dev/null); [ -z "$ELAPSED" ] && ELAPSED=0
ART=$(find target -type f \( -name 'arceos-helloworld' -o -name 'helloworld' \) 2>/dev/null | head -1)
BYTES=0
[ -n "$ART" ] && BYTES=$(wc -c <"$ART")
if [ "$RC" -eq 0 ] && [ -n "$ART" ] && [ "$BYTES" -ge 500000 ]; then
    echo "BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
else
    echo "BUILDSTORM_COMPILE mode=multi ok=false rc=$RC elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
    echo "----- buildstorm.build.out tail -----"
    tail -25 /work/buildstorm.build.out 2>/dev/null
fi
echo "#### OS COMP TEST GROUP END buildstorm ####"
sync"#####) } else { format!("./{}\0", script) }; let dash_c = "-c\0";         println!("[test-runner] {}: exec /bin/bash {} in chroot", group, command.trim_end_matches('\0')); if diag { let r = exec(shell, &[shell.as_ptr(), dash_c.as_ptr(), command.as_ptr(), core::ptr::null()], environ); println!("[test-runner] {}: exec {} (diag) failed ret={}", group, shell.trim_end_matches('\0'), r); println!("[test-runner] {}: exec failed, cwd_marker B", group); } else { let r = exec(shell, &[shell.as_ptr(), dash_c.as_ptr(), command.as_ptr(), core::ptr::null()], environ); println!("[test-runner] {}: exec {} failed ret={}", group, shell.trim_end_matches('\0'), r); println!("[test-runner] {}: exec failed, cwd_marker B", group); } exit(127); }
    let start = get_time() as u64; let mut status = 0;
    while pid > 0 && waitpid_wnohang(pid, &mut status) == 0 { if (get_time() as u64).saturating_sub(start) >= timeout * 1000 { let pgid = getpgid(pid as usize); if pgid > 0 { let _ = kill(!(pgid as usize) + 1, SIGKILL); } let _ = kill(pid as usize, SIGKILL); let _ = waitpid(pid as usize, &mut status); break; } sleep(100); }
    println!("#### OS COMP TEST GROUP END {}-{} ####", group, suffix); println!("[initproc] done {} in {} exit_code={}", script, work_dir.trim_end_matches('\0'), status);
}
pub fn run_selected_groups(environ: &[*const u8], cfg: &RuntimeConfig) {
    println!("[initproc] run_selected_groups start mask=0x{:03X}", cfg.mask);
    for &index in &cfg.order { let (group, script) = TEST_GROUPS[index]; if cfg.mask & (1 << index) == 0 { println!("[initproc] skip {} (mask bit{} not set)", group, index); continue; }
        // ltp_include 通过 ltprunner（suite 模式）处理 runtest 用例名过滤；
        // inline 模式只扫描实际二进制文件名，不解析 runtest 文件。
        if group == "ltp" && cfg.ltp_runner == LtpRunner::Inline {
            if cfg.ltp_libc != LtpLibc::Glibc { run_ltp_binaries(environ, "/musl\0", &cfg.ltp_exclude, &cfg.ltp_include, cfg.ltp_from.as_deref(), cfg.timeouts[index]); }
            if cfg.ltp_libc != LtpLibc::Musl { run_ltp_binaries(environ, "/glibc\0", &cfg.ltp_exclude, &cfg.ltp_include, cfg.ltp_from.as_deref(), cfg.timeouts[index]); }
        }
        else if group == "ltp" && (cfg.ltp_runner == LtpRunner::Suite || cfg.ltp_runner == LtpRunner::Script) {
            if cfg.ltp_libc != LtpLibc::Glibc { run_ltp_suite_runner(environ, "/musl/ltp", "musl", cfg.timeouts[index], cfg.conf_source.as_deref()); }
            if cfg.ltp_libc != LtpLibc::Musl { run_ltp_suite_runner(environ, "/glibc/ltp", "glibc", cfg.timeouts[index], cfg.conf_source.as_deref()); }
        }
        else if group == "cpython" { run_group_in_dir(environ, "/tools/tests/cpython\0", group, script, cfg.timeouts[index], cfg.diag); }
        else if group == "buildstorm" || group == "cagent" { run_group_chrooted(environ, "/sdcard\0", "/glibc\0", group, script, cfg.timeouts[index], cfg.diag); }
        else { run_group_in_dir(environ, "/musl\0", group, script, cfg.timeouts[index], cfg.diag); run_group_in_dir(environ, "/glibc\0", group, script, cfg.timeouts[index], cfg.diag); }
        sleep(1000); }
    println!("[initproc] run_selected_groups done");
}
