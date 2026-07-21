#!/bin/sh
set -eu

usage() {
    printf '%s\n' 'usage: test-rebaseline-purity-delta.sh --serial-kernel-builds' >&2
    printf '%s\n' '       test-rebaseline-purity-delta.sh --fixture tracked-mutation|gitlink-movement' >&2
    exit 2
}

fail() {
    printf '%s\n' "FAIL: $*" >&2
    exit 1
}

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=${REBASELINE_PURITY_REPO_ROOT:-$(CDPATH= cd -- "$script_dir/.." && pwd)}
allowlist=${REBASELINE_PURITY_ALLOWLIST:-$repo_root/.omo/rebaseline-allowlist.txt}
make_cmd=${REBASELINE_PURITY_MAKE:-make}

is_allowlisted_deleted() {
    awk -v candidate="$1" '
        $1 == candidate && $2 == "DELETED" { found = 1 }
        END { exit(found ? 0 : 1) }
    ' "$allowlist"
}

snapshot_tracked() {
    stage_file=$(mktemp "${TMPDIR:-/tmp}/rebaseline-purity-stage.XXXXXX")
    trap 'rm -f "$stage_file"' EXIT HUP INT TERM
    git -C "$repo_root" ls-files --stage >"$stage_file" ||
        fail 'cannot read tracked-file stage entries'

    tab=$(printf '\t')
    while IFS="$tab" read -r metadata path; do
        [ -n "$metadata" ] || fail 'malformed empty tracked-file stage entry'
        [ -n "$path" ] || fail "missing path in tracked-file stage entry: $metadata"
        case "$path" in
            *"$tab"*|*'
'*) fail "unsafe tracked-file path encoding: $path" ;;
        esac
        set -- $metadata
        [ "$#" -eq 3 ] || fail "malformed tracked-file metadata: $metadata"
        mode=$1
        index_oid=$2
        stage=$3
        [ "$stage" = 0 ] || fail "unsupported nonzero index stage for $path"

        case "$mode" in
            160000)
                gitlink_head=$(git -C "$repo_root/$path" rev-parse --verify 'HEAD^{commit}') ||
                    fail "gitlink HEAD is unavailable: $path"
                printf 'GITLINK\t%s\t%s\t%s\n' "$index_oid" "$gitlink_head" "$path"
                ;;
            *)
                if [ ! -e "$repo_root/$path" ] && [ ! -L "$repo_root/$path" ]; then
                    is_allowlisted_deleted "$path" ||
                        fail "tracked file is missing or unreadable: $path"
                    printf 'MISSING\t%s\n' "$index_oid" "$path"
                else
                    worktree_oid=$(git -C "$repo_root" hash-object --no-filters -- "$path") ||
                        fail "tracked file is missing or unreadable: $path"
                    printf 'FILE\t%s\t%s\t%s\n' "$index_oid" "$worktree_oid" "$path"
                fi
                ;;
        esac
    done <"$stage_file"
}

verify_isolation() {
    sh "$script_dir/test-rebaseline-isolation.sh" \
        --allowlist "$allowlist" \
        --repo-root "$repo_root" \
        --verify-fingerprints
}

run_serial_builds() {
    "$make_cmd" -B -C "$repo_root/os" rv64-kernel-build-only
    "$make_cmd" -B -C "$repo_root/os" la64-kernel-build-only
}

run_tracked_mutation_fixture() {
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/rebaseline-purity-tracked.XXXXXX")
    trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
    repo=$fixture_root/repo
    mkdir -p "$repo/os" "$repo/scripts" "$repo/bin"
    git -C "$repo" init -q
    git -C "$repo" config user.email rebaseline@example.invalid
    git -C "$repo" config user.name rebaseline-test
    printf '%s\n' base >"$repo/tracked file"
    git -C "$repo" add 'tracked file'
    git -C "$repo" commit -qm baseline
    : >"$repo/allowlist"
    cp "$script_dir/test-rebaseline-isolation.sh" "$repo/scripts/test-rebaseline-isolation.sh"
    cp "$0" "$repo/scripts/test-rebaseline-purity-delta.sh"
    cat >"$repo/bin/make" <<'EOF'
#!/bin/sh
set -eu
case " $* " in
    *' rv64-kernel-build-only '*) printf '%s\n' mutated >"${PURITY_FIXTURE_TRACKED_FILE:?}" ;;
