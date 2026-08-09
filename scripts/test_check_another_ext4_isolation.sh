#!/usr/bin/env sh
set -eu

root_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
gate_script="$root_dir/scripts/check_another_ext4_isolation.sh"
submodule_path='dependency/another_ext4'
submodule_commit='ab3b4dbd444c0bf78cf96669c4f9969d97647770'
source_submodule="$root_dir/$submodule_path"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/another-ext4-isolation.XXXXXX")
fixture_root="$tmp_dir/repo"

cleanup() {
    rm -rf "$tmp_dir"
}

trap cleanup EXIT HUP INT TERM

fail() {
    printf '%s\n' "test_check_another_ext4_isolation: $1" >&2
    exit 1
}

test -f "$gate_script" || fail "missing gate script: $gate_script"
test -d "$source_submodule" || fail "missing source submodule: $source_submodule"
git -C "$source_submodule" cat-file -e "$submodule_commit^{commit}" || \
    fail "source submodule does not contain $submodule_commit"

write_fixture_sources() {
    scenario=$1
    default_features='default = ["ext4_another_backend"]'
    extra_feature=''
    boot_route='let root = self::ext4_backend::open(crate::drivers::BLOCK_DEVICE.clone());'
    block_route='let mounted = self::ext4_backend::open(block_device.clone());'
    syscall_route='let mounted = crate::fs::ext4_backend::open(blk_dev.clone());'

    case "$scenario" in
        zero)
            default_features='default = []'
            ;;
        mixed)
            extra_feature='ext4_legacy_backend = []'
            default_features='default = ["ext4_another_backend", "ext4_legacy_backend"]'
            ;;
        bypass)
            syscall_route='let mounted = crate::fs::ext4::ext4fs::Ext4FileSystem::open_ext4rs(blk_dev.clone());'
            ;;
        good)
            ;;
        *)
            fail "unknown fixture scenario: $scenario"
            ;;
    esac

    mkdir -p "$fixture_root/os/src/fs" "$fixture_root/os/src/syscall/fs" "$fixture_root/os/make" "$fixture_root/scripts"
    printf '%s\n' \
        '[submodule "dependency/another_ext4"]' \
        '    path = dependency/another_ext4' \
        '    url = https://github.com/Mango-Iced-Americano/another_ext4.git' > "$fixture_root/.gitmodules"
    printf '%s\n' \
        '[dependencies]' \
        'lwext4_rust = { path = "../dependency/lwext4_rust", default-features = false, optional = true }' \
        'another_ext4 = { path = "../dependency/another_ext4", optional = true }' \
        '[features]' \
        "$default_features" \
        'ext4_lwext4_backend = ["dep:lwext4_rust"]' \
        'ext4_legacy_backend = []' \
        'ext4_another_backend = ["dep:another_ext4"]' \
        "$extra_feature" > "$fixture_root/os/Cargo.toml"
    printf '%s\n' \
        'EXT4_BACKEND ?= another' \
        'ifeq ($(EXT4_BACKEND),lwext4)' \
        'EXT4_BACKEND_FEATURE := ext4_lwext4_backend' \
        'else ifeq ($(EXT4_BACKEND),legacy)' \
        'EXT4_BACKEND_FEATURE := ext4_legacy_backend' \
        'else ifeq ($(EXT4_BACKEND),another)' \
        'EXT4_BACKEND_FEATURE := ext4_another_backend' \
        'else' \
        '$(error unsupported EXT4_BACKEND)' \
        'endif' > "$fixture_root/os/make/ext4_backend.mk"
    printf '%s\n' \
        'pub mod ext4_backend;' \
        "$boot_route" \
        "$block_route" > "$fixture_root/os/src/fs/mod.rs"
    printf '%s\n' \
        '#[cfg(not(any(feature = "ext4_lwext4_backend", feature = "ext4_legacy_backend", feature = "ext4_another_backend")))]' \
        'compile_error!("exactly one ext4 backend must be selected");' \
        '#[cfg(any(all(feature = "ext4_lwext4_backend", feature = "ext4_legacy_backend"), all(feature = "ext4_lwext4_backend", feature = "ext4_another_backend"), all(feature = "ext4_legacy_backend", feature = "ext4_another_backend")))]' \
        'compile_error!("ext4 backend features are mutually exclusive");' \
        'pub fn open() {}' > "$fixture_root/os/src/fs/ext4_backend.rs"
    printf '%s\n' "$syscall_route" > "$fixture_root/os/src/syscall/fs/sys_mount.rs"
    cp "$gate_script" "$fixture_root/scripts/check_another_ext4_isolation.sh"
}

setup_fixture() {
    scenario=$1
    rm -rf "$fixture_root"
    mkdir -p "$fixture_root"
    git init -q "$fixture_root"
    git -C "$fixture_root" config user.email fixture@example.invalid
    git -C "$fixture_root" config user.name fixture
    write_fixture_sources "$scenario"

    git -C "$fixture_root" add .gitmodules os scripts
    git -C "$fixture_root" update-index --add --cacheinfo "160000,$submodule_commit,$submodule_path"
    git -C "$fixture_root" commit -q -m fixture
}

expect_pass() {
    name=$1
    if sh "$fixture_root/scripts/check_another_ext4_isolation.sh"; then
        printf '%s\n' "ok - $name"
    else
        fail "$name: gate unexpectedly rejected the fixture"
    fi
}

expect_reject() {
    name=$1
    if sh "$fixture_root/scripts/check_another_ext4_isolation.sh" >/dev/null 2>&1; then
        fail "$name: gate unexpectedly accepted the fixture"
    fi
    printf '%s\n' "ok - $name"
}

setup_fixture zero
expect_reject 'no ext4 backend selection'

setup_fixture mixed
expect_reject 'mixed ext4 backend selection'

setup_fixture bypass
expect_reject 'direct backend mount bypass'

setup_fixture good
expect_pass 'exactly one feature-selected facade route'

printf '%s\n' 'all hermetic ext4 backend selection gate scenarios passed'
