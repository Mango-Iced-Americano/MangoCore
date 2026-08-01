#!/usr/bin/env python3
"""VisionFive 2 (StarFive JH7110) board bring-up CLI.

Consolidates the throwaway serial scripts used during board bring-up into
one argparse subcommand tool. Board facts (overridable via flags):
serial /dev/ttyUSB0 @ 115200 8N1; U-Boot prompt "StarFive #";
TFTP ipaddr 192.168.200.10 / serverip 192.168.200.1; host TFTP root
/srv/tftp/vf2 (files without "vf2/" prefix); kernel loaded at
0x40200000 (kernel-rv-image, ~9.75 MiB); boot cmd
`booti 0x40200000 - $fdtcontroladdr`. TFTP is slow (~2-4 min at 512B
blocks), so we always `setenv tftpblocksize 1468` first.

After our kernel finishes its tests it AUTO-REBOOTS back to U-Boot via
SBI SRST; the CLI treats "StarFive #" reappearing as success.

Subcommands: probe, env, fdt, tftp-load, boot, capture, send, wait-uboot,
reset, flash-off. Requires pyserial; stdlib only otherwise.
"""

import argparse
import hashlib
import os
import re
import sys
import time

try:
    import serial
except ImportError:
    sys.stderr.write("ERROR: pyserial is required (pip install pyserial)\n")
    sys.exit(2)

# Board constants / defaults
DEFAULT_PORT = "/dev/ttyUSB0"
DEFAULT_BAUD = 115200

UBOOT_PROMPT = "StarFive #"
KERNEL_ADDR = "0x40200000"
KERNEL_MAGIC_ADDR = "0x40200038"          # magic2 lives 0x38 into the image
KERNEL_MAGIC = "05435352"                  # 0x05435352, as printed by U-Boot md
BOOT_CMD = "booti 0x40200000 - $fdtcontroladdr"

BOARD_IP = "192.168.200.10"
SERVER_IP = "192.168.200.1"

TFTP_ROOT = "/srv/tftp/vf2"
DEFAULT_IMAGE = "kernel-rv-image"
TFTP_FAIL = "ERROR: TFTP transfer did not complete (no 'Bytes transferred =' line)\n"

# Markers our kernel prints during boot/test; we report which were seen.
KERNEL_MARKERS = ["[kernel]", "[initramfs]", "[init]", "[KTEST RESULT]",
                  "[L4 REGRESSION RESULT]", "[exit]"]


# Serial helpers
def open_serial(port, baud):
    """Open the serial port with 8N1 framing and a short read timeout."""
    try:
        return serial.Serial(port=port, baudrate=baud, timeout=0.1,
                             write_timeout=2.0)  # short timeout = responsive polls
    except serial.SerialException as exc:
        sys.stderr.write("ERROR: cannot open %s: %s\n" % (port, exc))
        sys.exit(1)


def drain(ser, quiet=0.3, echo=True):
    """Read until input stays idle for `quiet` seconds; returns the bytes."""
    buf = b""
    last_activity = time.monotonic()
    while True:
        data = ser.read(256)
        if data:
            buf += data
            last_activity = time.monotonic()
            if echo:
                sys.stdout.buffer.write(data)
                sys.stdout.flush()
        elif time.monotonic() - last_activity >= quiet:
            break
        else:
            time.sleep(0.02)
    return buf


def send_cmd(ser, cmd, wait=0.5, quiet=0.3, echo=True):
    """Send one command line and drain the echo + response (returns bytes)."""
    ser.reset_input_buffer()
    ser.write((cmd + "\r").encode())
    ser.flush()
    if wait:
        time.sleep(wait)
    return drain(ser, quiet=quiet, echo=echo)


def wait_for(ser, pattern, timeout, callback=None):
    """Poll until `pattern` appears or timeout; returns (found, transcript).

    `callback` receives each received bytes chunk (e.g. TFTP progress).
    """
    buf = b""
    window = ""  # bounded rolling text window for cheap pattern checks
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        data = ser.read(256)
        if not data:
            time.sleep(0.02)
            continue
        buf += data
        if callback:
            callback(data)
        window += data.decode(errors="replace")
        if len(window) > 65536: window = window[-65536:]
        if pattern in window:
            return True, buf
    return False, buf


def _print_chunk(data):
    """wait_for callback that streams received bytes to stdout."""
    sys.stdout.buffer.write(data)
    sys.stdout.flush()


