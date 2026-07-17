#!/usr/bin/env bash
# Build a self-contained LoongArch64 CPython runtime whose complete native
# dependency closure is compiled with -mstrict-align.

set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [[ "$ROOT" != /app ]]; then
    echo "error: run this script inside the project Docker container (/app)" >&2
    exit 2
fi

OUT=${CPYTHON_STRICT_OUT:-$ROOT/target/cpython-strict}
JOBS=${CPYTHON_STRICT_JOBS:-4}
PYTHON_JOBS=${CPYTHON_STRICT_PYTHON_JOBS:-1}
CACHE=$OUT/cache
SOURCES=$OUT/sources
BUILD=$OUT/build
STAMPS=$OUT/stamps
SYSROOT=$OUT/sysroot
HOSTPY=$OUT/host-python
RUNTIME=$OUT/runtime
ARTIFACTS=$OUT/artifacts
WRAP=$OUT/cross

mkdir -p "$CACHE" "$SOURCES" "$BUILD" "$STAMPS" "$SYSROOT" "$ARTIFACTS" "$WRAP"

TOOLCHAIN_ARCHIVE=x86_64-cross-tools-loongarch64-unknown-linux-musl-latest-20250911.tar.xz
TOOLCHAIN_URL=https://github.com/loong64/cross-tools/releases/download/20250911/x86_64-cross-tools-loongarch64-unknown-linux-musl-latest.tar.xz
TOOLCHAIN_SHA256=2d56d07146ed712ac44f5063f54a19656fce851492c3f1c10e31e6b6633db6d7
TOOLCHAIN_ROOT=$OUT/toolchain/full-gcc-15.2.0-musl-20250911/loongarch64-unknown-linux-musl
TOOLCHAIN_PREFIX=loongarch64-unknown-linux-musl

TARGET=loongarch64-linux-musl
BUILD_TRIPLE=x86_64-pc-linux-gnu
STRICT_FLAGS="-march=loongarch64 -mabi=lp64d -mstrict-align"
COMMON_CFLAGS="-Os -fstack-clash-protection -Wformat -Werror=format-security -fno-plt $STRICT_FLAGS"
COMMON_CPPFLAGS="-I$SYSROOT/usr/include"
COMMON_LDFLAGS="-L$SYSROOT/usr/lib -L$SYSROOT/lib -Wl,-rpath-link,$SYSROOT/usr/lib -Wl,-rpath-link,$SYSROOT/lib -Wl,--as-needed -Wl,-O1 -Wl,--sort-common"

declare -A APORTS_COMMIT=(
    [musl]=b0c8ea10e8f29cabe336b2e5d864124940e126ab
    [zlib]=f248b33b5943c7dc69bf691031d7612ab2e8ed93
    [bzip2]=33283848034c9885d984c8e8697c645c57324938
    [xz]=ce9944f1daadb681dd6f0f81a06e9cce97127377
    [libffi]=2c2a7bb4a8b16066834e90402567b2c19403a790
    [expat]=acbd3e4e0d1525f98da93aa9f7cf733aff995736
    [sqlite]=e716785aa8fead790466477c2e00cad115a5383b
    [openssl]=35c0d1f2b314f647008595f681786813760da191
    [ncurses]=2cee8a7328d061418336ad327b512d96bcd7bc5e
    [readline]=a854c03acdac188901fb012f7acbee70a36e8041
    [mpdecimal]=c0519b85456e9b838e3142e1d233aab0b433b476
    [python3]=0266eb2db4c93e7ee1d51e9e50f5baacbc15303c
)

log() {
    printf '[strict-cpython] %s\n' "$*"
}

fetch_sha256() {
    local url=$1 name=$2 sha=$3
    local path=$CACHE/$name
    if [[ -f "$path" ]] && printf '%s  %s\n' "$sha" "$path" | sha256sum --check --status; then
        return
    fi
    curl --fail --location --retry 5 --retry-all-errors --output "$path" "$url"
    printf '%s  %s\n' "$sha" "$path" | sha256sum --check --status || {
        echo "sha256 mismatch: $path" >&2
        exit 1
    }
}

fetch_sha512() {
    local url=$1 name=$2 sha=$3
    local path=$CACHE/$name
    if [[ -f "$path" ]] && printf '%s  %s\n' "$sha" "$path" | sha512sum --check --status; then
        return
    fi
    curl --fail --location --retry 5 --retry-all-errors --output "$path" "$url"
    printf '%s  %s\n' "$sha" "$path" | sha512sum --check --status || {
        echo "sha512 mismatch: $path" >&2
        exit 1
    }
}

fetch_aports_file() {
    local package=$1 name=$2 sha=$3
    local commit=${APORTS_COMMIT[$package]}
    fetch_sha512 \
        "https://gitlab.alpinelinux.org/alpine/aports/-/raw/$commit/main/$package/$name" \
        "$package-$name" "$sha"
}

unpack_tar() {
    local archive=$1 destination=$2
    rm -rf -- "$destination"
    mkdir -p "$destination"
    tar -xf "$archive" -C "$destination" --strip-components=1
}

apply_p1() {
    local tree=$1 patch=$2
    patch --directory="$tree" --strip=1 --forward < "$patch"
}

apply_p0() {
    local tree=$1 patch=$2
    patch --directory="$tree" --strip=0 --forward < "$patch"
}

mark_done() {
    touch "$STAMPS/$1.done"
}

is_done() {
    [[ -f "$STAMPS/$1.done" ]]
}

