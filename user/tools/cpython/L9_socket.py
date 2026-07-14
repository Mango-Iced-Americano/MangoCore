import os
import sys
import ssl
import socket
import urllib.error
import urllib.request
from contextlib import contextmanager

fail = 0
PREFIX = "[CPYTHON L9]"
REQUIRE_NET = os.environ.get("CPYTHON_L9_REQUIRE_NET") == "1"

CA_FILE = "/tools/tests/cpython/etc/ssl/certs/ca-certificates.crt"
if "SSL_CERT_FILE" not in os.environ and os.path.exists(CA_FILE):
    os.environ["SSL_CERT_FILE"] = CA_FILE


class SkipTest(Exception):
    pass


@contextmanager
def check(name):
    global fail
    print(f"{PREFIX} test: {name}", flush=True)
    try:
        yield
        print(f"{PREFIX} test: {name} PASS", flush=True)
    except SkipTest as e:
        print(f"{PREFIX} test: {name} SKIP ({e})", flush=True)
    except Exception as e:
        print(f"{PREFIX} test: {name} FAIL ({type(e).__name__}: {e})", flush=True)
        fail = 1


def external_skip(message):
    if REQUIRE_NET:
        raise RuntimeError(message)
    raise SkipTest(message)


dns_addr = None
tcp_ok = False


with check("layer1 socket create close"):
    s1 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s1.close()

    s2 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s2.close()


with check("layer2 socketpair local communication"):
    a, b = socket.socketpair()
    try:
        a.settimeout(5.0)
        b.settimeout(5.0)

        a.sendall(b"ping")
        data = b.recv(4)
        assert data == b"ping", data

        b.sendall(b"pong")
        data = a.recv(4)
        assert data == b"pong", data
    finally:
        a.close()
        b.close()


with check("layer3 DNS getaddrinfo cloudflare.com"):
    try:
        infos = socket.getaddrinfo(
            "cloudflare.com",
            80,
            socket.AF_INET,
            socket.SOCK_STREAM,
        )
    except socket.gaierror as e:
        external_skip(f"DNS unavailable: {e}")
    except OSError as e:
        external_skip(f"DNS resolver failed: {e}")

    assert infos, "empty getaddrinfo result"
    dns_addr = infos[0][4]
    print(f"{PREFIX} DNS addr: {dns_addr}", flush=True)


with check("layer4 TCP HTTP cloudflare.com"):
    s = None
    try:
        if dns_addr is None:
            raise RuntimeError("DNS layer did not return an IPv4 endpoint")
        s = socket.create_connection(dns_addr, timeout=10.0)
        s.settimeout(10.0)
        req = (
            b"GET /cdn-cgi/trace HTTP/1.0\r\n"
            b"Host: cloudflare.com\r\n"
            b"Connection: close\r\n"
            b"\r\n"
        )
        s.sendall(req)
        data = s.recv(512)
    except OSError as e:
        external_skip(f"TCP/HTTP unavailable: {e}")
    finally:
        if s is not None:
            s.close()

    assert data.startswith(b"HTTP/"), data[:80]
    tcp_ok = True
    print(f"{PREFIX} HTTP head: {data[:60]!r}", flush=True)


with check("layer5 HTTPS cloudflare.com"):
    if not tcp_ok:
        raise SkipTest("TCP layer did not pass")

    try:
        if os.path.exists(CA_FILE):
            ctx = ssl.create_default_context(cafile=CA_FILE)
        else:
            ctx = ssl.create_default_context()

        with urllib.request.urlopen(
            "https://cloudflare.com/cdn-cgi/trace",
            timeout=15.0,
            context=ctx,
        ) as resp:
            status = getattr(resp, "status", None) or resp.getcode()
            body = resp.read(256)

    except ssl.SSLError as e:
        if "protocol" in str(e).lower():
            external_skip(f"SSL protocol not available: {e}")
        print(f"{PREFIX} HTTPS SSL error: {e}", flush=True)
        raise
    except urllib.error.URLError as e:
        external_skip(f"HTTPS unavailable: {e}")
    except OSError as e:
        external_skip(f"HTTPS socket unavailable: {e}")

    assert status in (200, 301, 302), status
    assert body, "empty HTTPS body"
    print(f"{PREFIX} HTTPS status: {status}", flush=True)


print(f"{PREFIX} RESULT {'PASS' if fail == 0 else 'FAIL'}", flush=True)
sys.exit(fail)
