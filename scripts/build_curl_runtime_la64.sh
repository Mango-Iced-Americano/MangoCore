#!/bin/sh
# Build a LoongArch64 curl with a statically linked Mbed TLS backend.

set -eu

CURL_VERSION="8.19.0"
CURL_URL="https://curl.se/download/curl-${CURL_VERSION}.tar.xz"
CURL_SHA256="4eb41489790d19e190d7ac7e18e82857cdd68af8f4e66b292ced562d333f11df"
MBEDTLS_VERSION="3.6.7"
MBEDTLS_URL="https://github.com/Mbed-TLS/mbedtls/releases/download/mbedtls-${MBEDTLS_VERSION}/mbedtls-${MBEDTLS_VERSION}.tar.bz2"
MBEDTLS_SHA256="a7e8bcbec0e6f761b4af24f25677626b35f762f68eef79c08677a363212d11f6"
OUTPUT="${1:-user/tools/loongarch64/curl-runtime}"
CACHE="${CURL_SOURCE_CACHE:-/tmp/mangocore-curl-source}"
CURL_ARCHIVE="$CACHE/curl-${CURL_VERSION}.tar.xz"
CURL_SOURCE="$CACHE/curl-${CURL_VERSION}"
MBEDTLS_ARCHIVE="$CACHE/mbedtls-${MBEDTLS_VERSION}.tar.bz2"
MBEDTLS_SOURCE="$CACHE/mbedtls-${MBEDTLS_VERSION}"
MBEDTLS_BUILD="$CACHE/mbedtls-${MBEDTLS_VERSION}-build"
MBEDTLS_PREFIX="$CACHE/mbedtls-${MBEDTLS_VERSION}-loongarch64"
CC="loongarch64-linux-gnu-gcc"
CFLAGS="-O2 -march=loongarch64 -mabi=lp64d"

mkdir -p "$CACHE"
if [ ! -f "$MBEDTLS_ARCHIVE" ]; then
    echo "[curl-runtime] fetching $MBEDTLS_URL"
    curl --fail --location --retry 3 "$MBEDTLS_URL" --output "$MBEDTLS_ARCHIVE"
fi
echo "$MBEDTLS_SHA256  $MBEDTLS_ARCHIVE" | sha256sum -c -

rm -rf "$MBEDTLS_SOURCE" "$MBEDTLS_BUILD" "$MBEDTLS_PREFIX"
tar -xjf "$MBEDTLS_ARCHIVE" -C "$CACHE"
cmake -S "$MBEDTLS_SOURCE" -B "$MBEDTLS_BUILD" \
    -DCMAKE_SYSTEM_NAME=Linux \
    -DCMAKE_C_COMPILER="$CC" \
    -DCMAKE_C_FLAGS="$CFLAGS" \
    -DCMAKE_INSTALL_PREFIX="$MBEDTLS_PREFIX" \
    -DCMAKE_BUILD_TYPE=Release \
    -DENABLE_PROGRAMS=OFF \
    -DENABLE_TESTING=OFF \
    -DUSE_SHARED_MBEDTLS_LIBRARY=OFF \
    -DUSE_STATIC_MBEDTLS_LIBRARY=ON
cmake --build "$MBEDTLS_BUILD" --parallel "${JOBS:-2}"
cmake --install "$MBEDTLS_BUILD"

if [ ! -f "$CURL_ARCHIVE" ]; then
    echo "[curl-runtime] fetching $CURL_URL"
    curl --fail --location --retry 3 "$CURL_URL" --output "$CURL_ARCHIVE"
fi
echo "$CURL_SHA256  $CURL_ARCHIVE" | sha256sum -c -

rm -rf "$CURL_SOURCE"
tar -xJf "$CURL_ARCHIVE" -C "$CACHE"

cd "$CURL_SOURCE"
CC="$CC" \
CFLAGS="$CFLAGS" \
CPPFLAGS="-I$MBEDTLS_PREFIX/include" \
LDFLAGS="-L$MBEDTLS_PREFIX/lib" \
PKG_CONFIG_LIBDIR="$MBEDTLS_PREFIX/lib/pkgconfig" \
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
    --with-mbedtls="$MBEDTLS_PREFIX" \
    --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt \
    --without-ca-path \
    --without-zlib \
    --without-zstd

make -C lib -j"${JOBS:-2}"
make -C src -j"${JOBS:-2}" curl

cd - >/dev/null
rm -rf "$OUTPUT"
mkdir -p "$OUTPUT/bin" "$OUTPUT/lib64" "$OUTPUT/etc/ssl/certs"
install -m 0755 "$CURL_SOURCE/src/curl" "$OUTPUT/bin/curl"
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
    echo "curl_source=$CURL_URL"
    echo "curl_source_sha256=$CURL_SHA256"
    echo "mbedtls_source=$MBEDTLS_URL"
    echo "mbedtls_source_sha256=$MBEDTLS_SHA256"
    echo "cflags=$CFLAGS"
    echo "libc=glibc"
    echo "tls=mbedtls-$MBEDTLS_VERSION"
    echo "ca_bundle=/etc/ssl/certs/ca-certificates.crt"
} > "$OUTPUT/manifest.txt"

echo "[curl-runtime] HTTPS curl ready: $OUTPUT"