fetch_sources() {
    log "fetching and verifying pinned sources"
    fetch_sha256 "$TOOLCHAIN_URL" "$TOOLCHAIN_ARCHIVE" "$TOOLCHAIN_SHA256"
    fetch_sha256 \
        https://dl-cdn.alpinelinux.org/alpine/edge/main/loongarch64/linux-headers-7.1.3-r0.apk \
        linux-headers-7.1.3-r0.apk \
        7bdf8aa74d42130ecc422b921425877f31b0afa754b6265507c3d1682ec03ec3

    fetch_sha512 https://musl.libc.org/releases/musl-1.2.6.tar.gz musl-1.2.6.tar.gz \
        1adad96eddb3a2eb0cacb3e363b0046568925fcdd75cf8b0503f2139df1f693d64730779ca0ce8131b7624ab2d37f4247bb1d3393c523de6e30d2b1d7732555c
    fetch_aports_file musl handle-aux-at_base.patch \
        a76f79b801497ad994746cf82bb6eaf86f9e1ae646e6819fbae8532a7f4eee53a96ac1d4e789ec8f66aea2a68027b0597f7a579b3369e01258da8accfce41370
    fetch_aports_file musl 0001-add-stub-for-pthread_mutexattr_setprioceiling.patch \
        83ec0f774dbb5d4f4d4b917d149ea0ef609d028d18ae7624e90fedda6e3174c91ade93751bfd376adfa2848155110fc4dd02b6790363b98e5395c4787e8731e8
    fetch_aports_file musl fix-loongarch64-zero-len-extcontext.patch \
        6eae45aa82db69bc96386aedf3d1bfe83dfa0a7b4b824326c1c7e1bab31100d635ed0e8930c26a1694697fad59167a42e2b5f3a2ac51b07e36d85f047e268cff
    fetch_aports_file musl iconv-gb18030-fix.patch \
        f7849abeab0e4eab992a80464afa07b9c8aae5cee76040523b9dbc0931435f28ea8f5792b8a4a0cd6d608ac2f37e3225e89afcb3171a0a6df44450ed57cee83b
    fetch_aports_file musl CVE-2026-40200.patch \
        a64ab7688d1a85e560b5687783df482d2467a79a74400da5a1601382847d6b4e6a79b7529a8dc80c46ddda8dc2a812f2261557fc8ee98dc3bdf7322761bd6d9c

    fetch_sha512 https://github.com/madler/zlib/releases/download/v1.3.2/zlib-1.3.2.tar.gz zlib-1.3.2.tar.gz \
        70963771ea5d763614278a69b474f09b7d237ef8f53b675a10fe31d9923aeef601504b35d7ebd1b1e7f347e9ebb048e6b3b47fffdf137e7bdc7e8d5eb4ec4692
    fetch_sha512 https://sourceware.org/pub/bzip2/bzip2-1.0.8.tar.gz bzip2-1.0.8.tar.gz \
        083f5e675d73f3233c7930ebe20425a533feedeaaa9d8cc86831312a6581cefbe6ed0d08d2fa89be81082f2a5abdabca8b3c080bf97218a1bd59dc118a30b9f3
    fetch_aports_file bzip2 bzip2-1.0.4-makefile-CFLAGS.patch \
        d0430ae96d7a2d4e658a101c84262ba11048e3e3110ae9d7855b36792abc7827c0daba3cdcdec629130a9d3beb128052de458242e494a35962e903e50eddfe45
    fetch_aports_file bzip2 bzip2-1.0.4-man-links.patch \
        2d9a306bc0f552a58916ebc702d32350a225103c487e070d2082121a54e07f1813d3228f43293cc80a4bee62053fd597294c99a1751b1685cd678f4e5c6a2fe7
    fetch_aports_file bzip2 bzip2-1.0.2-progress.patch \
        b6810c73428f17245e0d7c2decd00c88986cd8ad1cfe4982defe34bdab808d53870ed92cb513b2d00c15301747ceb6ca958fb0e0458d0663b7d8f7c524f7ba4e
    fetch_aports_file bzip2 bzip2-1.0.3-no-test.patch \
        aefcafaaadc7f19b20fe023e0bd161127b9f32e0cd364621f6e5c03e95fb976e7e69e354ec46673a554392519532a3bfe56d982a5cde608c10e0b18c3847a030
    fetch_aports_file bzip2 saneso.patch \
        dd624110ce06426d2990ad1de96f5b6a2790c599030fb8848e26b64aa847cf956806f7a539fe61c6005d99bfc135920fc704f274862d2557ab1861adb7391d45

    fetch_sha512 https://github.com/tukaani-project/xz/archive/refs/tags/v5.8.3/xz-5.8.3.tar.gz xz-5.8.3.tar.gz \
        8fb5e6a13397d259d8ff7484f9b63f8a6752ff1c63e1a4601170ad8175aadefb5126a1cae7f73370bfc6c2a0b4e1c0bad57a58fc5b781d3f7d45e5a483c091cc
    fetch_sha512 https://github.com/libffi/libffi/releases/download/v3.5.2/libffi-3.5.2.tar.gz libffi-3.5.2.tar.gz \
        76974a84e3aee6bbd646a6da2e641825ae0b791ca6efdc479b2d4cbcd3ad607df59cffcf5031ad5bd30822961a8c6de164ac8ae379d1804acd388b1975cdbf4d
    fetch_sha512 https://github.com/libexpat/libexpat/releases/download/R_2_8_2/expat-2.8.2.tar.xz expat-2.8.2.tar.xz \
        68ee856b3eeefeb6bb800004951bbbe89a9a144354ae12bc9d670888fd89e8513243e0053c61674430c78e2beeeb85a3c86ac7644576a5bb9867fbce3643ff8d
    fetch_sha512 https://www.bytereef.org/software/mpdecimal/releases/mpdecimal-4.0.1.tar.gz mpdecimal-4.0.1.tar.gz \
        431fa8ab90d6b8cdecc38b1618fd89d040185dec3c1150203e20f40f10a16160058f6b8abddd000f6ecb74f4dc42d9fef8111444f1496ab34c34f6b814ed32b7

    fetch_sha512 https://github.com/openssl/openssl/releases/download/openssl-3.5.7/openssl-3.5.7.tar.gz openssl-3.5.7.tar.gz \
        de5351d2d532e1a3908a738f7d8aae448d32bc60bdb24808c556a24bc37a3f53daedf12b5d432eeb8c235e16939d842f908332ede8a447ca103ad1c493c820d7
    fetch_aports_file openssl auxv.patch \
        b2541075148fd5af4552d34158deb1a325f5adced90626dc03fd126f47323cde949b15b2657523c34b26530b322312a43bd42ada7295b4f2dd1d3b5d11892c62

    fetch_sha512 https://invisible-mirror.net/archives/ncurses/current/ncurses-6.6-20260516.tgz ncurses-6.6-20260516.tgz \
        20ffc27f3266b078b410f712422db00d77f5fd497e398b7be252fd207ab70ebd72a2bdf8fd6c94e2a9e41bd2b64d1c6502240c0b8423c99add073bd1226f5859
    fetch_aports_file ncurses cleanup-pkgconfig-ldflags.patch \
        201ef1876655101cedabc83a0ce46f75079b08f565ca8de4cf96fd69e41332a2d0597b77fe360dc58b10772586fa39bd52ac9ee670a912fef84840278356065a
    fetch_sha512 https://ftp.gnu.org/gnu/readline/readline-8.3.tar.gz readline-8.3.tar.gz \
        513002753dcf5db9213dbbb61d51217245f6a40d33b1dd45238e8062dfa8eef0c890b87a5548e11db959e842724fb572c4d3d7fb433773762a63c30efe808344
    fetch_aports_file readline fix-ncurses-underlinking.patch \
        3fa096385feee5f6c01866ef220a92ba646dcebf59c6bba701bc5a3d234df899c37c8f7954b900aa2738c70697b7444da2c9b76dc64ba33c68dec02ac2244371
    fetch_sha512 https://ftp.gnu.org/gnu/readline/readline-8.3-patches/readline83-001 readline83-001.patch \
        ced50af353ed527f6ec0eac5f65261f2ed208825ec72fe2acf5f0217f34f84f33dcbf01b895325f6b33664b5a426bac99506193e2ddb6eea8c79ccad37364b89
    fetch_sha512 https://ftp.gnu.org/gnu/readline/readline-8.3-patches/readline83-002 readline83-002.patch \
        e45ad6443bd4e271ec8e8ab883de561b6420aec362b0b7f0256086cb5a023d946df55994ed99c76ceb191e8a25e8059ae9b553ef1d546626d671b80af292f04d
    fetch_sha512 https://ftp.gnu.org/gnu/readline/readline-8.3-patches/readline83-003 readline83-003.patch \
        6b3ebffe994d0cd4d3466b15e3aee9a73613109283a4442f3bf10e28edcd1204df824c71356d66d01ac21014a806023a101fedf94526a19f6f590d9ffdc864cd

    fetch_sha512 https://www.sqlite.org/2026/sqlite-autoconf-3530300.tar.gz sqlite-autoconf-3530300.tar.gz \
        355a8db490ec2a68c2801644e56178a26416c355792586a6c1c904de116e26f8602bc344e7172181c9d92c4c9e696319243e16405460fad87b23ee997a3ef9da
    fetch_sha512 https://www.python.org/ftp/python/3.14.5/Python-3.14.5.tar.xz Python-3.14.5.tar.xz \
        efbaf629703cd004f6b7bc75fb16df794185589adaf8807cd45928f212271045a399df3cd9573e47c8708fb5c5002f9d4efe4e41dde4313b81a3e9d73158769f
    fetch_aports_file python3 musl-find_library.patch \
        ab8eaa2858d5109049b1f9f553198d40e0ef8d78211ad6455f7b491af525bffb16738fed60fc84e960c4889568d25753b9e4a1494834fea48291b33f07000ec2
}