# Shared board operations
def _stop_autoboot(ser, wait=45):
    """Bring the board to an idle U-Boot prompt.

    U-Boot boots a distro image by default unless the countdown is
    cancelled. If the board just powered up, the countdown may be missed —
    then it runs the full distro boot (mmc/nvme scan) and returns to the
    prompt on its own. Loop: send Ctrl+C early to cancel the countdown,
    then wait for the prompt; repeat until the prompt is seen or `wait`
    seconds elapse.
    """
    deadline = time.monotonic() + wait
    while time.monotonic() < deadline:
        ser.reset_input_buffer()
        ser.write(b"\x03")  # Ctrl+C cancels the countdown if still running
        time.sleep(0.3)
        found, _ = wait_for(ser, UBOOT_PROMPT, 3)
        if found:
            send_cmd(ser, "echo READY", wait=0.4)
            return
    # Fall back to sending Ctrl+C once more and draining.
    ser.write(b"\x03")
    time.sleep(0.3)
    drain(ser, quiet=0.3)


def _ensure_network(ser):
    """Idempotently configure U-Boot networking for TFTP.

    Board env vars are lost on power cycle, so every TFTP-dependent
    subcommand must (re)apply them before loading. All writes are volatile
    (no saveenv) — safe and repeatable.
    """
    send_cmd(ser, "setenv ipaddr %s" % BOARD_IP, wait=0.3)
    send_cmd(ser, "setenv serverip %s" % SERVER_IP, wait=0.3)
    send_cmd(ser, "setenv tftpblocksize 1468", wait=0.3)


def _run_tftp_load(ser, image, timeout):
    """Stop autoboot, ensure networking, tftpboot <image>; return size or None."""
    _stop_autoboot(ser)
    _ensure_network(ser)
    ser.reset_input_buffer()
    ser.write(("tftpboot %s %s\r" % (KERNEL_ADDR, image)).encode())
    ser.flush()
    found, buf = wait_for(ser, "Bytes transferred =", timeout,
                          callback=_print_chunk)
    if not found:
        return None
    buf += drain(ser, quiet=0.4)  # capture the "... bytes in ... (.. B/s)" tail
    match = re.search(r"Bytes transferred = (\d+)", buf.decode(errors="replace"))
    return int(match.group(1)) if match else None


def _verify_magic(ser):
    """Check magic2 at 0x40200038 == 0x05435352 via `md 0x40200038 4`."""
    out = send_cmd(ser, "md %s 4" % KERNEL_MAGIC_ADDR, wait=0.5)
    return KERNEL_MAGIC in out.decode(errors="replace")


# Subcommands
def cmd_probe(args):
    """Send 'echo PONG' and report whether the board answers."""
    ser = open_serial(args.port, args.baud)
    try:
        ser.reset_input_buffer()
        ser.write(b"echo PONG\r")
        ser.flush()
        found, buf = wait_for(ser, "PONG", args.timeout)
        text = buf.decode(errors="replace")
        alive = found and UBOOT_PROMPT in text
        print("board ALIVE (PONG + U-Boot prompt)" if alive else "board NOT responding")
        return 0 if alive else 1
    finally:
        ser.close()


def cmd_env(args):
    """Print the key U-Boot env vars (read-only)."""
    ser = open_serial(args.port, args.baud)
    try:
        send_cmd(ser, "printenv ipaddr serverip kernel_addr_r fdt_addr_r fdtcontroladdr", wait=0.6)
    finally:
        ser.close()
    return 0


def cmd_fdt(args):
    """Show the control FDT (read-only)."""
    ser = open_serial(args.port, args.baud)
    try:
        send_cmd(ser, "fdt addr -c", wait=0.6)
    finally:
        ser.close()
    return 0


def cmd_tftp_load(args):
    """TFTP-load an image and verify the kernel magic."""
    print("==> TFTP-loading '%s' to %s (timeout %ds, blocksize 1468)" % (args.image, KERNEL_ADDR, args.timeout))
    ser = open_serial(args.port, args.baud)
    try:
        size = _run_tftp_load(ser, args.image, args.timeout)
        if size is None:
            sys.stderr.write(TFTP_FAIL)
            return 1
        print("\nTransferred %d bytes (%.2f MiB)" % (size, size / 1024.0 / 1024.0))
        magic_ok = _verify_magic(ser)
        print("Kernel magic2 @ %s == 0x%s: %s" % (KERNEL_MAGIC_ADDR, KERNEL_MAGIC, "OK" if magic_ok else "MISMATCH"))
        return 0 if magic_ok else 1
    finally:
        ser.close()


