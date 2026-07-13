#!/bin/sh
# Build a baseline LoongArch64 curl that does not require LSX/LASX state.

set -eu

VERSION="8.19.0"
URL="https://curl.se/download/curl-${VERSION}.tar.xz"
SHA256="4eb41489790d19e190d7ac7e18e82857cdd68af8f4e66b292ced562d333f11df"
OUTPUT="${1:-user/tools/loongarch64/curl-runtime}"
CACHE="${CURL_SOURCE_CACHE:-/tmp/mangocore-curl-source}"
ARCHIVE="$CACHE/curl-${VERSION}.tar.xz"
SOURCE="$CACHE/curl-${VERSION}"
CC="loongarch64-linux-gnu-gcc"

mkdir -p "$CACHE"
if [ ! -f "$ARCHIVE" ]; then
    echo "[curl-runtime] fetching $URL"
    curl --fail --location --retry 3 "$URL" --output "$ARCHIVE"
fi
echo "$SHA256  $ARCHIVE" | sha256sum -c -

rm -rf "$SOURCE"
tar -xJf "$ARCHIVE" -C "$CACHE"

cd "$SOURCE"
CC="$CC" \
CFLAGS="-O2 -march=loongarch64 -mabi=lp64d" \
LDFLAGS="" \
./configure \
    --host=loongarch64-linux-gnu \
    --disable-shared \
    --enable-static \
    --disable-docs \
    --disable-manual \
    --disable-threaded-resolver \
    --disable-alt-svc \
    --disable-hsts \
    --disable-dict \
    --disable-file \
    --disable-ftp \
    --disable-gopher \
    --disable-imap \
    --disable-ldap \
    --disable-ldaps \
    --disable-mqtt \
    --disable-pop3 \
    --disable-rtsp \
    --disable-smb \
    --disable-smtp \
    --disable-telnet \
    --disable-tftp \
    --without-brotli \
    --without-libidn2 \
    --without-libpsl \
    --without-libssh2 \
    --without-ssl \
    --without-zlib \
    --without-zstd

make -C lib -j"${JOBS:-2}"
make -C src -j"${JOBS:-2}" curl

cd - >/dev/null
rm -rf "$OUTPUT"
mkdir -p "$OUTPUT/bin" "$OUTPUT/lib64" "$OUTPUT/etc/ssl/certs"
install -m 0755 "$SOURCE/src/curl" "$OUTPUT/bin/curl"
loongarch64-linux-gnu-strip "$OUTPUT/bin/curl"

# The executable uses the standard glibc LP64D interpreter. Bundle the loader,
# libc and NSS resolver modules so the initramfs remains independent of the SSD.
for library in \
    ld-linux-loongarch-lp64d.so.1 \
    libc.so.6 \
    libnss_dns.so.2 \
    libnss_files.so.2 \
    libresolv.so.2
do
    source_path="$($CC -print-file-name="$library")"
    if [ "$source_path" = "$library" ] || [ ! -f "$source_path" ]; then
        echo "[curl-runtime] missing cross-runtime library: $library" >&2
        exit 1
    fi
    install -m 0755 "$source_path" "$OUTPUT/lib64/$library"
    loongarch64-linux-gnu-strip --strip-unneeded "$OUTPUT/lib64/$library"
done
cp os/initramfs/common/etc/ssl/certs/ca-certificates.crt \
    "$OUTPUT/etc/ssl/certs/ca-certificates.crt"
cat > "$OUTPUT/etc/nsswitch.conf" <<'EOF'
hosts: files dns
EOF
{
    echo "source=$URL"
    echo "source_sha256=$SHA256"
    echo "cflags=-O2 -march=loongarch64 -mabi=lp64d"
    echo "libc=glibc"
    echo "tls=disabled"
} > "$OUTPUT/manifest.txt"

echo "[curl-runtime] baseline HTTP curl ready: $OUTPUT"