setup_toolchain() {
    if [[ ! -x "$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-gcc" ]]; then
        local toolchain_parent=${TOOLCHAIN_ROOT%/loongarch64-unknown-linux-musl}
        mkdir -p "$toolchain_parent"
        tar -xJf "$CACHE/$TOOLCHAIN_ARCHIVE" -C "$toolchain_parent"
    fi
    export PATH="$TOOLCHAIN_ROOT/bin:$WRAP:$PATH"
    GCC=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-gcc
    GXX=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-g++
    # Use the GCC frontends so archives containing LTO objects carry the
    # correct plugin metadata during CPython's PGO/LTO build.
    AR=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-gcc-ar
    RANLIB=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-gcc-ranlib
    STRIP=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-strip
    READELF=$TOOLCHAIN_ROOT/bin/$TOOLCHAIN_PREFIX-readelf
    QEMU=${CPYTHON_STRICT_QEMU:-}
    if [[ -z "$QEMU" ]]; then
        QEMU=$(command -v qemu-loongarch64-static || command -v qemu-loongarch64 || true)
    fi
    if [[ -z "$QEMU" ]]; then
        if is_done python-target && is_done runtime-package; then
            # A completed cache only needs its archive/index verified; none of
            # the stamped build stages below execute a target binary.
            QEMU=/bin/false
            log "qemu-user unavailable; using completed target/runtime cache"
        else
            echo "missing qemu-loongarch64-static; install qemu-user-static in the project Docker image" >&2
            exit 1
        fi
    fi
    "$GCC" -Werror $STRICT_FLAGS -x c -c /dev/null -o "$OUT/strict-flag-probe.o"
    # CPython's --enable-optimizations needs the complete libgcov profiler
    # runtime.  Minimal nolibc toolchains compile the sources but fail only at
    # the final PGO link, so reject them before the expensive build starts.
    local libgcov
    libgcov=$("$GCC" -print-file-name=libgcov.a)
    # Do not use grep -q here: with pipefail it closes the pipe early and nm
    # can report SIGPIPE, incorrectly rejecting a valid archive.
    nm "$libgcov" | grep ' T __gcov_exit$' >/dev/null || {
        echo "toolchain has incomplete PGO/libgcov support: $libgcov" >&2
        exit 1
    }
}