def cmd_boot(args):
    """tftp-load then booti, capture output until auto-reboot back to U-Boot."""
    log_path = args.log or "vf2-boot-%s.log" % time.strftime("%Y%m%d-%H%M%S")
    ser = open_serial(args.port, args.baud)
    try:
        print("==> TFTP-loading '%s'" % args.image)
        size = _run_tftp_load(ser, args.image, args.timeout)
        if size is None:
            sys.stderr.write(TFTP_FAIL)
            return 1
        print("\nTransferred %d bytes. Booting..." % size)

        # Send booti and capture immediately. Earlier this drained for
        # 0.8s after booti, which swallowed the kernel's early serial
        # output ([kernel] Hello, [dw_mshc] probe) — exactly the lines
        # needed to verify the SD driver on the real board.
        ser.reset_input_buffer()
        ser.write((BOOT_CMD + "\r").encode())
        ser.flush()

        seen = {m: False for m in KERNEL_MARKERS}
        rebooted = False
        window = ""
        start = time.monotonic()
        deadline = start + args.timeout
        with open(log_path, "w") as log:
            while time.monotonic() < deadline:
                data = ser.read(256)
                if not data:
                    continue
                s = data.decode(errors="replace")
                sys.stdout.write(s)
                sys.stdout.flush()
                log.write(s)
                window += s
                if len(window) > 65536:
                    window = window[-65536:]
                for m in seen:
                    if m in window: seen[m] = True
                if UBOOT_PROMPT in window:
                    rebooted = True
                    break
        elapsed = time.monotonic() - start
    finally:
        ser.close()

    found_markers = [m for m in KERNEL_MARKERS if seen[m]]
    status = ("REBOOTED back to U-Boot after %.0fs" % elapsed) if rebooted \
        else "TIMEOUT after %.0fs (no U-Boot prompt)" % elapsed
    print("\n=== boot summary ===")
    print("image        : %s (%d bytes)" % (args.image, size))
    print("result       : %s" % status)
    print("transcript   : %s" % log_path)
    print("markers      : %s" % (found_markers if found_markers else "(none)"))
    missing = [m for m in KERNEL_MARKERS if not seen[m]]
    if missing:
        print("markers MISS : %s" % missing)
    return 0 if rebooted else 1


def cmd_capture(args):
    """Stream serial output for N seconds to stdout (optionally to a log)."""
    ser = open_serial(args.port, args.baud)
    f = open(args.log, "w") if args.log else None
    try:
        print("==> Capturing serial output for %gs (Ctrl-C to stop early)"
              % args.seconds)
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            data = ser.read(256)
            if data:
                sys.stdout.buffer.write(data)
                sys.stdout.flush()
                if f:
                    f.write(data.decode(errors="replace"))
    except KeyboardInterrupt:
        pass
    finally:
        if f:
            f.close()
        ser.close()
    return 0


def cmd_send(args):
    """Send one raw U-Boot command and print its response."""
    ser = open_serial(args.port, args.baud)
    try:
        out = send_cmd(ser, args.cmd, wait=args.wait)
        if not out:
            print("(no response)")
    finally:
        ser.close()
    return 0


def cmd_wait_uboot(args):
    """Poll serial until the U-Boot prompt appears (auto-reboot detection)."""
    ser = open_serial(args.port, args.baud)
    try:
        found, buf = wait_for(ser, UBOOT_PROMPT, args.seconds)
        text = buf.decode(errors="replace")
        if text:
            sys.stdout.write(text if text.endswith("\n") else text + "\n")
        print("U-Boot prompt detected"
              if found else "TIMEOUT: no U-Boot prompt within %gs" % args.seconds)
        return 0 if found else 1
    finally:
        ser.close()


def cmd_reset(args):
    """Pulse DTR+RTS low to attempt a hardware reset, then watch for U-Boot."""
    ser = open_serial(args.port, args.baud)
    try:
        print("==> Asserting DTR+RTS low for 300ms to pulse board reset...")
        ser.setDTR(False)
        ser.setRTS(False)
        time.sleep(0.3)
        ser.setDTR(True)
        ser.setRTS(True)
        time.sleep(0.2)
        print("Released. Watching for a reset (banner / U-Boot prompt)...")
        found, buf = wait_for(ser, UBOOT_PROMPT, 10)
        if buf:
            sys.stdout.write(buf.decode(errors="replace"))
        if found:
            print("RESET OBSERVED: U-Boot prompt returned.")
        else:
            print("NO RESET OBSERVED within 10s.")
            print("NOTE: on this setup the USB-serial adapter is usually NOT wired")
            print("to the board's reset/EN line, so DTR/RTS toggling often does nothing;")
            print("power-cycle the board manually if needed.")
        return 0 if found else 1
    finally:
        ser.close()