esac
EOF
    chmod +x "$repo/bin/make"
    git -C "$repo" add scripts bin allowlist
    git -C "$repo" commit -qm fixture-helpers

    set +e
    REBASELINE_PURITY_REPO_ROOT="$repo" \
        REBASELINE_PURITY_ALLOWLIST="$repo/allowlist" \
        REBASELINE_PURITY_MAKE="$repo/bin/make" \
        PURITY_FIXTURE_TRACKED_FILE="$repo/tracked file" \
        sh "$repo/scripts/test-rebaseline-purity-delta.sh" --serial-kernel-builds \
        >"$fixture_root/output" 2>&1
    fixture_status=$?
    set -e
    [ "$fixture_status" -ne 0 ] || fail 'tracked-mutation fixture was accepted'
    grep -F 'FAIL: serial RV64 then LA64 kernel builds changed a tracked source, vendor, or configuration path' \
        "$fixture_root/output" >/dev/null ||
        {
            cat "$fixture_root/output" >&2
            fail 'tracked-mutation fixture did not report the tracked-file delta'
        }
    fail 'tracked-mutation fixture confirmed rejection of a changed tracked file'
}

run_gitlink_movement_fixture() {
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/rebaseline-purity-gitlink.XXXXXX")
    trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
    module_source=$fixture_root/module-source
    repo=$fixture_root/repo
    mkdir -p "$module_source" "$repo/os" "$repo/scripts" "$repo/bin"
    git -C "$module_source" init -q
    git -C "$module_source" config user.email rebaseline@example.invalid
    git -C "$module_source" config user.name rebaseline-test
    printf '%s\n' first >"$module_source/payload"
    git -C "$module_source" add payload
    git -C "$module_source" commit -qm first
    old_head=$(git -C "$module_source" rev-parse HEAD)
    printf '%s\n' second >"$module_source/payload"
    git -C "$module_source" commit -am second -q
    index_oid=$(git -C "$module_source" rev-parse HEAD)

    git -C "$repo" init -q
    git -C "$repo" config user.email rebaseline@example.invalid
    git -C "$repo" config user.name rebaseline-test
    git clone -q "$module_source" "$repo/module"
    git -C "$repo" update-index --add --cacheinfo "160000,$index_oid,module"
    git -C "$repo" commit -qm baseline
    : >"$repo/allowlist"
    cp "$script_dir/test-rebaseline-isolation.sh" "$repo/scripts/test-rebaseline-isolation.sh"
    cp "$0" "$repo/scripts/test-rebaseline-purity-delta.sh"
    cat >"$repo/bin/make" <<'EOF'
#!/bin/sh
set -eu
case " $* " in
    *' rv64-kernel-build-only '*) git -C "${PURITY_FIXTURE_GITLINK:?}" checkout -q "${PURITY_FIXTURE_GITLINK_OLD_HEAD:?}" ;;
esac
EOF
    chmod +x "$repo/bin/make"
    git -C "$repo" add scripts bin allowlist
    git -C "$repo" commit -qm fixture-helpers

    set +e
    REBASELINE_PURITY_REPO_ROOT="$repo" \
        REBASELINE_PURITY_ALLOWLIST="$repo/allowlist" \
        REBASELINE_PURITY_MAKE="$repo/bin/make" \
        PURITY_FIXTURE_GITLINK="$repo/module" \
        PURITY_FIXTURE_GITLINK_OLD_HEAD="$old_head" \
        sh "$repo/scripts/test-rebaseline-purity-delta.sh" --serial-kernel-builds \
        >"$fixture_root/output" 2>&1
    fixture_status=$?
    set -e
    [ "$fixture_status" -ne 0 ] || fail 'gitlink-movement fixture was accepted'
    grep -F 'FAIL: serial RV64 then LA64 kernel builds changed a tracked source, vendor, or configuration path' \
        "$fixture_root/output" >/dev/null ||
        {
            cat "$fixture_root/output" >&2
            fail 'gitlink-movement fixture did not report the gitlink HEAD delta'
        }
    fail 'gitlink-movement fixture confirmed rejection of a moved gitlink HEAD'
}

case "${1:-}" in
    --serial-kernel-builds)
        [ "$#" -eq 1 ] || usage
        ;;
    --fixture)
        [ "$#" -eq 2 ] || usage
        case "$2" in
            tracked-mutation) run_tracked_mutation_fixture ;;
            gitlink-movement) run_gitlink_movement_fixture ;;
            *) usage ;;
        esac
        ;;
    *) usage ;;
esac

snapshot_before=$(mktemp "${TMPDIR:-/tmp}/rebaseline-purity-before.XXXXXX")
snapshot_after=$(mktemp "${TMPDIR:-/tmp}/rebaseline-purity-after.XXXXXX")
trap 'rm -f "$snapshot_before" "$snapshot_after"' EXIT HUP INT TERM

verify_isolation
snapshot_tracked >"$snapshot_before"
run_serial_builds
snapshot_tracked >"$snapshot_after"
cmp -s "$snapshot_before" "$snapshot_after" ||
    fail 'serial RV64 then LA64 kernel builds changed a tracked source, vendor, or configuration path'
verify_isolation

printf '%s\n' 'PASS: forced serial RV64 then LA64 kernel builds preserve tracked sources, gitlink HEADs, configuration, and protected candidates'