install_kernel_headers() {
    if is_done kernel-headers; then return; fi
    tar -xzf "$CACHE/linux-headers-7.1.3-r0.apk" -C "$SYSROOT" \
        --exclude='.SIGN.*' --exclude='.PKGINFO'
    mark_done kernel-headers
}

build_musl() {
    if is_done musl; then return; fi
    local src=$BUILD/musl
    unpack_tar "$CACHE/musl-1.2.6.tar.gz" "$src"
    apply_p1 "$src" "$CACHE/musl-handle-aux-at_base.patch"
    apply_p1 "$src" "$CACHE/musl-0001-add-stub-for-pthread_mutexattr_setprioceiling.patch"
    apply_p1 "$src" "$CACHE/musl-fix-loongarch64-zero-len-extcontext.patch"
    apply_p1 "$src" "$CACHE/musl-iconv-gb18030-fix.patch"
    apply_p1 "$src" "$CACHE/musl-CVE-2026-40200.patch"
    (
        cd "$src"
        CC="$GCC" AR="$AR" RANLIB="$RANLIB" \
            CFLAGS="$COMMON_CFLAGS" \
            ./configure --target=loongarch64 --prefix=/usr --syslibdir=/lib --enable-debug
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install
    )
    mkdir -p "$SYSROOT/lib" "$SYSROOT/usr/lib"
    if [[ -f "$SYSROOT/usr/lib/libc.so" ]]; then
        mv "$SYSROOT/usr/lib/libc.so" "$SYSROOT/lib/ld-musl-loongarch64.so.1"
    fi
    ln -sfn ld-musl-loongarch64.so.1 "$SYSROOT/lib/libc.musl-loongarch64.so.1"
    ln -sfn ../../lib/ld-musl-loongarch64.so.1 "$SYSROOT/usr/lib/libc.so"
    mark_done musl
}

setup_musl_wrapper() {
    cat > "$WRAP/$TARGET-gcc" <<EOF
#!/usr/bin/env bash
exec "$GCC" --sysroot="$SYSROOT" "\$@"
EOF
    cat > "$WRAP/$TARGET-g++" <<EOF
#!/usr/bin/env bash
exec "$GXX" --sysroot="$SYSROOT" "\$@"
EOF
    chmod 0755 "$WRAP/$TARGET-gcc" "$WRAP/$TARGET-g++"
    CC=$WRAP/$TARGET-gcc
    CXX=$WRAP/$TARGET-g++
    export CC CXX AR RANLIB STRIP READELF
    export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
    export PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig"
}

configure_env() {
    env \
        CC="$CC" CXX="$CXX" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
        CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" CPPFLAGS="$COMMON_CPPFLAGS" \
        LDFLAGS="$COMMON_LDFLAGS" \
        PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
        PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig" \
        "$@"
}

build_zlib() {
    if is_done zlib; then return; fi
    local src=$BUILD/zlib
    unpack_tar "$CACHE/zlib-1.3.2.tar.gz" "$src"
    (
        cd "$src"
        CHOST=$TARGET CC="$CC" AR="$AR" RANLIB="$RANLIB" \
            CFLAGS="$COMMON_CFLAGS" LDFLAGS="$COMMON_LDFLAGS" \
            ./configure --prefix=/usr --shared --disable-crcvx
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install
    )
    mark_done zlib
}

build_bzip2() {
    if is_done bzip2; then return; fi
    local src=$BUILD/bzip2
    unpack_tar "$CACHE/bzip2-1.0.8.tar.gz" "$src"
    for p in \
        bzip2-1.0.4-makefile-CFLAGS.patch \
        bzip2-1.0.4-man-links.patch \
        bzip2-1.0.2-progress.patch \
        bzip2-1.0.3-no-test.patch \
        saneso.patch; do
        apply_p1 "$src" "$CACHE/bzip2-$p"
    done
    (
        cd "$src"
        make -f Makefile-libbz2_so -j"$JOBS" CC="$CC" AR="$AR" RANLIB="$RANLIB" \
            CFLAGS="$COMMON_CFLAGS -fPIC"
        make -j"$JOBS" CC="$CC" AR="$AR" RANLIB="$RANLIB" CFLAGS="$COMMON_CFLAGS"
        install -Dm644 bzlib.h "$SYSROOT/usr/include/bzlib.h"
        install -Dm644 libbz2.a "$SYSROOT/usr/lib/libbz2.a"
        install -Dm755 libbz2.so.1.0.8 "$SYSROOT/usr/lib/libbz2.so.1.0.8"
        ln -sfn libbz2.so.1.0.8 "$SYSROOT/usr/lib/libbz2.so.1.0"
        ln -sfn libbz2.so.1.0.8 "$SYSROOT/usr/lib/libbz2.so.1"
        ln -sfn libbz2.so.1.0.8 "$SYSROOT/usr/lib/libbz2.so"
    )
    mark_done bzip2
}

build_autoconf_package() {
    local name=$1 archive=$2
    shift 2
    if is_done "$name"; then return; fi
    local src=$BUILD/$name
    unpack_tar "$CACHE/$archive" "$src"
    (
        cd "$src"
        configure_env ./configure --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr "$@"
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install
    )
    mark_done "$name"
}

build_xz() {
    if is_done xz; then return; fi
    local src=$BUILD/xz
    unpack_tar "$CACHE/xz-5.8.3.tar.gz" "$src"
    (
        cd "$src"
        autoreconf -fi
        configure_env ./configure --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --disable-rpath --disable-werror --disable-doc
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install
    )
    mark_done xz
}

