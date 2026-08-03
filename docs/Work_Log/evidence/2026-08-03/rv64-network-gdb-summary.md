# RV64 clock-sync hang — exact Make and GDB evidence

## Deterministic root cause

The stall is an **interrupt-masked VirtIO-net completion spin**, not a block
device problem and not a `spin::Mutex` lock-order deadlock.

`ntpd` opens a UDP socket. `UdpSocket::new()` calls `NET_INTERFACE.poll()`
(`os/src/net/socket/inet/datagram/udp.rs:631`), which calls `poll_once()` while
holding `NET_INTERFACE.inner` (`os/src/net/config.rs:630,838`). smoltcp tries
to dispatch DHCP egress, and the VirtIO net driver spins in
`VirtQueue::add_notify_wait_pop()` until a completion arrives. The GDB snapshot
captures that exact stack below `sys_socket`, with `sstatus=0x8000000200006000`
(SIE clear) and `sie=0x20` (timer only). On the sole hart the completion cannot
be serviced, so `can_pop()` never becomes true.

There is no waiter/owner cycle: the same CPU is busy-waiting. It happens to
hold `NET_INTERFACE.inner` over the poll/send, but this lock has no competing
hart in the captured one-hart run.

## Exact target versus development target

| Invocation in this checkout | Actual QEMU profile | Result |
|---|---|---|
| `make -C os rv64-run ARCH=rv64 PROFILE=normal` | **competition**: target expands to `make -f make/rv64.mk comp`; official `sdcard-rv.img`, network, `-smp 1` | reproducibly stops at `[test-runner] synchronizing clock` |
| `make -C os run ARCH=rv64 PROFILE=normal` | **development**: self-built `rootfs-rv.img`, no NIC, `-smp threads=1` | prints fallback clock, completes the selected workload, shuts down |
| Prior investigation | manually launched development-equivalent QEMU with self-built rootfs, no NIC, private x1, and a gdbstub | passed; it did **not** exercise the literal `rv64-run` target or network egress |

`development-make-dry-run.txt` is the authoritative argv evidence. Therefore
the reported `rv64-run == development` premise does not match this checkout's
Makefile: `os/Makefile:95-96` maps `rv64-run` to `comp`.

## Block-device finding

All current local runs detected only `vda` (`0x10001000`) and `vdb`
(`0x10002000`), then mounted both root and tools. No local `vdc` was observed.
The prior CI vdc/8-minute event at old commit `c622a0e7` is a distinct
configuration/commit observation; it is not on the captured current stack.

## Toggle proof

With the same competition x0/x1 and `-smp 1`, replacing only
`QEMU_COMPETITION_AFTER_DRIVES` to omit its NIC/filter pair produced `No net
device`, then the fallback line and LTP progress. Restoring the ordinary
network-enabled literal target stalls. See `competition-no-net-toggle.log` and
`named-rv64-run-qemu.log`.

## Minimal repair

Remove or defer the unconditional `NET_INTERFACE.poll()` in
`UdpSocket::new()` (one line at `udp.rs:631`). Do **not** replace it with
`try_poll()`: it can still perform the same smoltcp egress. Let task-context
network polling execute after syscall return, where interrupts are enabled.

Before merging, add a focused regression that creates a UDP socket with a
pending DHCP client and verifies socket creation returns; then rebuild RV64 and
LA64 serially and repeat the network-enabled competition QEMU path.