def cmd_flash_off(args):
    """Safety check of the TFTP image. No board interaction."""
    path = os.path.join(TFTP_ROOT, DEFAULT_IMAGE)
    if not os.path.isfile(path):
        sys.stderr.write("ERROR: %s does not exist\n" % path)
        return 1
    size = os.path.getsize(path)
    with open(path, "rb") as fh:
        md5 = hashlib.md5(fh.read()).hexdigest()
    print("image : %s" % path)
    print("size  : %d bytes (%.2f MiB)" % (size, size / 1024.0 / 1024.0))
    print("md5   : %s" % md5)
    print("No board interaction. Safe to boot this image.")
    return 0


# Argument parser
def build_arg_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--port", default=DEFAULT_PORT, metavar="PORT",
                        help="serial port (default: %s)" % DEFAULT_PORT)
    common.add_argument("--baud", type=int, default=DEFAULT_BAUD, metavar="BAUD",
                        help="serial baud rate (default: %d)" % DEFAULT_BAUD)

    parser = argparse.ArgumentParser(
        prog="vf2_cli.py",
        description="VisionFive 2 board bring-up CLI (U-Boot over serial).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  vf2_cli.py probe\n"
            "  vf2_cli.py boot kernel-rv-image --timeout 400 --log /tmp/boot.log\n"
            "  vf2_cli.py send \"ls mmc 1:3 /\"\n"
        ),
    )
    sub = parser.add_subparsers(dest="command", metavar="COMMAND", required=True)

    p = sub.add_parser("probe", parents=[common],
                       help="check board liveness via echo PONG")
    p.add_argument("--timeout", type=float, default=5,
                   help="seconds to wait for PONG (default 5)")
    p.set_defaults(func=cmd_probe)

    p = sub.add_parser("env", parents=[common],
                       help="print key U-Boot env vars (read-only)")
    p.set_defaults(func=cmd_env)

    p = sub.add_parser("fdt", parents=[common],
                       help="show the control FDT (read-only)")
    p.set_defaults(func=cmd_fdt)

    p = sub.add_parser("tftp-load", parents=[common],
                       help="TFTP-load an image to 0x40200000 and verify magic")
    p.add_argument("image", nargs="?", default=DEFAULT_IMAGE,
                   help="image file name on the TFTP server (default: %s)"
                        % DEFAULT_IMAGE)
    p.add_argument("--timeout", type=float, default=300,
                   help="TFTP transfer timeout in seconds (default 300)")
    p.set_defaults(func=cmd_tftp_load)

    p = sub.add_parser("boot", parents=[common],
                       help="tftp-load then booti; watch for the auto-reboot")
    p.add_argument("image", nargs="?", default=DEFAULT_IMAGE,
                   help="image file name on the TFTP server (default: %s)"
                        % DEFAULT_IMAGE)
    p.add_argument("--timeout", type=float, default=300,
                   help="seconds to watch for the reboot (default 300)")
    p.add_argument("--log", default=None, metavar="PATH",
                   help="transcript log path (default: vf2-boot-<ts>.log in cwd)")
    p.set_defaults(func=cmd_boot)

    p = sub.add_parser("capture", parents=[common],
                       help="stream serial output for N seconds")
    p.add_argument("seconds", nargs="?", type=float, default=60,
                   help="seconds to capture (default 60)")
    p.add_argument("--log", default=None, metavar="PATH",
                   help="also save the output to this file")
    p.set_defaults(func=cmd_capture)

    p = sub.add_parser("send", parents=[common],
                       help="send one raw U-Boot command")
    p.add_argument("cmd", help="command to send, e.g. 'ls mmc 1:3 /'")
    p.add_argument("--wait", type=float, default=1.0,
                   help="seconds to wait for the response (default 1.0)")
    p.set_defaults(func=cmd_send)

    p = sub.add_parser("wait-uboot", parents=[common],
                       help="poll until the U-Boot prompt appears")
    p.add_argument("seconds", nargs="?", type=float, default=30,
                   help="timeout in seconds (default 30)")
    p.set_defaults(func=cmd_wait_uboot)

    p = sub.add_parser("reset", parents=[common],
                       help="attempt a hardware reset via DTR/RTS toggling")
    p.set_defaults(func=cmd_reset)

    p = sub.add_parser("flash-off",
                       help="safety check of the TFTP image (no board interaction)")
    p.set_defaults(func=cmd_flash_off)

    return parser


def main():
    parser = build_arg_parser()
    args = parser.parse_args()
    try:
        return args.func(args)
    except serial.SerialException as exc:
        sys.stderr.write("ERROR: serial I/O failed: %s\n" % exc)
        return 1
    except KeyboardInterrupt:
        sys.stderr.write("\ninterrupted\n")
        return 130


if __name__ == "__main__":
    sys.exit(main())