build_openssl() {
    if is_done openssl; then return; fi
    local src=$BUILD/openssl
    unpack_tar "$CACHE/openssl-3.5.7.tar.gz" "$src"
    apply_p1 "$src" "$CACHE/openssl-auxv.patch"
    (
        cd "$src"
        CC="$CC" AR="$AR" RANLIB="$RANLIB" \
            ./Configure linux64-loongarch64 shared no-tests no-docs \
            --prefix=/usr --libdir=lib --openssldir=/etc/ssl \
            $COMMON_CFLAGS $COMMON_LDFLAGS
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install_sw install_ssldirs
    )
    mark_done openssl
}

build_ncurses() {
    if is_done ncurses; then return; fi
    local src=$BUILD/ncurses
    unpack_tar "$CACHE/ncurses-6.6-20260516.tgz" "$src"
    apply_p1 "$src" "$CACHE/ncurses-cleanup-pkgconfig-ldflags.patch"
    (
        cd "$src"
        configure_env ./configure --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --with-shared --with-normal --without-debug --without-ada --without-tests \
            --enable-widec --with-termlib --with-ticlib --enable-pc-files \
            --with-pkg-config-libdir=/usr/lib/pkgconfig --disable-stripping \
            --with-build-cc=gcc
        make -j"$JOBS" libs
        # With --enable-pc-files, the ncurses install.libs target already
        # installs the generated pkg-config files.  The 6.6 snapshot has no
        # separate top-level install.pc target.
        make DESTDIR="$SYSROOT" install.libs install.includes
    )
    mark_done ncurses
}

build_readline() {
    if is_done readline; then return; fi
    local src=$BUILD/readline
    # readline.pc asks for the non-wide compatibility name.  Our deliberately
    # split wide ncurses build only installs ncursesw.pc, whose Libs field also
    # carries the required split tinfow dependency.
    ln -sfn ncursesw.pc "$SYSROOT/usr/lib/pkgconfig/ncurses.pc"
    unpack_tar "$CACHE/readline-8.3.tar.gz" "$src"
    # Alpine's local patch and the upstream readline83 patches are explicitly
    # authored for patch -p0 (the former targets shlib/Makefile.in).
    apply_p0 "$src" "$CACHE/readline-fix-ncurses-underlinking.patch"
    for p in readline83-001.patch readline83-002.patch readline83-003.patch; do
        apply_p0 "$src" "$CACHE/$p"
    done
    (
        cd "$src"
        configure_env ./configure --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --enable-static --enable-shared --with-curses
        # ncurses was built with --with-termlib, so termcap symbols live in
        # libtinfow rather than libncursesw.  Command-line make variables are
        # inherited by readline's shlib sub-make and must name both libraries.
        make -j"$JOBS" SHLIB_LIBS="-lncursesw -ltinfow"
        make DESTDIR="$SYSROOT" SHLIB_LIBS="-lncursesw -ltinfow" install
    )
    mark_done readline
}

build_sqlite() {
    if is_done sqlite; then return; fi
    local src=$BUILD/sqlite
    unpack_tar "$CACHE/sqlite-autoconf-3530300.tar.gz" "$src"
    (
        cd "$src"
        configure_env ./configure --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --enable-threadsafe --disable-readline --enable-session --enable-static \
            --enable-fts3 --enable-fts4 --enable-fts5 --soname=legacy
        make -j"$JOBS"
        make DESTDIR="$SYSROOT" install
    )
    mark_done sqlite
}

build_host_python() {
    if is_done host-python; then return; fi
    local src=$BUILD/python-host
    unpack_tar "$CACHE/Python-3.14.5.tar.xz" "$src"
    (
        cd "$src"
        # setup_musl_wrapper exports target tools for dependency builds.  The
        # build Python must be a native x86_64 executable used by CPython's
        # cross-build machinery, so isolate it from every target setting.
        unset CC CXX AR RANLIB STRIP READELF CFLAGS CXXFLAGS CPPFLAGS LDFLAGS
        unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR
        ./configure --prefix="$HOSTPY" --without-ensurepip
        make -j"$JOBS"
        make -j1 install
    )
    mark_done host-python
}

