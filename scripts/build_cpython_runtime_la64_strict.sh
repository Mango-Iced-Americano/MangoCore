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
HOST_TOOLS=$OUT/host-tools
PILLOW_BUILD_DEPS=$OUT/pillow-build-deps
RUSTUP_HOME=$HOST_TOOLS/rustup
CARGO_HOME=$HOST_TOOLS/cargo

mkdir -p \
    "$CACHE" "$SOURCES" "$BUILD" "$STAMPS" "$SYSROOT" "$ARTIFACTS" \
    "$WRAP" "$HOST_TOOLS" "$PILLOW_BUILD_DEPS"

TOOLCHAIN_ARCHIVE=x86_64-cross-tools-loongarch64-unknown-linux-musl-latest-20250911.tar.xz
TOOLCHAIN_URL=https://github.com/loong64/cross-tools/releases/download/20250911/x86_64-cross-tools-loongarch64-unknown-linux-musl-latest.tar.xz
TOOLCHAIN_SHA256=2d56d07146ed712ac44f5063f54a19656fce851492c3f1c10e31e6b6633db6d7
TOOLCHAIN_ROOT=$OUT/toolchain/full-gcc-15.2.0-musl-20250911/loongarch64-unknown-linux-musl
TOOLCHAIN_PREFIX=loongarch64-unknown-linux-musl

QEMU_USER_ARCHIVE=qemu-user-static_8.2.2+ds-0ubuntu1.17_amd64.deb
QEMU_USER_URL=https://archive.ubuntu.com/ubuntu/pool/universe/q/qemu/qemu-user-static_8.2.2%2bds-0ubuntu1.17_amd64.deb
QEMU_USER_SHA256=4558164baf4250d4dcc0dcbcf114b44b4b77b5fed773267187e72cedae883fdc
QEMU_USER_ROOT=$HOST_TOOLS/qemu-user-static-8.2.2-u17
QEMU_USER_BIN=$QEMU_USER_ROOT/usr/bin/qemu-loongarch64-static

LIBJPEG_ARCHIVE=libjpeg-turbo-3.1.4.1.tar.gz
LIBJPEG_URL=https://github.com/libjpeg-turbo/libjpeg-turbo/releases/download/3.1.4.1/libjpeg-turbo-3.1.4.1.tar.gz
LIBJPEG_SHA256=ecae8008e2cc9ade2f2c1bb9d5e6d4fb73e7c433866a056bd82980741571a022

PILLOW_ARCHIVE=pillow-12.3.0.tar.gz
PILLOW_URL=https://files.pythonhosted.org/packages/source/p/pillow/pillow-12.3.0.tar.gz
PILLOW_SHA256=3b8182a766685eaa002637e28b4ec8d6b18819a0c71f579bf0dbaa5830297cce
MARKUPSAFE_ARCHIVE=markupsafe-3.0.3.tar.gz
MARKUPSAFE_URL=https://files.pythonhosted.org/packages/source/m/markupsafe/markupsafe-3.0.3.tar.gz
MARKUPSAFE_SHA256=722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698
PYYAML_ARCHIVE=pyyaml-6.0.3.tar.gz
PYYAML_URL=https://files.pythonhosted.org/packages/05/8e/961c0007c59b8dd7729d542c61a4d537767a59645b82a0b521206e1e25c2/pyyaml-6.0.3.tar.gz
PYYAML_SHA256=d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f
LIBXML2_ARCHIVE=libxml2-2.14.6.tar.xz
LIBXML2_URL=https://download.gnome.org/sources/libxml2/2.14/libxml2-2.14.6.tar.xz
LIBXML2_SHA256=7ce458a0affeb83f0b55f1f4f9e0e55735dbfc1a9de124ee86fb4a66b597203a
LIBXSLT_ARCHIVE=libxslt-1.1.43.tar.xz
LIBXSLT_URL=https://download.gnome.org/sources/libxslt/1.1/libxslt-1.1.43.tar.xz
LIBXSLT_SHA256=5a3d6b383ca5afc235b171118e90f5ff6aa27e9fea3303065231a6d403f0183a
LXML_ARCHIVE=lxml-6.1.1.tar.gz
LXML_URL=https://files.pythonhosted.org/packages/05/3b/aab6728cae887456f409b4d75e8a01856e4f04bd510de38052a47768b680/lxml-6.1.1.tar.gz
LXML_SHA256=ba96ae44888e0185281e937633a743ea90d5a196c6000f82565ebb0580012d40
PRIMP_ARCHIVE=primp-0.15.0.tar.gz
PRIMP_URL=https://files.pythonhosted.org/packages/56/0b/a87556189da4de1fc6360ca1aa05e8335509633f836cdd06dd17f0743300/primp-0.15.0.tar.gz
PRIMP_SHA256=1af8ea4b15f57571ff7fc5e282a82c5eb69bc695e19b8ddeeda324397965b30a
RUSTUP_INIT=rustup-init-1.28.2-x86_64-unknown-linux-gnu
RUSTUP_INIT_URL=https://static.rust-lang.org/rustup/archive/1.28.2/x86_64-unknown-linux-gnu/rustup-init
RUSTUP_INIT_SHA256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c
MATURIN_WHEEL=maturin-1.8.3-py3-none-manylinux_2_12_x86_64.manylinux2010_x86_64.musllinux_1_1_x86_64.whl
MATURIN_URL=https://files.pythonhosted.org/packages/2e/6d/bf1b8bb9a8b1d9adad242b4089794be318446142975762d04f04ffabae40/maturin-1.8.3-py3-none-manylinux_2_12_x86_64.manylinux2010_x86_64.musllinux_1_1_x86_64.whl
MATURIN_SHA256=11564fac7486313b7baf3aa4e82c20e1b20364aad3fde2ccbc4c07693c0b7e16
DDGS_WHEEL=ddgs-9.0.0-py3-none-any.whl
DDGS_URL=https://files.pythonhosted.org/packages/e5/05/bd3ed9a28212b313f5678533152c4d79fbc386e44245ca5eed426d75f019/ddgs-9.0.0-py3-none-any.whl
DDGS_SHA256=5dd11d666d6caf1cfdbd341579637bb670c4b2f41df5413b76705519d8e7a22c
MARKDOWNIFY_WHEEL=markdownify-0.14.1-py3-none-any.whl
MARKDOWNIFY_URL=https://files.pythonhosted.org/packages/65/0b/74cec93a7b05edf4fc3ea1c899fe8a37f041d7b9d303c75abf7a162924e0/markdownify-0.14.1-py3-none-any.whl
MARKDOWNIFY_SHA256=4c46a6c0c12c6005ddcd49b45a5a890398b002ef51380cd319db62df5e09bc2a
BEAUTIFULSOUP4_WHEEL=beautifulsoup4-4.12.3-py3-none-any.whl
BEAUTIFULSOUP4_URL=https://files.pythonhosted.org/packages/b1/fe/e8c672695b37eecc5cbf43e1d0638d88d66ba3a44c4d321c796f4e59167f/beautifulsoup4-4.12.3-py3-none-any.whl
BEAUTIFULSOUP4_SHA256=b80878c9f40111313e55da8ba20bdba06d8fa3969fc68304167741bbf9e082ed
SOUPSIEVE_WHEEL=soupsieve-2.6-py3-none-any.whl
SOUPSIEVE_URL=https://files.pythonhosted.org/packages/d1/c2/fe97d779f3ef3b15f05c94a2f1e3d21732574ed441687474db9d342a7315/soupsieve-2.6-py3-none-any.whl
SOUPSIEVE_SHA256=e72c4ff06e4fb6e4b5a9f0f55fe6e81514581fca1515028625d0f299c602ccc9
SIX_WHEEL=six-1.17.0-py2.py3-none-any.whl
SIX_URL=https://files.pythonhosted.org/packages/b7/ce/149a00dd41f10bc29e5921b496af8b574d8413afcd5e30dfa0ed46c2cc5e/six-1.17.0-py2.py3-none-any.whl
SIX_SHA256=4721f391ed90541fddacab5acf947aa0d3dc7d27b2e1e8eda2be8970586c3274
CLICK_WHEEL=click-8.1.8-py3-none-any.whl
CLICK_URL=https://files.pythonhosted.org/packages/7e/d4/7ebdbd03970677812aac39c869717059dbb71a4cfc033ca6e5221787892c/click-8.1.8-py3-none-any.whl
CLICK_SHA256=63c132bbbed01578a06712a2d1f497bb62d9c1c0d329b7903a866228027263b2
SETUPTOOLS_WHEEL=setuptools-80.9.0-py3-none-any.whl
SETUPTOOLS_URL=https://files.pythonhosted.org/packages/a3/dc/17031897dae0efacfea57dfd3a82fdd2a2aeb58e0ff71b77b87e44edc772/setuptools-80.9.0-py3-none-any.whl
SETUPTOOLS_SHA256=062d34222ad13e0cc312a4c02d73f059e86a4acbfbdea8f8f76b28c99f306922
PYBIND11_WHEEL=pybind11-3.0.1-py3-none-any.whl
PYBIND11_URL=https://files.pythonhosted.org/packages/cd/8a/37362fc2b949d5f733a8b0f2ff51ba423914cabefe69f1d1b6aab710f5fe/pybind11-3.0.1-py3-none-any.whl
PYBIND11_SHA256=aa8f0aa6e0a94d3b64adfc38f560f33f15e589be2175e103c0a33c6bce55ee89
WHEEL_WHEEL=wheel-0.45.1-py3-none-any.whl
WHEEL_URL=https://files.pythonhosted.org/packages/0b/2c/87f3254fd8ffd29e4c02732eee68a83a1d3c346ae39bc6822dcbcb697f2b/wheel-0.45.1-py3-none-any.whl
WHEEL_SHA256=708e7481cc80179af0e556bbf0cc00b8444c7321e2700b8d8580231d13017248

