#!/bin/sh
set -eu

usage() {
    printf '%s\n' "usage: $0 --allowlist FILE --repo-root DIR --verify-fingerprints" >&2
    printf '%s\n' "       $0 --fixture staged-unowned" >&2
    exit 2
}

fail() {
    printf '%s\n' "FAIL: $*" >&2
    exit 1
}

run_staged_unowned_fixture() {
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/rebaseline-isolation-fixture.XXXXXX")
    trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
    repo=$fixture_root/repo
    mkdir -p "$repo"
    git -C "$repo" init -q
    git -C "$repo" config user.email rebaseline@example.invalid
    git -C "$repo" config user.name rebaseline-test
    printf '%s\n' base >"$repo/tracked.txt"
    git -C "$repo" add tracked.txt
    git -C "$repo" commit -qm baseline
    printf '%s\n' staged >"$repo/staged.txt"
    git -C "$repo" add staged.txt
    : >"$fixture_root/allowlist"

    if "$0" --allowlist "$fixture_root/allowlist" --repo-root "$repo" --verify-fingerprints \
        >"$fixture_root/output" 2>&1; then
        fail 'staged-unowned fixture was accepted'
    fi
    grep -F 'FAIL: dirty path is not allowlisted: staged.txt' "$fixture_root/output" >/dev/null ||
        fail 'staged-unowned fixture did not identify staged.txt'
    printf '%s\n' 'PASS: staged-unowned fixture rejected'
}

if [ "$#" -eq 2 ] && [ "$1" = '--fixture' ] && [ "$2" = 'staged-unowned' ]; then
    run_staged_unowned_fixture
    exit 0
fi

[ "$#" -eq 5 ] || usage
[ "$1" = '--allowlist' ] || usage
allowlist=$2
[ "$3" = '--repo-root' ] || usage
repo_root=$4
[ "$5" = '--verify-fingerprints' ] || usage

[ -r "$allowlist" ] || fail "allowlist is not readable: $allowlist"
repo_root=$(CDPATH= cd -- "$repo_root" && pwd)
git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
    fail "repo root is not a Git worktree: $repo_root"

allowlist_entries=$(mktemp "${TMPDIR:-/tmp}/rebaseline-allowlist.XXXXXX")
trap 'rm -f "$allowlist_entries"' EXIT HUP INT TERM

while IFS=' ' read -r path fingerprint disposition extra; do
    case "$path" in
        ''|'#'*) continue ;;
    esac
    [ -z "${extra:-}" ] || fail "malformed allowlist entry: $path"
    case "$path" in
        /*|*'..'*|*'//'*) fail "unsafe allowlist path: $path" ;;
    esac
    case "$disposition" in
        outside-rebaseline|reconcile-before) ;;
        *) fail "invalid allowlist disposition for $path: $disposition" ;;
    esac
    case "$fingerprint" in
        DELETED|????????????????????????????????????????????????????????????????) ;;
        *) fail "invalid SHA-256 for $path" ;;
    esac
    if grep -Fqx "$path $fingerprint $disposition" "$allowlist_entries"; then
        fail "duplicate allowlist path: $path"
    fi
    printf '%s %s %s\n' "$path" "$fingerprint" "$disposition" >>"$allowlist_entries"
done <"$allowlist"

status_file=$(mktemp "${TMPDIR:-/tmp}/rebaseline-status.XXXXXX")
trap 'rm -f "$allowlist_entries" "$status_file"' EXIT HUP INT TERM
git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$status_file" ||
    fail 'git status failed'

while IFS= read -r status_line; do
    [ -n "$status_line" ] || continue
    status=${status_line%${status_line#??}}
    path=${status_line#???}
    case "$status" in
        R*|C*|?R|?C) fail "rename/copy status requires reconciliation: $path" ;;
    esac
    entry=$(grep -F "${path} " "$allowlist_entries" || true)
    [ -n "$entry" ] || fail "dirty path is not allowlisted: $path"
    set -- $entry
    expected_hash=$2
    disposition=$3
    [ "$disposition" = 'outside-rebaseline' ] ||
        fail "dirty path requires reconciliation before execution: $path"
    case "$status" in
        *D*|D*)
            [ "$expected_hash" = 'DELETED' ] || fail "deleted path must use DELETED fingerprint: $path"
            ;;
        *)
            [ "$expected_hash" != 'DELETED' ] || fail "existing path cannot use DELETED fingerprint: $path"
            actual_hash=$(sha256sum "$repo_root/$path" | cut -d ' ' -f 1) ||
                fail "cannot hash allowlisted path: $path"
            [ "$actual_hash" = "$expected_hash" ] || fail "fingerprint mismatch: $path"
            ;;
    esac
done <"$status_file"

printf '%s\n' 'PASS: rebaseline isolation allowlist verified'