build_target_python() {
    if is_done python-target; then return; fi
    local src=$BUILD/python-target
    if [[ ${CPYTHON_STRICT_RESUME_PGO:-0} == 1 ]]; then
        [[ -f "$src/profile-gen-stamp" ]] || {
            echo "cannot resume PGO: profile-gen-stamp is missing" >&2
            exit 1
        }
        find "$src" -name '*.gcda' -print -quit | grep . >/dev/null || {
            echo "cannot resume PGO: no GCC profile data exists" >&2
            exit 1
        }
        log "resuming a separately verified completed PGO training run"
        (
            cd "$src"
            make clean-retain-profile
            touch profile-run-stamp
            make -j"$PYTHON_JOBS"
            make -j1 DESTDIR="$RUNTIME" EXTRA_CFLAGS="$COMMON_CFLAGS" install
        )
        mark_done python-target
        return
    fi
    unpack_tar "$CACHE/Python-3.14.5.tar.xz" "$src"
    apply_p1 "$src" "$CACHE/python3-musl-find_library.patch"
    rm -rf "$src/Modules/expat"

    # CPython does not route its PGO profile command through HOSTRUNNER for
    # normal cross builds.  Keep the upstream build otherwise intact and use
    # the configured runner only for the target profile binary.
    sed -i \
        's|$(LLVM_PROF_FILE) $(RUNSHARED) ./$(BUILDPYTHON) $(PROFILE_TASK)|$(LLVM_PROF_FILE) $(HOSTRUNNER) ./$(BUILDPYTHON) $(PROFILE_TASK)|' \
        "$src/Makefile.pre.in"
    python3 - "$src/Makefile.pre.in" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
old = "\t$(LLVM_PROF_FILE) $(HOSTRUNNER) ./$(BUILDPYTHON) $(PROFILE_TASK)\n"
new = (
    "\t-$(LLVM_PROF_FILE) $(HOSTRUNNER) ./$(BUILDPYTHON) $(PROFILE_TASK)\n"
    "\t@test -n \"$$(find . -name '*.gcda' -print -quit)\" || "
    "{ echo 'PGO training produced no GCC profile data' >&2; exit 1; }\n"
)
if text.count(old) != 1:
    raise SystemExit("unexpected CPython PGO recipe; refusing an unverified patch")
path.write_text(text.replace(old, new), encoding="utf-8")
PY
    # This crosstool-NG release drives GCC LTO correctly with -flto, but was
    # built without the separately loadable GNU ld plugin and rejects the
    # redundant -fuse-linker-plugin option selected by CPython for generic
    # GCC targets.  Keep LTO and fat LTO objects while dropping only that
    # unsupported transport switch.
    sed -i \
        's/-flto -fuse-linker-plugin -ffat-lto-objects/-flto -ffat-lto-objects/g' \
        "$src/configure"

    local qemu_runner="$QEMU -L $SYSROOT -E LD_LIBRARY_PATH=$src:$SYSROOT/usr/lib:$SYSROOT/lib"
    local py_cflags="$COMMON_CFLAGS -O2 -DTHREAD_STACK_SIZE=0x200000"
    local py_ldflags="$COMMON_LDFLAGS -Wl,-z,stack-size=0x200000"
    (
        cd "$src"
        HOSTRUNNER="$qemu_runner" \
        CC="$CC" CXX="$CXX" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" READELF="$READELF" \
        CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" \
        CFLAGS_NODIST="$py_cflags" CXXFLAGS_NODIST="$COMMON_CFLAGS -O2" \
        CPPFLAGS="$COMMON_CPPFLAGS" LDFLAGS="$COMMON_LDFLAGS" LDFLAGS_NODIST="$py_ldflags" \
        PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
        PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig" \
        ac_cv_file__dev_ptmx=yes ac_cv_file__dev_ptc=no \
        ac_cv_aligned_required=yes ac_cv_pthread_is_default=yes \
        ac_cv_kpthread=no ac_cv_kthread=no ac_cv_pthread=no \
        ac_cv_cxx_thread=yes \
        ./configure \
            --build="$BUILD_TRIPLE" --host="$TARGET" \
            --with-build-python="$HOSTPY/bin/python3.14" \
            --prefix=/usr \
            --enable-ipv6 \
            --enable-loadable-sqlite-extensions \
            --enable-optimizations \
            --enable-shared \
            --with-lto \
            --with-computed-gotos \
            --with-system-expat \
            --with-system-libmpdec \
            --without-ensurepip
        # Several concurrent GCC 15.2 CPython LTO links can exhaust the build
        # container and trigger an lto1 ICE.  Keep only this PGO/LTO phase
        # serial; dependency builds still use JOBS.
        make -j"$PYTHON_JOBS"
        make -j1 DESTDIR="$RUNTIME" EXTRA_CFLAGS="$COMMON_CFLAGS" install
    )
    mark_done python-target
}

copy_runtime_library() {
    local pattern=$1 matched=0 directory file
    shopt -s nullglob
    for directory in "$SYSROOT/usr/lib" "$SYSROOT/usr/lib64"; do
        for file in "$directory"/$pattern; do
            cp -a "$file" "$RUNTIME/usr/lib/"
            matched=1
        done
    done
    shopt -u nullglob
    if [[ $matched -eq 0 ]]; then
        echo "missing runtime library pattern: $pattern" >&2
        exit 1
    fi
}