TARGET=loongarch64-linux-musl
BUILD_TRIPLE=x86_64-pc-linux-gnu
STRICT_FLAGS="-march=loongarch64 -mabi=lp64d -mstrict-align"
COMMON_CFLAGS="-Os -fstack-clash-protection -Wformat -Werror=format-security -fno-plt $STRICT_FLAGS"
COMMON_CPPFLAGS="-I$SYSROOT/usr/include"
COMMON_LDFLAGS="-L$SYSROOT/usr/lib -L$SYSROOT/lib -Wl,-rpath-link,$SYSROOT/usr/lib -Wl,-rpath-link,$SYSROOT/lib -Wl,--as-needed -Wl,-O1 -Wl,--sort-common"
RUNTIME_INTERP=/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1

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

package_input_digest() {
    {
        sha256sum "$ROOT/scripts/build_cpython_runtime_la64_strict.sh"
        find "$ROOT/user/tools/cpython" -type f -print0 | \
            LC_ALL=C sort -z | xargs -0 sha256sum
    } | sha256sum | awk '{print $1}'
}

package_cache_current() {
    local stamp=$STAMPS/runtime-package.inputs.sha256
    is_done runtime-package && [[ -f "$stamp" ]] && \
        [[ $(cat "$stamp") == $(package_input_digest) ]]
}

