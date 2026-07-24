#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
contract=$script_dir/test-rebaseline-isolation.sh
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/rebaseline-isolation.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM

repo=$fixture_root/repo
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email rebaseline@example.invalid
git -C "$repo" config user.name rebaseline-test
printf '%s\n' base >"$repo/tracked.txt"
git -C "$repo" add tracked.txt
git -C "$repo" commit -qm baseline

printf '%s\n' protected >"$repo/tracked.txt"
hash=$(sha256sum "$repo/tracked.txt" | cut -d ' ' -f 1)
printf '%s %s outside-rebaseline\n' tracked.txt "$hash" >"$fixture_root/allowlist"

"$contract" --allowlist "$fixture_root/allowlist" --repo-root "$repo" --verify-fingerprints

printf '%s\n' unexpected >"$repo/untracked.txt"
if "$contract" --allowlist "$fixture_root/allowlist" --repo-root "$repo" --verify-fingerprints \
    >"$fixture_root/unexpected.out" 2>&1; then
    printf '%s\n' 'FAIL: unexpected dirty path fixture was accepted' >&2
    exit 1
fi
grep -F 'FAIL: dirty path is not allowlisted: untracked.txt' "$fixture_root/unexpected.out" >/dev/null

printf '%s\n' changed >"$repo/tracked.txt"
if "$contract" --allowlist "$fixture_root/allowlist" --repo-root "$repo" --verify-fingerprints \
    >"$fixture_root/fingerprint.out" 2>&1; then
    printf '%s\n' 'FAIL: changed protected path fixture was accepted' >&2
    exit 1
fi
grep -F 'FAIL: fingerprint mismatch: tracked.txt' "$fixture_root/fingerprint.out" >/dev/null

printf '%s\n' 'PASS: rebaseline isolation contract self-test'