package_runtime() {
    if is_done runtime-package; then return; fi
    mkdir -p "$RUNTIME/lib" "$RUNTIME/usr/lib" "$RUNTIME/etc"
    cp -a "$SYSROOT/lib/ld-musl-loongarch64.so.1" "$RUNTIME/lib/"
    ln -sfn ld-musl-loongarch64.so.1 "$RUNTIME/lib/libc.musl-loongarch64.so.1"
    for pattern in \
        'libz.so*' 'libbz2.so*' 'liblzma.so*' 'libffi.so*' 'libexpat.so*' \
        'libmpdec.so*' 'libcrypto.so*' 'libssl.so*' 'libncursesw.so*' \
        'libtinfow.so*' 'libpanelw.so*' 'libreadline.so*' 'libhistory.so*' \
        'libsqlite3.so*'; do
        copy_runtime_library "$pattern"
    done
    if [[ -d "$SYSROOT/usr/lib/ossl-modules" ]]; then
        cp -a "$SYSROOT/usr/lib/ossl-modules" "$RUNTIME/usr/lib/"
    fi
    if [[ -d "$SYSROOT/etc/ssl" ]]; then
        cp -a "$SYSROOT/etc/ssl" "$RUNTIME/etc/"
    fi
    if [[ -d "$ROOT/user/tools/loongarch64/tests/cpython/etc/ssl" ]]; then
        cp -a "$ROOT/user/tools/loongarch64/tests/cpython/etc/ssl/." "$RUNTIME/etc/ssl/"
    fi
    if [[ -d "$ROOT/user/tools/loongarch64/tests/cpython/etc/terminfo" ]]; then
        cp -a "$ROOT/user/tools/loongarch64/tests/cpython/etc/terminfo" "$RUNTIME/etc/"
    fi
    install -m 0755 "$ROOT/user/tools/cpython/run_cpython.sh" "$RUNTIME/run_cpython.sh"
    install -m 0755 "$ROOT/user/tools/cpython/python3-wrapper.sh" "$RUNTIME/python3-wrapper.sh"
    install -m 0755 "$ROOT/user/tools/cpython/cpython_testcode.sh" "$RUNTIME/cpython_testcode.sh"
    install -m 0755 "$ROOT/user/tools/cpython/run_strict_benchmark.sh" "$RUNTIME/run_strict_benchmark.sh"
    install -m 0755 "$ROOT/user/tools/cpython/run_strict_functional.sh" "$RUNTIME/run_strict_functional.sh"
    install -m 0755 "$ROOT/user/tools/cpython/strict_runtime_smoke.sh" "$RUNTIME/strict_runtime_smoke.sh"
    install -m 0755 "$ROOT/user/tools/cpython/L3_check_files.sh" "$RUNTIME/L3_check_files.sh"
    install -m 0755 "$ROOT/user/tools/cpython/L4_startup.sh" "$RUNTIME/L4_startup.sh"
    install -m 0644 "$ROOT/user/tools/cpython/L5_language.py" "$RUNTIME/L5_language.py"
    install -m 0644 "$ROOT/user/tools/cpython/L6_stdlib.py" "$RUNTIME/L6_stdlib.py"
    install -m 0644 "$ROOT/user/tools/cpython/L7_filesystem.py" "$RUNTIME/L7_filesystem.py"
    install -m 0644 "$ROOT/user/tools/cpython/L8_thread.py" "$RUNTIME/L8_thread.py"
    install -m 0644 "$ROOT/user/tools/cpython/L8_subprocess.py" "$RUNTIME/L8_subprocess.py"
    install -m 0644 "$ROOT/user/tools/cpython/L9_socket.py" "$RUNTIME/L9_socket.py"

    python3 - "$RUNTIME" "$STRIP" <<'PY'
import pathlib
import subprocess
import sys

runtime = pathlib.Path(sys.argv[1])
strip = sys.argv[2]
elfs = []
for path in sorted(runtime.rglob("*")):
    if not path.is_file() or path.is_symlink():
        continue
    with path.open("rb") as stream:
        if stream.read(4) == b"\x7fELF":
            elfs.append(str(path))
for offset in range(0, len(elfs), 64):
    subprocess.run([strip, "--strip-unneeded", *elfs[offset:offset + 64]], check=True)
print(f"stripped_elfs={len(elfs)}")
PY

    local sysconfig
    sysconfig=$(find "$RUNTIME/usr/lib" -name '_sysconfigdata_*.py' -print -quit)
    [[ -n "$sysconfig" ]] || { echo "missing _sysconfigdata" >&2; exit 1; }
    grep -q -- '-mstrict-align' "$sysconfig" || {
        echo "strict flag missing from installed sysconfig: $sysconfig" >&2
        exit 1
    }

    "$QEMU" -L "$RUNTIME" \
        -E "LD_LIBRARY_PATH=$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" -S -c \
        'import _bz2,_ctypes,_decimal,_hashlib,_lzma,_sqlite3,readline,ssl,sysconfig,threading,zlib; flags=" ".join(str(sysconfig.get_config_var(k) or "") for k in ("CFLAGS","CONFIGURE_CFLAGS","CONFIGURE_CFLAGS_NODIST","PY_CFLAGS","PGO_PROF_USE_FLAG")); assert "-mstrict-align" in flags; assert "-fprofile-use" in flags; args=sysconfig.get_config_var("CONFIG_ARGS") or ""; assert "--enable-optimizations" in args and "--with-lto" in args; t=threading.Thread(target=lambda:None); t.start(); t.join(); print("strict-runtime-smoke-ok")'

    python3 - "$RUNTIME" "$OUT" "$GCC" "$READELF" "$STRICT_FLAGS" "$ROOT" <<'PY'
import hashlib
import json
import os
import pathlib
import subprocess
import sys

runtime = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])
gcc = sys.argv[3]
readelf = sys.argv[4]
strict_flags = sys.argv[5]
root = pathlib.Path(sys.argv[6])

def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

elfs = []
# musl's interpreter is also libc and satisfies DT_NEEDED libc.so without a
# second on-disk DSO (the same layout is used by the existing board runtime).
provided = {"libc.so"}
for path in sorted(runtime.rglob("*")):
    if not path.is_file() or path.is_symlink():
        continue
    with path.open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            continue
    header = subprocess.run(
        [readelf, "-h", str(path)], text=True, capture_output=True, check=False
    )
    if header.returncode:
        continue
    if "Machine:                           LoongArch" not in header.stdout:
        raise SystemExit(f"non-LoongArch ELF in runtime: {path}")
    dynamic = subprocess.run(
        [readelf, "-d", str(path)], text=True, capture_output=True, check=False
    ).stdout
    needed = []
    soname = None
    for line in dynamic.splitlines():
        if "(NEEDED)" in line:
            needed.append(line.rsplit("[", 1)[1].split("]", 1)[0])
        elif "(SONAME)" in line:
            soname = line.rsplit("[", 1)[1].split("]", 1)[0]
    provided.add(path.name)
    if soname:
        provided.add(soname)
    elfs.append({
        "path": str(path.relative_to(runtime)),
        "sha256": sha256(path),
        "needed": needed,
        "soname": soname,
    })

missing = sorted({name for elf in elfs for name in elf["needed"] if name not in provided})
if missing:
    raise SystemExit("unresolved runtime DT_NEEDED entries: " + ", ".join(missing))

sources = []
for path in sorted((out / "cache").iterdir()):
    if path.is_file():
        sources.append({"name": path.name, "sha256": sha256(path), "size": path.stat().st_size})

profile_files = sorted((out / "build" / "python-target").rglob("*.gcda"))
if not profile_files:
    raise SystemExit("PGO manifest validation found no GCC profile data")
handler_source = root / "os" / "src" / "hal" / "arch" / "loongarch64" / "trap" / "mod.rs"