fetch_sources() {
    log "fetching and verifying pinned sources"
    fetch_sha256 "$TOOLCHAIN_URL" "$TOOLCHAIN_ARCHIVE" "$TOOLCHAIN_SHA256"
    fetch_sha256 "$QEMU_USER_URL" "$QEMU_USER_ARCHIVE" "$QEMU_USER_SHA256"
    fetch_sha256 "$LIBJPEG_URL" "$LIBJPEG_ARCHIVE" "$LIBJPEG_SHA256"
    fetch_sha256 "$PILLOW_URL" "$PILLOW_ARCHIVE" "$PILLOW_SHA256"
    fetch_sha256 "$MARKUPSAFE_URL" "$MARKUPSAFE_ARCHIVE" "$MARKUPSAFE_SHA256"
    fetch_sha256 "$PYYAML_URL" "$PYYAML_ARCHIVE" "$PYYAML_SHA256"
    fetch_sha256 "$LIBXML2_URL" "$LIBXML2_ARCHIVE" "$LIBXML2_SHA256"
    fetch_sha256 "$LIBXSLT_URL" "$LIBXSLT_ARCHIVE" "$LIBXSLT_SHA256"
    fetch_sha256 "$LXML_URL" "$LXML_ARCHIVE" "$LXML_SHA256"
    fetch_sha256 "$PRIMP_URL" "$PRIMP_ARCHIVE" "$PRIMP_SHA256"
    fetch_sha256 "$RUSTUP_INIT_URL" "$RUSTUP_INIT" "$RUSTUP_INIT_SHA256"
    fetch_sha256 "$MATURIN_URL" "$MATURIN_WHEEL" "$MATURIN_SHA256"
    fetch_sha256 "$DDGS_URL" "$DDGS_WHEEL" "$DDGS_SHA256"
    fetch_sha256 "$MARKDOWNIFY_URL" "$MARKDOWNIFY_WHEEL" "$MARKDOWNIFY_SHA256"
    fetch_sha256 "$BEAUTIFULSOUP4_URL" "$BEAUTIFULSOUP4_WHEEL" "$BEAUTIFULSOUP4_SHA256"
    fetch_sha256 "$SOUPSIEVE_URL" "$SOUPSIEVE_WHEEL" "$SOUPSIEVE_SHA256"
    fetch_sha256 "$SIX_URL" "$SIX_WHEEL" "$SIX_SHA256"
    fetch_sha256 "$CLICK_URL" "$CLICK_WHEEL" "$CLICK_SHA256"
    fetch_sha256 "$SETUPTOOLS_URL" "$SETUPTOOLS_WHEEL" "$SETUPTOOLS_SHA256"
    fetch_sha256 "$PYBIND11_URL" "$PYBIND11_WHEEL" "$PYBIND11_SHA256"
    fetch_sha256 "$WHEEL_URL" "$WHEEL_WHEEL" "$WHEEL_SHA256"
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

setup_qemu_user() {
    if [[ -x "$QEMU_USER_BIN" ]]; then return; fi
    rm -rf -- "$QEMU_USER_ROOT"
    mkdir -p "$QEMU_USER_ROOT"
    dpkg-deb -x "$CACHE/$QEMU_USER_ARCHIVE" "$QEMU_USER_ROOT"
    [[ -x "$QEMU_USER_BIN" ]] || {
        echo "bundled qemu-loongarch64-static is missing after extraction" >&2
        exit 1
    }
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
    PATCHELF=$(command -v patchelf || true)
    if [[ -z "$PATCHELF" ]]; then
        echo "missing patchelf; required to bind Python self-exec to the P4 loader" >&2
        exit 1
    fi
    QEMU=${CPYTHON_STRICT_QEMU:-}
    if [[ -z "$QEMU" ]]; then
        QEMU=$(command -v qemu-loongarch64-static || command -v qemu-loongarch64 || true)
    fi
    if [[ -z "$QEMU" ]]; then
        QEMU=$QEMU_USER_BIN
    fi
    [[ -x "$QEMU" ]] || {
        echo "missing qemu-loongarch64-static: $QEMU" >&2
        exit 1
    }
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
exec "$GCC" --sysroot="$SYSROOT" "\$@" $STRICT_FLAGS
EOF
    cat > "$WRAP/$TARGET-g++" <<EOF
#!/usr/bin/env bash
exec "$GXX" --sysroot="$SYSROOT" "\$@" $STRICT_FLAGS
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

build_libjpeg_turbo() {
    local stamp=libjpeg-turbo-3.1.4.1
    if is_done "$stamp"; then return; fi
    local src=$BUILD/libjpeg-turbo-3.1.4.1
    local build_dir=$BUILD/libjpeg-turbo-3.1.4.1-build
    unpack_tar "$CACHE/$LIBJPEG_ARCHIVE" "$src"
    rm -rf -- "$build_dir"
    mkdir -p "$build_dir"
    SOURCE_DATE_EPOCH=0 cmake -S "$src" -B "$build_dir" \
        -DCMAKE_SYSTEM_NAME=Linux \
        -DCMAKE_SYSTEM_PROCESSOR=loongarch64 \
        -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY \
        -DCMAKE_C_COMPILER="$CC" \
        -DCMAKE_INSTALL_PREFIX=/usr \
        -DCMAKE_INSTALL_LIBDIR=lib \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_C_FLAGS="$COMMON_CFLAGS" \
        -DCMAKE_SHARED_LINKER_FLAGS="$COMMON_LDFLAGS" \
        -DCMAKE_SKIP_RPATH=TRUE \
        -DCMAKE_EXPORT_COMPILE_COMMANDS=ON \
        -DENABLE_SHARED=TRUE \
        -DENABLE_STATIC=FALSE \
        -DWITH_SIMD=FALSE \
        -DWITH_TURBOJPEG=FALSE \
        -DWITH_TOOLS=FALSE \
        -DWITH_JAVA=FALSE \
        -DWITH_TESTS=FALSE
    SOURCE_DATE_EPOCH=0 cmake --build "$build_dir" --parallel "$JOBS"
    SOURCE_DATE_EPOCH=0 DESTDIR="$SYSROOT" cmake --install "$build_dir"
    python3 - "$build_dir/compile_commands.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
commands = json.loads(path.read_text(encoding="utf-8"))
if not commands:
    raise SystemExit("libjpeg-turbo compile database is empty")
missing = [entry["file"] for entry in commands if "-mstrict-align" not in entry["command"].split()]
if missing:
    raise SystemExit("libjpeg-turbo objects missing -mstrict-align: " + ", ".join(missing[:8]))
print(f"libjpeg_strict_compile_units={len(commands)}")
PY
    [[ -f "$SYSROOT/usr/lib/libjpeg.so.62.4.0" ]] || {
        echo "strict libjpeg shared library was not installed" >&2
        exit 1
    }
    mark_done "$stamp"
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

verify_strict_c_build_log() {
    local component=$1 build_log=$2 minimum_units=$3
    python3 - "$component" "$build_log" "$CC" "$minimum_units" <<'PY'
import pathlib
import sys

component, log_path, compiler, minimum = sys.argv[1:]
lines = pathlib.Path(log_path).read_text(encoding="utf-8", errors="replace").splitlines()
compile_lines = [
    line for line in lines
    if compiler + " " in line and " -c " in line
]
if len(compile_lines) < int(minimum):
    raise SystemExit(
        f"unexpectedly small {component} native build: {len(compile_lines)} compile units"
    )
missing = [line for line in compile_lines if "-mstrict-align" not in line.split()]
if missing:
    raise SystemExit(f"{component} compile unit missing -mstrict-align: {missing[0]}")
print(f"{component}_strict_compile_units={len(compile_lines)}")
PY
}

build_libxml2() {
    local stamp=libxml2-2.14.6-strict
    if is_done "$stamp"; then return; fi
    local src=$BUILD/libxml2-2.14.6
    local build_log=$BUILD/libxml2-2.14.6-build.log
    unpack_tar "$CACHE/$LIBXML2_ARCHIVE" "$src"
    (
        cd "$src"
        configure_env ./configure \
            --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --enable-shared --enable-static \
            --without-python --without-icu --without-readline --without-history \
            --without-http --without-modules --with-threads --with-zlib --with-lzma
        make V=1 -j"$JOBS" 2>&1 | tee "$build_log"
        make DESTDIR="$SYSROOT" install
    )
    verify_strict_c_build_log libxml2 "$build_log" 20
    [[ -f "$SYSROOT/usr/lib/libxml2.so.16" ]] || {
        echo "strict libxml2 shared library was not installed" >&2
        exit 1
    }
    mark_done "$stamp"
}

build_libxslt() {
    local stamp=libxslt-1.1.43-strict
    if is_done "$stamp"; then return; fi
    local src=$BUILD/libxslt-1.1.43
    local build_log=$BUILD/libxslt-1.1.43-build.log
    # Installed libtool archives retain target prefix paths such as
    # /usr/lib/liblzma.la.  When consumed from the cross sysroot, libtool
    # incorrectly resolves those paths against the Docker host.  The shared
    # objects and pkg-config metadata are canonical for this dynamic build.
    rm -f "$SYSROOT/usr/lib/libxml2.la" "$SYSROOT/usr/lib/liblzma.la"
    unpack_tar "$CACHE/$LIBXSLT_ARCHIVE" "$src"
    (
        cd "$src"
        env \
            CC="$CC" CXX="$CXX" AR="$AR" RANLIB="$RANLIB" STRIP="$STRIP" \
            CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" \
            CPPFLAGS="$COMMON_CPPFLAGS -I$SYSROOT/usr/include/libxml2" \
            LDFLAGS="$COMMON_LDFLAGS" \
            PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
            PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig" \
            XML2_CONFIG="$SYSROOT/usr/bin/xml2-config" \
            ./configure \
            --build="$BUILD_TRIPLE" --host="$TARGET" --prefix=/usr \
            --enable-shared --enable-static \
            --without-python --without-crypto --without-plugins \
            --with-libxml-prefix="$SYSROOT/usr"
        make V=1 -j"$JOBS" 2>&1 | tee "$build_log"
        make DESTDIR="$SYSROOT" install
    )
    verify_strict_c_build_log libxslt "$build_log" 10
    [[ -f "$SYSROOT/usr/lib/libxslt.so.1" && -f "$SYSROOT/usr/lib/libexslt.so.0" ]] || {
        echo "strict libxslt/libexslt shared libraries were not installed" >&2
        exit 1
    }
    mark_done "$stamp"
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

build_pillow() {
    local stamp=pillow-12.3.0
    if is_done "$stamp"; then return; fi
    local src=$BUILD/pillow-12.3.0
    local wheels=$BUILD/pillow-12.3.0-wheels
    local build_log=$BUILD/pillow-12.3.0-build.log
    local site_packages=$RUNTIME/usr/lib/python3.14/site-packages
    local target_include=$RUNTIME/usr/include/python3.14
    local target_sysconfig sysconfig_name wheel

    [[ -f "$target_include/Python.h" ]] || {
        echo "target Python headers are missing under $target_include" >&2
        exit 1
    }
    target_sysconfig=$(find "$RUNTIME/usr/lib/python3.14" -maxdepth 1 \
        -name '_sysconfigdata_*.py' -print -quit)
    [[ -n "$target_sysconfig" ]] || {
        echo "target Python sysconfig is missing" >&2
        exit 1
    }
    sysconfig_name=$(basename "$target_sysconfig" .py)

    unpack_tar "$CACHE/$PILLOW_ARCHIVE" "$src"
    rm -rf -- "$PILLOW_BUILD_DEPS" "$wheels"
    mkdir -p "$PILLOW_BUILD_DEPS" "$wheels" "$site_packages"
    for dependency_wheel in "$SETUPTOOLS_WHEEL" "$PYBIND11_WHEEL" "$WHEEL_WHEEL"; do
        python3 -m zipfile -e "$CACHE/$dependency_wheel" "$PILLOW_BUILD_DEPS"
    done

    # Run the native build backend with the matching host Python, while
    # forcing setuptools to consume the target CPython sysconfig.  The target
    # include directory is intentionally first; the host headers that
    # setuptools appends are only a fallback and must never win pyconfig.h.
    (
        cd "$src"
        SOURCE_DATE_EPOCH=0 \
        PYTHONPATH="$PILLOW_BUILD_DEPS:$RUNTIME/usr/lib/python3.14" \
        _PYTHON_SYSCONFIGDATA_NAME="$sysconfig_name" \
        _PYTHON_HOST_PLATFORM=linux-loongarch64 \
        CC="$CC" CXX="$CXX" LDSHARED="$CC -shared" \
        CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" \
        CPPFLAGS="-I$target_include $COMMON_CPPFLAGS" \
        LDFLAGS="$COMMON_LDFLAGS" \
        PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
        PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig" \
        MAX_CONCURRENCY=1 \
        "$HOSTPY/bin/python3.14" setup.py bdist_wheel --dist-dir "$wheels" \
            --pillow-configuration=platform-guessing=disable \
            --pillow-configuration=zlib=enable \
            --pillow-configuration=jpeg=enable \
            --pillow-configuration=tiff=disable \
            --pillow-configuration=freetype=disable \
            --pillow-configuration=raqm=disable \
            --pillow-configuration=lcms=disable \
            --pillow-configuration=webp=disable \
            --pillow-configuration=jpeg2000=disable \
            --pillow-configuration=imagequant=disable \
            --pillow-configuration=xcb=disable \
            --pillow-configuration=avif=disable \
            --pillow-configuration=parallel=1 \
            2>&1 | tee "$build_log"
    )

    python3 - "$build_log" "$CC" "$target_include" "$HOSTPY/include" <<'PY'
import pathlib
import sys

log = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").splitlines()
compiler = sys.argv[2]
target_include = "-I" + sys.argv[3]
host_include = "-I" + sys.argv[4]
compile_lines = [line for line in log if line.startswith(compiler + " ") and " -c " in line]
if len(compile_lines) < 20:
    raise SystemExit(f"unexpectedly small Pillow native build: {len(compile_lines)} compile units")
missing = [line for line in compile_lines if "-mstrict-align" not in line.split()]
if missing:
    raise SystemExit("Pillow compile unit missing -mstrict-align: " + missing[0])
core = next((line for line in compile_lines if " src/_imaging.c " in line), None)
if core is None:
    raise SystemExit("Pillow core extension compile command is missing")
if target_include not in core:
    raise SystemExit("Pillow core extension did not use target Python headers")
if host_include in core and core.index(target_include) > core.index(host_include):
    raise SystemExit("host Python headers precede target headers in Pillow build")
print(f"pillow_strict_compile_units={len(compile_lines)}")
PY

    wheel=$(find "$wheels" -maxdepth 1 -type f \
        -name 'pillow-12.3.0-cp314-cp314-linux_loongarch64.whl' -print -quit)
    [[ -n "$wheel" ]] || {
        echo "strict Pillow LoongArch wheel was not produced" >&2
        exit 1
    }
    python3 - "$wheel" "$site_packages" <<'PY'
import pathlib
import shutil
import sys
import zipfile

wheel = pathlib.Path(sys.argv[1])
site = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(wheel) as archive:
    for name in archive.namelist():
        member = pathlib.PurePosixPath(name)
        if member.is_absolute() or ".." in member.parts:
            raise SystemExit(f"unsafe Pillow wheel member: {name}")
    for stale in (site / "PIL", site / "pillow-12.3.0.dist-info"):
        if stale.exists():
            shutil.rmtree(stale)
    archive.extractall(site)
PY
    grep -Fq 'Tag: cp314-cp314-linux_loongarch64' \
        "$site_packages/pillow-12.3.0.dist-info/WHEEL" || {
        echo "installed Pillow metadata has the wrong target tag" >&2
        exit 1
    }
    [[ -f "$site_packages/PIL/_imaging.cpython-314.so" ]] || {
        echo "installed Pillow core extension is missing" >&2
        exit 1
    }
    mark_done "$stamp"
}

build_markupsafe() {
    local stamp=markupsafe-3.0.3
    if is_done "$stamp"; then return; fi
    local src=$BUILD/markupsafe-3.0.3
    local wheels=$BUILD/markupsafe-3.0.3-wheels
    local build_log=$BUILD/markupsafe-3.0.3-build.log
    local site_packages=$RUNTIME/usr/lib/python3.14/site-packages
    local target_include=$RUNTIME/usr/include/python3.14
    local target_sysconfig sysconfig_name wheel

    target_sysconfig=$(find "$RUNTIME/usr/lib/python3.14" -maxdepth 1 \
        -name '_sysconfigdata_*.py' -print -quit)
    [[ -f "$target_include/Python.h" && -n "$target_sysconfig" ]] || {
        echo "target Python build metadata is missing for MarkupSafe" >&2
        exit 1
    }
    sysconfig_name=$(basename "$target_sysconfig" .py)

    unpack_tar "$CACHE/$MARKUPSAFE_ARCHIVE" "$src"
    rm -rf -- "$wheels"
    mkdir -p "$wheels" "$site_packages"
    if [[ ! -f "$PILLOW_BUILD_DEPS/setuptools/__init__.py" ]]; then
        rm -rf -- "$PILLOW_BUILD_DEPS"
        mkdir -p "$PILLOW_BUILD_DEPS"
        for dependency_wheel in "$SETUPTOOLS_WHEEL" "$WHEEL_WHEEL"; do
            python3 -m zipfile -e "$CACHE/$dependency_wheel" "$PILLOW_BUILD_DEPS"
        done
    fi

    (
        cd "$src"
        SOURCE_DATE_EPOCH=0 \
        PYTHONPATH="$PILLOW_BUILD_DEPS:$RUNTIME/usr/lib/python3.14" \
        _PYTHON_SYSCONFIGDATA_NAME="$sysconfig_name" \
        _PYTHON_HOST_PLATFORM=linux-loongarch64 \
        CC="$CC" CXX="$CXX" LDSHARED="$CC -shared" \
        CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" \
        CPPFLAGS="-I$target_include $COMMON_CPPFLAGS" \
        LDFLAGS="$COMMON_LDFLAGS" \
        MAX_CONCURRENCY=1 \
        "$HOSTPY/bin/python3.14" setup.py bdist_wheel --dist-dir "$wheels" \
            2>&1 | tee "$build_log"
    )

    python3 - "$build_log" "$CC" "$target_include" "$HOSTPY/include" <<'PY'
import pathlib
import sys

log = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").splitlines()
compiler = sys.argv[2]
target_include = "-I" + sys.argv[3]
host_include = "-I" + sys.argv[4]
compile_lines = [line for line in log if line.startswith(compiler + " ") and " -c " in line]
if len(compile_lines) != 1:
    raise SystemExit(f"expected one MarkupSafe native compile unit, found {len(compile_lines)}")
command = compile_lines[0]
if "-mstrict-align" not in command.split():
    raise SystemExit("MarkupSafe speedups compile unit is missing -mstrict-align")
if target_include not in command:
    raise SystemExit("MarkupSafe speedups did not use target Python headers")
if host_include in command and command.index(target_include) > command.index(host_include):
    raise SystemExit("host Python headers precede target headers in MarkupSafe build")
print("markupsafe_strict_compile_units=1")
PY

    wheel=$(find "$wheels" -maxdepth 1 -type f \
        -name 'markupsafe-3.0.3-cp314-cp314-linux_loongarch64.whl' -print -quit)
    [[ -n "$wheel" ]] || {
        echo "strict MarkupSafe LoongArch wheel was not produced" >&2
        exit 1
    }
    python3 - "$wheel" "$site_packages" <<'PY'
import pathlib
import shutil
import sys
import zipfile

wheel = pathlib.Path(sys.argv[1])
site = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(wheel) as archive:
    for name in archive.namelist():
        member = pathlib.PurePosixPath(name)
        if member.is_absolute() or ".." in member.parts:
            raise SystemExit(f"unsafe MarkupSafe wheel member: {name}")
    for stale in (site / "markupsafe", site / "markupsafe-3.0.3.dist-info"):
        if stale.exists():
            shutil.rmtree(stale)
    archive.extractall(site)
PY
    grep -Fq 'Tag: cp314-cp314-linux_loongarch64' \
        "$site_packages/markupsafe-3.0.3.dist-info/WHEEL" || {
        echo "installed MarkupSafe metadata has the wrong target tag" >&2
        exit 1
    }
    [[ -f "$site_packages/markupsafe/_speedups.cpython-314.so" ]] || {
        echo "installed MarkupSafe speedups extension is missing" >&2
        exit 1
    }
    mark_done "$stamp"
}

build_pyyaml_pure() {
    local stamp=pyyaml-6.0.3-pure
    if is_done "$stamp"; then return; fi
    local src=$BUILD/pyyaml-6.0.3
    local wheels=$BUILD/pyyaml-6.0.3-wheels
    local build_log=$BUILD/pyyaml-6.0.3-build.log
    local site_packages=$RUNTIME/usr/lib/python3.14/site-packages
    local wheel

    unpack_tar "$CACHE/$PYYAML_ARCHIVE" "$src"
    rm -rf -- "$wheels"
    mkdir -p "$wheels" "$site_packages"
    if [[ ! -f "$PILLOW_BUILD_DEPS/setuptools/__init__.py" ]]; then
        rm -rf -- "$PILLOW_BUILD_DEPS"
        mkdir -p "$PILLOW_BUILD_DEPS"
        for dependency_wheel in "$SETUPTOOLS_WHEEL" "$WHEEL_WHEEL"; do
            python3 -m zipfile -e "$CACHE/$dependency_wheel" "$PILLOW_BUILD_DEPS"
        done
    fi

    # PyYAML's libyaml accelerator is optional.  Keep this runtime dependency
    # deliberately pure Python: that closes SmolAgent's import graph without
    # introducing an unverified native libyaml/Cython extension.  Every ELF in
    # the packaged runtime therefore remains covered by the strict manifest.
    (
        cd "$src"
        SOURCE_DATE_EPOCH=0 \
        PYYAML_FORCE_LIBYAML=0 \
        PYTHONPATH="$PILLOW_BUILD_DEPS" \
        "$HOSTPY/bin/python3.14" setup.py --without-libyaml \
            bdist_wheel --dist-dir "$wheels" 2>&1 | tee "$build_log"
    )
    if grep -Fq ' -c ' "$build_log"; then
        echo "pure PyYAML build unexpectedly compiled native code" >&2
        exit 1
    fi

    wheel=$(find "$wheels" -maxdepth 1 -type f \
        -name 'pyyaml-6.0.3-py3-none-any.whl' -print -quit)
    [[ -n "$wheel" ]] || {
        echo "pure PyYAML wheel was not produced" >&2
        exit 1
    }
    python3 - "$wheel" "$site_packages" <<'PY'
import pathlib
import shutil
import sys
import zipfile

wheel = pathlib.Path(sys.argv[1])
site = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(wheel) as archive:
    for name in archive.namelist():
        member = pathlib.PurePosixPath(name)
        if member.is_absolute() or ".." in member.parts:
            raise SystemExit(f"unsafe PyYAML wheel member: {name}")
    for stale in (site / "yaml", site / "_yaml"):
        if stale.exists():
            shutil.rmtree(stale)
    for stale in site.glob("*yaml-*.dist-info"):
        shutil.rmtree(stale)
    archive.extractall(site)

for path in [site / "yaml", site / "_yaml"]:
    for member in path.rglob("*"):
        if not member.is_file():
            continue
        if member.suffix == ".so" or ".so." in member.name:
            raise SystemExit(f"pure PyYAML wheel contains native extension: {member}")
        with member.open("rb") as stream:
            if stream.read(4) == b"\x7fELF":
                raise SystemExit(f"pure PyYAML wheel contains ELF: {member}")
PY
    grep -Fq 'Root-Is-Purelib: true' \
        "$site_packages/pyyaml-6.0.3.dist-info/WHEEL" || {
        echo "installed PyYAML metadata is not pure Python" >&2
        exit 1
    }
    grep -Fq 'Tag: py3-none-any' \
        "$site_packages/pyyaml-6.0.3.dist-info/WHEEL" || {
        echo "installed PyYAML metadata has the wrong wheel tag" >&2
        exit 1
    }
    mark_done "$stamp"
}

install_wheel_safely() {
    local wheel=$1 expected_name=$2 expected_version=$3 expected_tag=$4
    local site_packages=$RUNTIME/usr/lib/python3.14/site-packages
    mkdir -p "$site_packages"
    python3 - "$wheel" "$site_packages" "$expected_name" "$expected_version" "$expected_tag" <<'PY'
import csv
import email.parser
import pathlib
import shutil
import sys
import zipfile

wheel = pathlib.Path(sys.argv[1])
site = pathlib.Path(sys.argv[2])
expected_name, expected_version, expected_tag = sys.argv[3:]

def normalized(value):
    return value.lower().replace("-", "_").replace(".", "_")

with zipfile.ZipFile(wheel) as archive:
    names = archive.namelist()
    for name in names:
        member = pathlib.PurePosixPath(name)
        if member.is_absolute() or ".." in member.parts:
            raise SystemExit(f"unsafe wheel member: {name}")
    metadata_names = [name for name in names if name.endswith(".dist-info/METADATA")]
    wheel_names = [name for name in names if name.endswith(".dist-info/WHEEL")]
    if len(metadata_names) != 1 or len(wheel_names) != 1:
        raise SystemExit(f"invalid wheel metadata layout: {wheel}")
    metadata = email.parser.BytesParser().parsebytes(archive.read(metadata_names[0]))
    if normalized(metadata["Name"]) != normalized(expected_name):
        raise SystemExit(f"wheel name mismatch: {metadata['Name']} != {expected_name}")
    if metadata["Version"] != expected_version:
        raise SystemExit(f"wheel version mismatch: {metadata['Version']} != {expected_version}")
    wheel_text = archive.read(wheel_names[0]).decode("utf-8")
    if f"Tag: {expected_tag}" not in wheel_text:
        raise SystemExit(f"wheel tag mismatch: expected {expected_tag}")
    if expected_tag.endswith("none-any") and "Root-Is-Purelib: true" not in wheel_text:
        raise SystemExit(f"pure wheel is not marked purelib: {wheel}")
    for name in names:
        if name.endswith("/"):
            continue
        payload = archive.read(name)
        if name.endswith(".so") or ".so." in name or payload.startswith(b"\x7fELF"):
            if expected_tag.endswith("none-any"):
                raise SystemExit(f"pure wheel contains native payload: {name}")

    # Remove previous versions of this distribution using their RECORD files,
    # but never follow paths outside the target site-packages directory.
    for dist_info in sorted(site.glob("*.dist-info")):
        old_metadata = dist_info / "METADATA"
        if not old_metadata.is_file():
            continue
        old = email.parser.BytesParser().parse(old_metadata.open("rb"))
        if normalized(old.get("Name", "")) != normalized(expected_name):
            continue
        record = dist_info / "RECORD"
        if record.is_file():
            with record.open(newline="", encoding="utf-8") as stream:
                for row in csv.reader(stream):
                    if not row:
                        continue
                    relative = pathlib.PurePosixPath(row[0])
                    if relative.is_absolute() or ".." in relative.parts:
                        raise SystemExit(f"unsafe installed RECORD path: {row[0]}")
                    candidate = site.joinpath(*relative.parts)
                    if candidate.is_file() or candidate.is_symlink():
                        candidate.unlink()
        if dist_info.exists():
            shutil.rmtree(dist_info)
    archive.extractall(site)
PY
}

build_lxml() {
    local stamp=lxml-6.1.1-strict
    if is_done "$stamp"; then return; fi
    local src=$BUILD/lxml-6.1.1
    local wheels=$BUILD/lxml-6.1.1-wheels
    local build_log=$BUILD/lxml-6.1.1-build.log
    local site_packages=$RUNTIME/usr/lib/python3.14/site-packages
    local target_include=$RUNTIME/usr/include/python3.14
    local target_sysconfig sysconfig_name wheel

    target_sysconfig=$(find "$RUNTIME/usr/lib/python3.14" -maxdepth 1 \
        -name '_sysconfigdata_*.py' -print -quit)
    [[ -f "$target_include/Python.h" && -n "$target_sysconfig" ]] || {
        echo "target Python build metadata is missing for lxml" >&2
        exit 1
    }
    sysconfig_name=$(basename "$target_sysconfig" .py)
    unpack_tar "$CACHE/$LXML_ARCHIVE" "$src"
    rm -rf -- "$wheels"
    mkdir -p "$wheels" "$site_packages"

    (
        cd "$src"
        SOURCE_DATE_EPOCH=0 \
        PYTHONPATH="$PILLOW_BUILD_DEPS:$RUNTIME/usr/lib/python3.14" \
        _PYTHON_SYSCONFIGDATA_NAME="$sysconfig_name" \
        _PYTHON_HOST_PLATFORM=linux-loongarch64 \
        CC="$CC" CXX="$CXX" LDSHARED="$CC -shared" \
        CFLAGS="$COMMON_CFLAGS" CXXFLAGS="$COMMON_CFLAGS" \
        CPPFLAGS="-I$target_include $COMMON_CPPFLAGS" \
        LDFLAGS="$COMMON_LDFLAGS" \
        PKG_CONFIG=pkg-config \
        PKG_CONFIG_SYSROOT_DIR="$SYSROOT" \
        PKG_CONFIG_LIBDIR="$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig" \
        "$HOSTPY/bin/python3.14" setup.py --without-cython bdist_wheel \
            --dist-dir "$wheels" 2>&1 | tee "$build_log"
    )
    verify_strict_c_build_log lxml "$build_log" 2
    wheel=$(find "$wheels" -maxdepth 1 -type f \
        -name 'lxml-6.1.1-cp314-cp314-linux_loongarch64.whl' -print -quit)
    [[ -n "$wheel" ]] || {
        echo "strict lxml LoongArch wheel was not produced" >&2
        exit 1
    }
    install_wheel_safely "$wheel" lxml 6.1.1 cp314-cp314-linux_loongarch64
    [[ $(find "$site_packages/lxml" -maxdepth 1 -type f -name '*.so' | wc -l) -ge 2 ]] || {
        echo "installed lxml native extensions are missing" >&2
        exit 1
    }
    mark_done "$stamp"
}

setup_rust_toolchain() {
    mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" "$HOST_TOOLS/bin" "$HOST_TOOLS/home"
    install -m 0755 "$CACHE/$RUSTUP_INIT" "$HOST_TOOLS/bin/rustup-init"
    export RUSTUP_HOME CARGO_HOME HOME=$HOST_TOOLS/home
    export PATH="$CARGO_HOME/bin:$HOST_TOOLS/bin:$PATH"
    if [[ ! -x "$CARGO_HOME/bin/rustc" ]]; then
        "$HOST_TOOLS/bin/rustup-init" -y --no-modify-path --profile minimal \
            --default-toolchain nightly-2025-01-18 \
            --target loongarch64-unknown-linux-musl
    else
        rustup target add --toolchain nightly-2025-01-18 \
            loongarch64-unknown-linux-musl
    fi
    rustup default nightly-2025-01-18
    rustc --version | grep -Fq '1.86.0-nightly' || {
        echo "unexpected Rust compiler for primp: $(rustc --version)" >&2
        exit 1
    }
}

build_primp() {
    local stamp=primp-0.15.0-strict
    if is_done "$stamp"; then return; fi
    local src=$BUILD/primp-0.15.0
    local wheels=$BUILD/primp-0.15.0-wheels
    local build_log=$BUILD/primp-0.15.0-build.log
    local maturin_root=$HOST_TOOLS/maturin-1.8.3
    local boringssl_crate boringssl_src wheel

    setup_rust_toolchain
    unpack_tar "$CACHE/$PRIMP_ARCHIVE" "$src"
    [[ -f "$src/Cargo.lock" ]] || {
        echo "pinned primp sdist is missing Cargo.lock" >&2
        exit 1
    }
    (
        cd "$src"
        cargo metadata --locked --format-version 1 \
            > "$BUILD/primp-0.15.0-cargo-metadata.json"
    )
    boringssl_crate=$(find "$CARGO_HOME/registry/src" -maxdepth 2 -type d \
        -name 'boring-sys2-4.15.11' -print -quit)
    [[ -n "$boringssl_crate" && -f "$boringssl_crate/deps/boringssl/CMakeLists.txt" ]] || {
        echo "Cargo.lock did not resolve the expected boring-sys2 4.15.11 source" >&2
        exit 1
    }
    boringssl_src=$BUILD/boringssl-primp-0.15.0
    rm -rf -- "$boringssl_src"
    mkdir -p "$boringssl_src"
    cp -a "$boringssl_crate/deps/boringssl/." "$boringssl_src/"
    git -C "$boringssl_src" init --quiet
    git -C "$boringssl_src" apply --whitespace=fix \
        "$boringssl_crate/patches/boringssl-44b3df6f03d85c901767250329c571db405122d5.patch"
    git -C "$boringssl_src" apply --whitespace=error \
        "$ROOT/user/tools/cpython/patches/boringssl-loongarch64-generic.patch"
    rm -rf -- "$wheels" "$maturin_root"
    mkdir -p "$wheels" "$maturin_root" "$HOST_TOOLS/bin"
    python3 -m zipfile -e "$CACHE/$MATURIN_WHEEL" "$maturin_root"
    install -m 0755 "$maturin_root/maturin-1.8.3.data/scripts/maturin" \
        "$HOST_TOOLS/bin/maturin"

    (
        cd "$src"
        SOURCE_DATE_EPOCH=0 \
        PYO3_CROSS=1 PYO3_CROSS_PYTHON_VERSION=3.14 \
        PYO3_CROSS_LIB_DIR="$RUNTIME/usr/lib" \
        CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER="$CC" \
        CC_loongarch64_unknown_linux_musl="$CC" \
        CXX_loongarch64_unknown_linux_musl="$CXX" \
        AR_loongarch64_unknown_linux_musl="$AR" \
        CFLAGS_loongarch64_unknown_linux_musl="$COMMON_CFLAGS" \
        CXXFLAGS_loongarch64_unknown_linux_musl="$COMMON_CFLAGS" \
        BORING_BSSL_SOURCE_PATH="$boringssl_src" \
        BORING_BSSL_ASSUME_PATCHED=1 \
        BORING_BSSL_SYSROOT="$SYSROOT" \
        RUSTFLAGS='-C target-feature=-ual' \
        "$HOST_TOOLS/bin/maturin" -vv build --release --locked \
            --target loongarch64-unknown-linux-musl \
            --compatibility linux --out "$wheels" 2>&1 | tee "$build_log"
    )
    grep -Fq 'target-feature=-ual' "$build_log" || {
        echo "primp Rust build log does not prove -ual" >&2
        exit 1
    }
    local boringssl_commands
    boringssl_commands=$(find "$src/target/loongarch64-unknown-linux-musl/release/build" \
        -path '*boring-sys2-*/out/build/compile_commands.json' -print -quit)
    [[ -n "$boringssl_commands" ]] || {
        echo "primp BoringSSL compile database is missing" >&2
        exit 1
    }
    python3 - "$boringssl_commands" <<'PY'
import json
import pathlib
import sys

commands = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
if len(commands) < 100:
    raise SystemExit(f"unexpectedly small BoringSSL build: {len(commands)} compile units")
missing = [entry["file"] for entry in commands if "-mstrict-align" not in entry["command"].split()]
if missing:
    raise SystemExit("BoringSSL compile unit missing -mstrict-align: " + missing[0])
if not all("-DOPENSSL_NO_ASM" in entry["command"] for entry in commands):
    raise SystemExit("BoringSSL LoongArch generic build is missing OPENSSL_NO_ASM")
print(f"boringssl_strict_compile_units={len(commands)}")
PY
    wheel=$(find "$wheels" -maxdepth 1 -type f \
        -name 'primp-0.15.0-cp38-abi3-linux_loongarch64.whl' -print -quit)
    [[ -n "$wheel" ]] || {
        echo "strict primp LoongArch abi3 wheel was not produced" >&2
        exit 1
    }
    install_wheel_safely "$wheel" primp 0.15.0 cp38-abi3-linux_loongarch64
    mark_done "$stamp"
}

install_smolagents_toolkit_pure() {
    local stamp=smolagents-toolkit-pure-v1
    if is_done "$stamp"; then return; fi
    install_wheel_safely "$CACHE/$CLICK_WHEEL" click 8.1.8 py3-none-any
    install_wheel_safely "$CACHE/$SIX_WHEEL" six 1.17.0 py3-none-any
    install_wheel_safely "$CACHE/$SOUPSIEVE_WHEEL" soupsieve 2.6 py3-none-any
    install_wheel_safely "$CACHE/$BEAUTIFULSOUP4_WHEEL" beautifulsoup4 4.12.3 py3-none-any
    install_wheel_safely "$CACHE/$MARKDOWNIFY_WHEEL" markdownify 0.14.1 py3-none-any
    install_wheel_safely "$CACHE/$DDGS_WHEEL" ddgs 9.0.0 py3-none-any
    mark_done "$stamp"
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
    local package_digest toolchain_syslib
    package_digest=$(package_input_digest)
    if package_cache_current; then return; fi
    rm -f "$STAMPS/runtime-package.done"
    mkdir -p "$RUNTIME/lib" "$RUNTIME/usr/lib" "$RUNTIME/etc"
    cp -a "$SYSROOT/lib/ld-musl-loongarch64.so.1" "$RUNTIME/lib/"
    ln -sfn ld-musl-loongarch64.so.1 "$RUNTIME/lib/libc.musl-loongarch64.so.1"
    for pattern in \
        'libz.so*' 'libbz2.so*' 'liblzma.so*' 'libffi.so*' 'libexpat.so*' \
        'libmpdec.so*' 'libcrypto.so*' 'libssl.so*' 'libncursesw.so*' \
        'libtinfow.so*' 'libpanelw.so*' 'libreadline.so*' 'libhistory.so*' \
        'libsqlite3.so*' 'libjpeg.so*' 'libxml2.so*' 'libxslt.so*' \
        'libexslt.so*'; do
        copy_runtime_library "$pattern"
    done
    # Rust's LoongArch musl target and BoringSSL's C++ glue link the pinned
    # cross-toolchain unwind/C++ runtimes dynamically.  These libraries do not
    # belong to the separately built musl sysroot, so include the exact files
    # from the same pinned toolchain that linked primp.
    toolchain_syslib=$TOOLCHAIN_ROOT/$TOOLCHAIN_PREFIX/sysroot/lib
    install -m 0755 "$toolchain_syslib/libgcc_s.so.1" \
        "$RUNTIME/usr/lib/libgcc_s.so.1"
    install -m 0755 "$toolchain_syslib/libstdc++.so.6.0.34" \
        "$RUNTIME/usr/lib/libstdc++.so.6.0.34"
    ln -sfn libstdc++.so.6.0.34 "$RUNTIME/usr/lib/libstdc++.so.6"
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
    install -m 0755 "$ROOT/user/tools/cpython/python3-wrapper-persist.sh" "$RUNTIME/python3-wrapper.sh"
    install -m 0755 "$ROOT/user/tools/cpython/cpython_testcode.sh" "$RUNTIME/cpython_testcode.sh"
    install -m 0755 "$ROOT/user/tools/cpython/run_strict_benchmark.sh" "$RUNTIME/run_strict_benchmark.sh"
    install -m 0755 "$ROOT/user/tools/cpython/run_strict_functional.sh" "$RUNTIME/run_strict_functional.sh"
    install -m 0755 "$ROOT/user/tools/cpython/strict_runtime_smoke.sh" "$RUNTIME/strict_runtime_smoke.sh"
    install -m 0644 "$ROOT/user/tools/cpython/verify_runtime_integrity.py" "$RUNTIME/verify_runtime_integrity.py"
    install -m 0644 "$ROOT/user/tools/cpython/pillow_strict_smoke.py" "$RUNTIME/pillow_strict_smoke.py"
    install -m 0644 "$ROOT/user/tools/cpython/smolagents_toolkit_smoke.py" \
        "$RUNTIME/smolagents_toolkit_smoke.py"
    install -m 0755 "$ROOT/user/tools/cpython/L3_check_files.sh" "$RUNTIME/L3_check_files.sh"
    install -m 0755 "$ROOT/user/tools/cpython/L4_startup.sh" "$RUNTIME/L4_startup.sh"
    install -m 0644 "$ROOT/user/tools/cpython/L5_language.py" "$RUNTIME/L5_language.py"
    install -m 0644 "$ROOT/user/tools/cpython/L6_stdlib.py" "$RUNTIME/L6_stdlib.py"
    install -m 0644 "$ROOT/user/tools/cpython/L7_filesystem.py" "$RUNTIME/L7_filesystem.py"
    install -m 0644 "$ROOT/user/tools/cpython/L8_thread.py" "$RUNTIME/L8_thread.py"
    install -m 0644 "$ROOT/user/tools/cpython/L8_subprocess.py" "$RUNTIME/L8_subprocess.py"
    install -m 0644 "$ROOT/user/tools/cpython/L9_socket.py" "$RUNTIME/L9_socket.py"

    python3 - "$RUNTIME" "$STRIP" "$READELF" <<'PY'
import pathlib
import subprocess
import sys

runtime = pathlib.Path(sys.argv[1])
strip = sys.argv[2]
readelf = sys.argv[3]
elfs = []
strip_elfs = []
for path in sorted(runtime.rglob("*")):
    if not path.is_file() or path.is_symlink():
        continue
    with path.open("rb") as stream:
        if stream.read(4) == b"\x7fELF":
            elfs.append(str(path))
            sections = subprocess.run(
                [readelf, "-S", str(path)],
                text=True,
                capture_output=True,
                check=True,
            ).stdout
            # Re-running GNU strip on an already stripped, patchelf-adjusted
            # ELF is not guaranteed to be byte-idempotent.  Only files that
            # still contain symbols or debug sections need this operation.
            if ".symtab" in sections or ".debug_" in sections:
                strip_elfs.append(str(path))
for offset in range(0, len(strip_elfs), 64):
    subprocess.run(
        [strip, "--strip-unneeded", *strip_elfs[offset:offset + 64]],
        check=True,
    )
print(f"native_elfs={len(elfs)}")
print(f"stripped_elfs={len(strip_elfs)}")
PY

    # The wrapper starts Python through the P4 loader explicitly, but Python
    # subprocesses commonly exec sys.executable directly.  Bind every runtime
    # executable with PT_INTERP to the stable P4 `current` loader so self-exec,
    # pip build isolation, and multiprocessing cannot fall back to /lib.
    python3 - "$RUNTIME" "$READELF" "$PATCHELF" "$RUNTIME_INTERP" <<'PY'
import pathlib
import subprocess
import sys

runtime = pathlib.Path(sys.argv[1])
readelf = sys.argv[2]
patchelf = sys.argv[3]
expected = sys.argv[4]
bound = []
patched = []
for path in sorted(runtime.rglob("*")):
    if not path.is_file() or path.is_symlink():
        continue
    with path.open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            continue
    program_headers = subprocess.run(
        [readelf, "-l", str(path)], text=True, capture_output=True, check=True
    ).stdout
    interpreter = None
    for line in program_headers.splitlines():
        if "Requesting program interpreter:" in line:
            interpreter = line.split(
                "Requesting program interpreter:", 1
            )[1].rstrip("]").strip()
            break
    if interpreter is None:
        continue
    # patchelf --set-interpreter is not byte-idempotent for an already-bound
    # executable: repeated packaging can grow the program-header layout and
    # change the artifact hash.  Rewrite only when the current PT_INTERP is
    # different, then always verify the final value.
    if interpreter != expected:
        subprocess.run([patchelf, "--set-interpreter", expected, str(path)], check=True)
        patched.append(str(path.relative_to(runtime)))
    verified = subprocess.run(
        [readelf, "-l", str(path)], text=True, capture_output=True, check=True
    ).stdout
    if f"Requesting program interpreter: {expected}" not in verified:
        raise SystemExit(f"failed to bind PT_INTERP for {path}")
    bound.append(str(path.relative_to(runtime)))
if not bound:
    raise SystemExit("runtime contains no PT_INTERP executable to bind")
print("p4_interp_elfs=" + ",".join(bound))
print("p4_interp_rewritten=" + (",".join(patched) if patched else "none"))
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
        "$RUNTIME/lib/ld-musl-loongarch64.so.1" \
        --library-path "$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" -S -c \
        'import _bz2,_ctypes,_decimal,_hashlib,_lzma,_sqlite3,readline,ssl,sysconfig,threading,zlib; flags=" ".join(str(sysconfig.get_config_var(k) or "") for k in ("CFLAGS","CONFIGURE_CFLAGS","CONFIGURE_CFLAGS_NODIST","PY_CFLAGS","PGO_PROF_USE_FLAG")); assert "-mstrict-align" in flags; assert "-fprofile-use" in flags; args=sysconfig.get_config_var("CONFIG_ARGS") or ""; assert "--enable-optimizations" in args and "--with-lto" in args; t=threading.Thread(target=lambda:None); t.start(); t.join(); print("strict-runtime-smoke-ok")'

    "$QEMU" -L "$RUNTIME" \
        -E "LD_LIBRARY_PATH=$RUNTIME/usr/lib:$RUNTIME/lib" \
        -E "PYTHONHOME=$RUNTIME/usr" \
        -E "PYTHONNOUSERSITE=1" \
        -E "CPYTHON_ROOT=$RUNTIME" \
        "$RUNTIME/lib/ld-musl-loongarch64.so.1" \
        --library-path "$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" -c \
        'from markupsafe import Markup, _speedups, escape; assert escape("<x>") == Markup("&lt;x&gt;"); print("strict-markupsafe-smoke-ok", _speedups.__file__)'

    "$QEMU" -L "$RUNTIME" \
        -E "LD_LIBRARY_PATH=$RUNTIME/usr/lib:$RUNTIME/lib" \
        -E "PYTHONHOME=$RUNTIME/usr" \
        -E "PYTHONNOUSERSITE=1" \
        -E "CPYTHON_ROOT=$RUNTIME" \
        "$RUNTIME/lib/ld-musl-loongarch64.so.1" \
        --library-path "$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" -c \
        'import yaml; assert yaml.__version__ == "6.0.3" and yaml.__with_libyaml__ is False; assert yaml.safe_load("answer: 42") == {"answer": 42}; print("strict-pyyaml-pure-smoke-ok", yaml.__file__)'

    "$QEMU" -L "$RUNTIME" \
        -E "LD_LIBRARY_PATH=$RUNTIME/usr/lib:$RUNTIME/lib" \
        -E "PYTHONHOME=$RUNTIME/usr" \
        -E "PYTHONNOUSERSITE=1" \
        -E "CPYTHON_ROOT=$RUNTIME" \
        "$RUNTIME/lib/ld-musl-loongarch64.so.1" \
        --library-path "$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" "$RUNTIME/pillow_strict_smoke.py"

    "$QEMU" -L "$RUNTIME" \
        -E "LD_LIBRARY_PATH=$RUNTIME/usr/lib:$RUNTIME/lib" \
        -E "PYTHONHOME=$RUNTIME/usr" \
        -E "PYTHONNOUSERSITE=1" \
        -E "CPYTHON_ROOT=$RUNTIME" \
        "$RUNTIME/lib/ld-musl-loongarch64.so.1" \
        --library-path "$RUNTIME/usr/lib:$RUNTIME/lib" \
        "$RUNTIME/usr/bin/python3" -S "$RUNTIME/smolagents_toolkit_smoke.py" --exact

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
expected_interp = "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"

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
    program_headers = subprocess.run(
        [readelf, "-l", str(path)], text=True, capture_output=True, check=False
    ).stdout
    interpreter = None
    for line in program_headers.splitlines():
        if "Requesting program interpreter:" in line:
            interpreter = line.split("Requesting program interpreter:", 1)[1].rstrip("]").strip()
            if interpreter != expected_interp:
                raise SystemExit(
                    f"non-P4 PT_INTERP in runtime: {path}: {interpreter}"
                )
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
        "interpreter": interpreter,
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
pillow_wheels = sorted((out / "build" / "pillow-12.3.0-wheels").glob("*.whl"))
if len(pillow_wheels) != 1:
    raise SystemExit(f"expected one strict Pillow wheel, found {len(pillow_wheels)}")
pillow_wheel = pillow_wheels[0]
pillow_wheel_tag = "cp314-cp314-linux_loongarch64"
pillow_metadata = runtime / "usr/lib/python3.14/site-packages/pillow-12.3.0.dist-info/WHEEL"
if f"Tag: {pillow_wheel_tag}" not in pillow_metadata.read_text(encoding="utf-8"):
    raise SystemExit("installed Pillow wheel does not carry the target ABI tag")
markupsafe_wheels = sorted((out / "build" / "markupsafe-3.0.3-wheels").glob("*.whl"))
if len(markupsafe_wheels) != 1:
    raise SystemExit(f"expected one strict MarkupSafe wheel, found {len(markupsafe_wheels)}")
markupsafe_wheel = markupsafe_wheels[0]
markupsafe_wheel_tag = "cp314-cp314-linux_loongarch64"
markupsafe_metadata = runtime / "usr/lib/python3.14/site-packages/markupsafe-3.0.3.dist-info/WHEEL"
if f"Tag: {markupsafe_wheel_tag}" not in markupsafe_metadata.read_text(encoding="utf-8"):
    raise SystemExit("installed MarkupSafe wheel does not carry the target ABI tag")
pyyaml_wheels = sorted((out / "build" / "pyyaml-6.0.3-wheels").glob("*.whl"))
if len(pyyaml_wheels) != 1:
    raise SystemExit(f"expected one pure PyYAML wheel, found {len(pyyaml_wheels)}")
pyyaml_wheel = pyyaml_wheels[0]
pyyaml_wheel_tag = "py3-none-any"
pyyaml_metadata = runtime / "usr/lib/python3.14/site-packages/pyyaml-6.0.3.dist-info/WHEEL"
pyyaml_wheel_text = pyyaml_metadata.read_text(encoding="utf-8")
if f"Tag: {pyyaml_wheel_tag}" not in pyyaml_wheel_text or "Root-Is-Purelib: true" not in pyyaml_wheel_text:
    raise SystemExit("installed PyYAML wheel is not the expected pure-Python build")

lxml_wheels = sorted((out / "build" / "lxml-6.1.1-wheels").glob("*.whl"))
if len(lxml_wheels) != 1:
    raise SystemExit(f"expected one strict lxml wheel, found {len(lxml_wheels)}")
lxml_wheel = lxml_wheels[0]
lxml_wheel_tag = "cp314-cp314-linux_loongarch64"
lxml_metadata = runtime / "usr/lib/python3.14/site-packages/lxml-6.1.1.dist-info/WHEEL"
if f"Tag: {lxml_wheel_tag}" not in lxml_metadata.read_text(encoding="utf-8"):
    raise SystemExit("installed lxml wheel does not carry the target ABI tag")

primp_wheels = sorted((out / "build" / "primp-0.15.0-wheels").glob("*.whl"))
if len(primp_wheels) != 1:
    raise SystemExit(f"expected one strict primp wheel, found {len(primp_wheels)}")
primp_wheel = primp_wheels[0]
primp_wheel_tag = "cp38-abi3-linux_loongarch64"
primp_metadata = runtime / "usr/lib/python3.14/site-packages/primp-0.15.0.dist-info/WHEEL"
if f"Tag: {primp_wheel_tag}" not in primp_metadata.read_text(encoding="utf-8"):
    raise SystemExit("installed primp wheel does not carry the target ABI tag")

pure_toolkit_specs = {
    "click": ("8.1.8", "click-8.1.8-py3-none-any.whl", "py3-none-any"),
    "six": ("1.17.0", "six-1.17.0-py2.py3-none-any.whl", "py3-none-any"),
    "soupsieve": ("2.6", "soupsieve-2.6-py3-none-any.whl", "py3-none-any"),
    "beautifulsoup4": ("4.12.3", "beautifulsoup4-4.12.3-py3-none-any.whl", "py3-none-any"),
    "markdownify": ("0.14.1", "markdownify-0.14.1-py3-none-any.whl", "py3-none-any"),
    "ddgs": ("9.0.0", "ddgs-9.0.0-py3-none-any.whl", "py3-none-any"),
}
pure_toolkit_manifest = {}
for distribution, (version, wheel_name, wheel_tag) in pure_toolkit_specs.items():
    dist_info = distribution.replace("-", "_") + f"-{version}.dist-info"
    wheel_metadata = runtime / "usr/lib/python3.14/site-packages" / dist_info / "WHEEL"
    wheel_text = wheel_metadata.read_text(encoding="utf-8")
    if f"Tag: {wheel_tag}" not in wheel_text or "Root-Is-Purelib: true" not in wheel_text:
        raise SystemExit(f"installed {distribution} wheel is not the expected pure-Python build")
    source_wheel = out / "cache" / wheel_name
    pure_toolkit_manifest[distribution] = {
        "version": version,
        "source_build": False,
        "wheel": wheel_name,
        "wheel_sha256": sha256(source_wheel),
        "wheel_tag": wheel_tag,
        "pure_python": True,
    }

manifest = {
    "schema": 4,
    "runtime_policy": "mangocore-la64-strict-align-v1",
    "native_closure_policy": "CPython, Pillow, MarkupSafe, lxml, primp, musl loader/libc and every packaged native dependency are strict-aligned; GCC/C extensions use -mstrict-align, Rust uses -C target-feature=-ual, and PyYAML plus the remaining SmolAgents toolkit is pure Python",
    "runtime_interpreter": expected_interp,
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
    "python_packages": {
        "Pillow": {
            "version": "12.3.0",
            "source_build": True,
            "wheel": pillow_wheel.name,
            "wheel_sha256": sha256(pillow_wheel),
            "wheel_tag": pillow_wheel_tag,
            "features_enabled": ["jpeg", "zlib"],
            "features_disabled": [
                "avif", "freetype", "imagequant", "jpeg2000", "lcms",
                "raqm", "tiff", "webp", "xcb",
            ],
        },
        "MarkupSafe": {
            "version": "3.0.3",
            "source_build": True,
            "wheel": markupsafe_wheel.name,
            "wheel_sha256": sha256(markupsafe_wheel),
            "wheel_tag": markupsafe_wheel_tag,
            "speedups": True,
        },
        "PyYAML": {
            "version": "6.0.3",
            "source_build": True,
            "wheel": pyyaml_wheel.name,
            "wheel_sha256": sha256(pyyaml_wheel),
            "wheel_tag": pyyaml_wheel_tag,
            "pure_python": True,
            "libyaml_accelerator": False,
        },
        "lxml": {
            "version": "6.1.1",
            "source_build": True,
            "wheel": lxml_wheel.name,
            "wheel_sha256": sha256(lxml_wheel),
            "wheel_tag": lxml_wheel_tag,
            "without_cython": True,
            "dynamic_dependencies": ["libxml2.so.16", "libxslt.so.1", "libexslt.so.0"],
        },
        "primp": {
            "version": "0.15.0",
            "source_build": True,
            "wheel": primp_wheel.name,
            "wheel_sha256": sha256(primp_wheel),
            "wheel_tag": primp_wheel_tag,
            "rust_toolchain": "nightly-2025-01-18",
            "rust_target_feature": "-ual",
            "cargo_locked": True,
        },
        **pure_toolkit_manifest,
    },
    "native_dependencies": {
        "libjpeg-turbo": {
            "version": "3.1.4.1",
            "shared_soname": "libjpeg.so.62",
            "simd": False,
            "strict_align_compile_database_verified": True,
        },
        "libxml2": {
            "version": "2.14.6",
            "shared_soname": "libxml2.so.16",
            "strict_align_build_log_verified": True,
        },
        "libxslt": {
            "version": "1.1.43",
            "shared_soname": "libxslt.so.1",
            "exslt_soname": "libexslt.so.0",
            "strict_align_build_log_verified": True,
        }
    },
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
    printf '%s\n' "$package_digest" > "$STAMPS/runtime-package.inputs.sha256.tmp"
    mv "$STAMPS/runtime-package.inputs.sha256.tmp" "$STAMPS/runtime-package.inputs.sha256"
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
required_interp = "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"

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
    packages = manifest.get("python_packages", {})
    if (
        manifest.get("schema", 0) < 4
        or manifest.get("target") != "loongarch64-linux-musl"
        or not required_flags.issubset(flags)
        or manifest.get("pgo") is not True
        or manifest.get("lto") is not True
        or manifest.get("runtime_interpreter") != required_interp
        or packages.get("Pillow", {}).get("version") != "12.3.0"
        or packages.get("MarkupSafe", {}).get("version") != "3.0.3"
        or packages.get("lxml", {}).get("version") != "6.1.1"
        or packages.get("primp", {}).get("version") != "0.15.0"
        or packages.get("ddgs", {}).get("version") != "9.0.0"
        or packages.get("markdownify", {}).get("version") != "0.14.1"
        or packages.get("beautifulsoup4", {}).get("version") != "4.12.3"
        or packages.get("soupsieve", {}).get("version") != "2.6"
        or packages.get("six", {}).get("version") != "1.17.0"
        or packages.get("click", {}).get("version") != "8.1.8"
        or not manifest.get("elfs")
    ):
        continue
    if any(
        elf.get("interpreter") not in (None, required_interp)
        for elf in manifest["elfs"]
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
    setup_qemu_user
    setup_toolchain
    install_kernel_headers
    build_musl
    setup_musl_wrapper
    build_zlib
    build_libjpeg_turbo
    build_bzip2
    build_xz
    build_libxml2
    build_libxslt
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
    build_pillow
    build_markupsafe
    build_pyyaml_pure
    build_lxml
    build_primp
    install_smolagents_toolkit_pure
    package_runtime
    write_current_artifact_index
}

main "$@"
