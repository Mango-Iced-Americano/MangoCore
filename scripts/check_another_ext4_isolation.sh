#!/usr/bin/env sh
set -eu

root_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
submodule_path='dependency/another_ext4'
submodule_url='git@github.com:Mango-Iced-Americano/another_ext4.git'
submodule_branch='mango'
submodule_commit='6887c41ef212b483a6841c87cb4d4b025b8d2c1b'
cargo_toml="$root_dir/os/Cargo.toml"
make_backend="$root_dir/os/make/ext4_backend.mk"
fs_mod="$root_dir/os/src/fs/mod.rs"
facade="$root_dir/os/src/fs/ext4_backend.rs"
mount_syscall="$root_dir/os/src/syscall/fs/sys_mount.rs"

fail() {
    printf '%s\n' "another_ext4 isolation gate: $1" >&2
    exit 1
}

require_stage_zero_file() {
    path=$1
    entry=$(git -C "$root_dir" ls-files --stage -- "$path")
    test -n "$entry" || fail "staged index has no entry for $path"
    test "$(printf '%s\n' "$entry" | awk 'END { print NR }')" = 1 || \
        fail "staged index has multiple entries for $path; resolve index conflicts first"
    test "$(printf '%s\n' "$entry" | awk '{ print $3 }')" = 0 || \
        fail "staged index entry for $path must be at stage 0"
}

require_exactly_one_default_backend() {
    default_features=$(awk '
        /^default[[:space:]]*=/ { capture = 1 }
        capture { print }
        capture && /\]/ { exit }
    ' "$cargo_toml")
    selected=$(printf '%s\n' "$default_features" | grep -o 'ext4_\(lwext4\|legacy\|another\)_backend' || true)
    count=$(printf '%s\n' "$selected" | awk 'NF { count += 1 } END { print count + 0 }')
    test "$count" = 1 || fail "Cargo default features must select exactly one ext4 backend; found $count"
    test "$selected" = ext4_lwext4_backend || \
        fail "Cargo default must select ext4_lwext4_backend; got ${selected:-none}"
}

require_feature_contract() {
    grep -Fq 'lwext4_rust = { path = "../dependency/lwext4_rust", default-features = false, optional = true }' "$cargo_toml" || \
        fail 'Cargo must expose lwext4_rust as an optional backend dependency'
    grep -Fq 'another_ext4 = { path = "../dependency/another_ext4", optional = true }' "$cargo_toml" || \
        fail 'Cargo must expose another_ext4 as an optional backend dependency'
    grep -Fq 'ext4_lwext4_backend = ["dep:lwext4_rust"]' "$cargo_toml" || \
        fail 'Cargo must define ext4_lwext4_backend'
    grep -Fq 'ext4_legacy_backend = []' "$cargo_toml" || \
        fail 'Cargo must define ext4_legacy_backend'
    grep -Fq 'ext4_another_backend = ["dep:another_ext4"]' "$cargo_toml" || \
        fail 'Cargo must define ext4_another_backend'
    require_exactly_one_default_backend
}

require_make_mapping() {
    test -f "$make_backend" || fail 'missing EXT4_BACKEND Make mapping'
    grep -Fq 'EXT4_BACKEND ?= lwext4' "$make_backend" || fail 'Make default must be lwext4'
    grep -Fq 'EXT4_BACKEND_FEATURE := ext4_lwext4_backend' "$make_backend" || fail 'Make lacks lwext4 mapping'
    grep -Fq 'EXT4_BACKEND_FEATURE := ext4_legacy_backend' "$make_backend" || fail 'Make lacks legacy mapping'
    grep -Fq 'EXT4_BACKEND_FEATURE := ext4_another_backend' "$make_backend" || fail 'Make lacks another mapping'
}

require_facade_routes() {
    test -f "$facade" || fail 'missing shared ext4 backend facade'
    grep -Fq 'compile_error!' "$facade" || fail 'facade lacks compile-time selection guards'
    grep -Fq 'pub fn open' "$facade" || fail 'facade lacks ext4 open entrypoint'
    test "$(grep -Fc 'self::ext4_backend::open(' "$fs_mod")" = 2 || \
        fail 'boot and mount_block_fs must each call the facade once'
    grep -Fq 'crate::fs::ext4_backend::open(' "$mount_syscall" || \
        fail 'sys_mount must call the shared facade'
    if grep -Eq 'ext4_lwext4::ext4fs::Ext4FileSystem::open_ext4rs|ext4::ext4fs::Ext4FileSystem::open_ext4rs' "$fs_mod" "$mount_syscall"; then
        fail 'boot, mount_block_fs, or sys_mount bypasses the shared facade'
    fi
    if grep -Fq 'another_ext4::' "$facade" "$fs_mod" "$mount_syscall"; then
        fail 'another_ext4 must remain feature-selected, not runtime-selected'
    fi
}

require_stage_zero_file '.gitmodules'
test -f "$root_dir/.gitmodules" || fail 'missing .gitmodules; pinned Mango fork submodule is not configured'
git -C "$root_dir" diff --quiet -- .gitmodules || \
    fail 'worktree .gitmodules differs from the staged index snapshot'

configured_url=$(git -C "$root_dir" config --file .gitmodules --get "submodule.$submodule_path.url" || true)
test "$configured_url" = "$submodule_url" || \
    fail "expected .gitmodules URL for $submodule_path to be $submodule_url; got ${configured_url:-unset}"

configured_branch=$(git -C "$root_dir" config --file .gitmodules --get "submodule.$submodule_path.branch" || true)
test "$configured_branch" = "$submodule_branch" || \
    fail "expected .gitmodules branch for $submodule_path to be $submodule_branch; got ${configured_branch:-unset}"

gitlink_entry=$(git -C "$root_dir" ls-files --stage -- "$submodule_path")
test -n "$gitlink_entry" || fail "staged index has no gitlink for $submodule_path; add the pinned submodule before wiring it"
gitlink_commit=$(printf '%s\n' "$gitlink_entry" | awk '{ print $2 }')
test "$gitlink_commit" = "$submodule_commit" || \
    fail "staged index gitlink for $submodule_path must be pinned to $submodule_commit; got $gitlink_commit"

require_feature_contract
require_make_mapping
require_facade_routes

printf '%s\n' "another_ext4 isolation gate: pass (pinned commit $gitlink_commit)"