manifest = {
    "schema": 2,
    "runtime_policy": "mangocore-la64-strict-align-v1",
    "native_closure_policy": "CPython, musl loader/libc and every packaged native dependency use -mstrict-align",
    "target": "loongarch64-linux-musl",
    "python_version": "3.14.5",
    "compiler": subprocess.check_output([gcc, "--version"], text=True).splitlines()[0],
    "toolchain_release": "loong64/cross-tools 20250911",
    "strict_flags": strict_flags,
    "pgo": True,
    "pgo_profile_file_count": len(profile_files),
    "pgo_training_policy": "upstream --pgo under QEMU; nonzero is accepted only when GCC profile data exists",
    "lto": True,
    "kernel_handler_modified": False,
    "kernel_handler_source_sha256": sha256(handler_source),
    "build_script_sha256": sha256(root / "scripts" / "build_cpython_runtime_la64_strict.sh"),
    "elf_count": len(elfs),
    "elfs": elfs,
    "virtual_dso_providers": {"libc.so": "lib/ld-musl-loongarch64.so.1"},
    "sources": sources,
    "aports_commits": {
        "musl": "b0c8ea10e8f29cabe336b2e5d864124940e126ab",
        "zlib": "f248b33b5943c7dc69bf691031d7612ab2e8ed93",
        "bzip2": "33283848034c9885d984c8e8697c645c57324938",
        "xz": "ce9944f1daadb681dd6f0f81a06e9cce97127377",
        "libffi": "2c2a7bb4a8b16066834e90402567b2c19403a790",
        "expat": "acbd3e4e0d1525f98da93aa9f7cf733aff995736",
        "sqlite": "e716785aa8fead790466477c2e00cad115a5383b",
        "openssl": "35c0d1f2b314f647008595f681786813760da191",
        "ncurses": "2cee8a7328d061418336ad327b512d96bcd7bc5e",
        "readline": "a854c03acdac188901fb012f7acbee70a36e8041",
        "mpdecimal": "c0519b85456e9b838e3142e1d233aab0b433b476",
        "python3": "0266eb2db4c93e7ee1d51e9e50f5baacbc15303c",
    },
}
(runtime / "strict-runtime-manifest.json").write_text(
    json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
)
PY

    local tmp_archive=$ARTIFACTS/cpython-la64-strict-3.14.5.tar.xz
    # Do not archive the synthetic root member or retain leading "./" in
    # member names.  MangoCore's current ext4/VFS path rejects BusyBox tar's
    # attempt to recreate the explicit "./" directory, even though ordinary
    # normalized child paths are valid.  --no-recursion avoids duplicates
    # because the sorted input already contains every directory and file.
    (
        cd "$RUNTIME"
        find . -mindepth 1 -print0 | LC_ALL=C sort -z | \
            tar --null --no-recursion --transform='s|^\./||' \
                --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 \
                --numeric-owner -cJf "$tmp_archive" -T -
    )
    local archive_sha
    archive_sha=$(sha256sum "$tmp_archive" | awk '{print $1}')
    local final_archive=$ARTIFACTS/cpython-la64-strict-3.14.5-${archive_sha:0:12}.tar.xz
    mv "$tmp_archive" "$final_archive"
    printf '%s  %s\n' "$archive_sha" "$(basename "$final_archive")" > "$final_archive.sha256"
    log "artifact: $final_archive"
    log "sha256: $archive_sha"
    mark_done runtime-package
}

write_current_artifact_index() {
    python3 - "$ARTIFACTS" <<'PY'
import hashlib
import json
import pathlib
import tarfile
import sys

artifacts = pathlib.Path(sys.argv[1])
required_flags = {"-march=loongarch64", "-mabi=lp64d", "-mstrict-align"}

def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

valid = []
for archive in artifacts.glob("cpython-la64-strict-*.tar.xz"):
    sidecar = pathlib.Path(str(archive) + ".sha256")
    if not sidecar.is_file():
        continue
    expected = sidecar.read_text(encoding="utf-8").split()[0]
    actual = sha256(archive)
    if expected != actual:
        continue
    try:
        with tarfile.open(archive, "r:xz") as tar:
            member = tar.extractfile("strict-runtime-manifest.json")
            if member is None:
                continue
            manifest_bytes = member.read()
            manifest = json.loads(manifest_bytes)
    except (KeyError, OSError, tarfile.TarError, json.JSONDecodeError):
        continue
    flags = set(str(manifest.get("strict_flags", "")).split())
    if (
        manifest.get("target") != "loongarch64-linux-musl"
        or not required_flags.issubset(flags)
        or manifest.get("pgo") is not True
        or manifest.get("lto") is not True
        or not manifest.get("elfs")
    ):
        continue
    manifest_digest = hashlib.sha256(manifest_bytes).hexdigest()
    valid.append((archive.stat().st_mtime_ns, archive, actual, manifest_digest, manifest))

if not valid:
    raise SystemExit("no verified strict-aligned LoongArch runtime artifact")
_, archive, digest, manifest_digest, manifest = max(
    valid, key=lambda item: (item[0], item[1].name)
)
index = {
    "schema": 1,
    "runtime_policy": "mangocore-la64-strict-align-v1",
    "artifact": archive.name,
    "sha256": digest,
    "manifest_sha256": manifest_digest,
    "manifest_schema": manifest.get("schema", 1),
}
temporary = artifacts / ".current.json.tmp"
temporary.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n", encoding="utf-8")
temporary.replace(artifacts / "current.json")
print("current_artifact=" + archive.name)
print("current_sha256=" + digest)
PY
}

main() {
    fetch_sources
    setup_toolchain
    install_kernel_headers
    build_musl
    setup_musl_wrapper
    build_zlib
    build_bzip2
    build_xz
    build_autoconf_package libffi libffi-3.5.2.tar.gz \
        --enable-pax_emutramp --enable-portable-binary --disable-exec-static-tramp
    build_autoconf_package expat expat-2.8.2.tar.xz --enable-static
    build_autoconf_package mpdecimal mpdecimal-4.0.1.tar.gz --enable-shared
    build_openssl
    build_ncurses
    build_readline
    build_sqlite
    build_host_python
    build_target_python
    package_runtime
    write_current_artifact_index
}

main "$@"
