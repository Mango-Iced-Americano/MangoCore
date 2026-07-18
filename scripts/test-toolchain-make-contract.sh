#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

makefiles='Makefile os/Makefile os/make/rv64.mk os/make/la64.mk user/Makefile'
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/toolchain-make-contract.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
overall=0

fail() {
    echo "FAIL: $*" >&2
    overall=1
}

pass() {
    echo "PASS: $*"
}

require_match() {
    file=$1
    pattern=$2
    description=$3
    if grep -Eq "$pattern" "$repo_root/$file"; then
        pass "$description"
    else
        fail "$description ($file)"
    fi
}

require_absent() {
    pattern=$1
    description=$2
    for makefile in $makefiles; do
        if grep -En "$pattern" "$repo_root/$makefile" >/dev/null; then
            fail "$description ($makefile)"
            return
        fi
    done
    pass "$description"
}

require_dependency() {
    file=$1
    target=$2
    prerequisite=$3
    if awk -v target="$target" -v prerequisite="$prerequisite" '
        /^[^[:space:]#][^:]*:/ {
            split($0, parts, ":")
            count = split(parts[1], targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target && index(" " $0 " ", " " prerequisite " ")) {
                    found = 1
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"; then
        pass "$file: $target requires $prerequisite"
    else
        fail "$file: $target must require $prerequisite"
    fi
}

require_root_all_recipe_order() {
    if awk '
        /^all:[[:space:]]*toolchain-preflight[[:space:]]*$/ {
            header = 1
            next
        }
        header && /^\t/ {
            if (stage == 0 && $0 ~ /\$\(MAKE\)[[:space:]]+prepare-cargo-config/) {
                stage = 1
            } else if (stage == 1 && $0 ~ /\$\(MAKE\)[[:space:]]+clean/) {
                stage = 2
            } else if (stage == 2 && $0 ~ /\$\(MAKE\)[[:space:]]+-C[[:space:]]+os[[:space:]]+all/) {
                stage = 3
            }
            next
        }
        header && /^[^[:space:]#]/ {
            exit(stage == 3 ? 0 : 1)
        }
        END { exit(header && stage == 3 ? 0 : 1) }
    ' "$repo_root/Makefile"; then
        pass 'root all orders preflight, prepare-cargo-config, clean, then os all'
    else
        fail 'root all must order preflight, prepare-cargo-config, clean, then os all'
    fi
}

for makefile in $makefiles; do
    if [ -r "$repo_root/$makefile" ]; then
        pass "read $makefile"
    else
        fail "missing Makefile $makefile"
    fi
done

for makefile in $makefiles; do
    require_match "$makefile" \
        '^[[:space:]]*export[[:space:]]+RUSTUP_AUTO_INSTALL[[:space:]]*:=[[:space:]]*0[[:space:]]*$' \
        "$makefile exports RUSTUP_AUTO_INSTALL=0"
    require_match "$makefile" \
        '^[[:space:]]*unexport[[:space:]]+RUSTUP_TOOLCHAIN[[:space:]]*$' \
        "$makefile unexports RUSTUP_TOOLCHAIN"
done

require_absent 'rustup[[:space:]]+(default|override[[:space:]]+set|target[[:space:]]+add|component[[:space:]]+add)' \
    'no first-party Makefile provisions or mutates Rustup'
require_absent '^[[:space:]]*[-@]*-[[:space:]]*\(?rustup[[:space:]]' \
    'no first-party Makefile ignores Rustup failures with a Make error prefix'
require_absent 'rustup[[:space:]].*\|\|[[:space:]]*(true|:)' \
    'no first-party Makefile ignores Rustup failures'

setup_calls=0
for makefile in $makefiles; do
    count=$(grep -Ec '^[[:space:]]*@?sh[[:space:]]+[^[:space:]]*rustup-setup\.sh[[:space:]]*$' \
        "$repo_root/$makefile" || true)
    setup_calls=$((setup_calls + count))
done
if [ "$setup_calls" -eq 1 ] \
    && grep -Eq '^[[:space:]]*@?sh[[:space:]]+scripts/rustup-setup\.sh[[:space:]]*$' "$repo_root/Makefile" \
    && ! grep -Eq 'rustup-setup\.sh' "$repo_root/os/Makefile" "$repo_root/os/make/rv64.mk" \
        "$repo_root/os/make/la64.mk" "$repo_root/user/Makefile"; then
    pass 'root Makefile is the only setup-script caller'
else
    fail 'root Makefile must be the only setup-script caller'
fi

require_dependency Makefile all toolchain-preflight
require_dependency Makefile env toolchain-preflight
require_dependency Makefile kernel toolchain-preflight
require_dependency Makefile run toolchain-preflight
require_dependency Makefile runsimple toolchain-preflight
require_dependency Makefile change-kernel-only toolchain-preflight
require_dependency Makefile regression toolchain-preflight
require_dependency Makefile check-fast toolchain-preflight
require_dependency Makefile unittest toolchain-preflight
require_root_all_recipe_order
require_match os/Makefile '^[[:space:]]*\.NOTPARALLEL:[[:space:]]*$' \
    'os/Makefile disables parallel execution'
require_match os/Makefile \
    '^[[:space:]]*all:[[:space:]]+prepare-cargo-config[[:space:]]+rv64_all[[:space:]]+la64_all[[:space:]]*$' \
    'os all orders prepare-cargo-config, rv64_all, then la64_all'
require_dependency os/Makefile prepare-cargo-config toolchain-preflight
require_dependency os/Makefile env toolchain-preflight
require_dependency os/Makefile rv64-debug toolchain-preflight
require_dependency os/Makefile la64-debug toolchain-preflight
require_dependency os/Makefile rv64-run-only toolchain-preflight
require_dependency os/Makefile la64-run-only toolchain-preflight
require_dependency os/Makefile comp toolchain-preflight
require_dependency os/Makefile fs-img toolchain-preflight
require_dependency os/Makefile run toolchain-preflight
require_dependency os/Makefile gdb toolchain-preflight
require_dependency os/Makefile la64-inject-runtime toolchain-preflight
require_dependency os/Makefile inject-test toolchain-preflight
require_dependency os/make/rv64.mk all toolchain-preflight
require_dependency os/make/rv64.mk env toolchain-preflight
require_dependency os/make/rv64.mk user toolchain-preflight
require_dependency os/make/rv64.mk fs-img toolchain-preflight
require_dependency os/make/rv64.mk kernel toolchain-preflight
require_dependency os/make/rv64.mk run toolchain-preflight
require_dependency os/make/rv64.mk runsimple toolchain-preflight
require_dependency os/make/rv64.mk comp toolchain-preflight
require_dependency os/make/rv64.mk comp-gdb toolchain-preflight
require_dependency os/make/rv64.mk ktest-run toolchain-preflight
require_dependency os/make/rv64.mk regression-run toolchain-preflight
require_dependency os/make/la64.mk all toolchain-preflight
require_dependency os/make/la64.mk env toolchain-preflight
require_dependency os/make/la64.mk user toolchain-preflight
require_dependency os/make/la64.mk fs-img toolchain-preflight
require_dependency os/make/la64.mk kernel toolchain-preflight
require_dependency os/make/la64.mk run toolchain-preflight
require_dependency os/make/la64.mk runsimple toolchain-preflight
require_dependency os/make/la64.mk comp toolchain-preflight
require_dependency os/make/la64.mk comp-gdb toolchain-preflight
require_dependency os/make/la64.mk ktest-run toolchain-preflight
require_dependency os/make/la64.mk regression-run toolchain-preflight
require_dependency user/Makefile env toolchain-preflight
require_dependency user/Makefile rust-user env

has_setup_dependency=0
for makefile in $makefiles; do
    if ! awk '
    /^[^[:space:]#][^:]*:/ && $0 !~ /^(\.PHONY|toolchain-setup):/ && index($0, "toolchain-setup") {
        violates = 1
    }
    END { exit(violates ? 1 : 0) }
' "$repo_root/$makefile"; then
        has_setup_dependency=1
    fi
done
if [ "$has_setup_dependency" -eq 0 ]; then
    pass 'normal targets depend on preflight rather than setup'
else
    fail 'normal targets must not depend on toolchain-setup'
fi

fixture_root=$work_dir/fixture
fixture_rustup_home=$work_dir/rustup-home
fixture_path=$work_dir/bin:/usr/bin:/bin
mkdir -p "$fixture_root/os/make" "$fixture_root/user/src" "$fixture_root/scripts" \
    "$work_dir/bin" "$fixture_rustup_home"
for makefile in $makefiles; do
    mkdir -p "$fixture_root/$(dirname "$makefile")"
    cp "$repo_root/$makefile" "$fixture_root/$makefile"
done
sed -i \
    -e "s|rustc -vV|PATH=$fixture_path rustc -vV|" \
    -e "s|rustc --print sysroot|PATH=$fixture_path rustc --print sysroot|" \
    "$fixture_root/os/make/rv64.mk"
printf '\ntoolchain-contract-rustc-probe: env\n\t@: $(HOST_TRIPLE) $(LLVM_TOOLS_DIR)\n' \
    >>"$fixture_root/os/make/rv64.mk"
cat >"$fixture_root/os/make/toolchain-contract.mk" <<EOF
PATH := $fixture_path
export PATH
RUSTUP_HOME := $fixture_rustup_home
export RUSTUP_HOME
FAKE_RUSTUP_HOME := $fixture_rustup_home
export FAKE_RUSTUP_HOME
FAKE_PREFLIGHT_LOG := $work_dir/preflight.log
export FAKE_PREFLIGHT_LOG
FAKE_RUSTUP_LOG := $work_dir/rustup.log
export FAKE_RUSTUP_LOG
EOF
: >"$fixture_root/user/src/lang_items.rs.rv"
: >"$fixture_root/user/src/lang_items.rs.la"

cat >"$fixture_root/scripts/rustup-preflight.sh" <<'EOF'
#!/bin/sh
set -eu

if [ "${RUSTUP_AUTO_INSTALL-}" != 0 ]; then
    echo "fake preflight: RUSTUP_AUTO_INSTALL was not forced to 0" >&2
    exit 90
fi
if [ "${RUSTUP_TOOLCHAIN+x}" = x ]; then
    echo "fake preflight: caller RUSTUP_TOOLCHAIN leaked into recipe" >&2
    exit 91
fi
if [ "${RUSTUP_HOME-}" != "${FAKE_RUSTUP_HOME:?}" ]; then
    echo "fake preflight: RUSTUP_HOME was not the harness-owned directory" >&2
    exit 92
fi
printf '%s\n' "${FAKE_PREFLIGHT_LABEL:?}" >>"${FAKE_PREFLIGHT_LOG:?}"
EOF
chmod +x "$fixture_root/scripts/rustup-preflight.sh"

cat >"$work_dir/bin/rustup" <<'EOF'
#!/bin/sh
set -eu

if [ "${RUSTUP_HOME-}" != "${FAKE_RUSTUP_HOME:?}" ]; then
    echo "fake rustup: RUSTUP_HOME was not the harness-owned directory" >&2
    exit 96
fi
printf '%s\n' "$*" >>"${FAKE_RUSTUP_LOG:?}"
exit 97
EOF
chmod +x "$work_dir/bin/rustup"

cat >"$work_dir/bin/rustc" <<'EOF'
#!/bin/sh
set -eu

rustup_toolchain_state=unset
if [ "${RUSTUP_TOOLCHAIN+x}" = x ]; then
    rustup_toolchain_state=set
fi
if [ -n "${RUSTUP_HOME-}" ] && [ -d "$RUSTUP_HOME" ]; then
    printf '%s\t%s\t%s\n' "$*" "${RUSTUP_AUTO_INSTALL-}" "$rustup_toolchain_state" \
        >>"$RUSTUP_HOME/rustc.log"
fi
if [ "${RUSTUP_AUTO_INSTALL-}" != 0 ]; then
    echo "fake rustc: RUSTUP_AUTO_INSTALL was not forced to 0" >&2
    exit 98
fi
if [ "${RUSTUP_TOOLCHAIN+x}" = x ]; then
    echo "fake rustc: caller RUSTUP_TOOLCHAIN leaked into parse-time command" >&2
    exit 99
fi
if [ -z "${RUSTUP_HOME-}" ] || [ ! -d "$RUSTUP_HOME" ]; then
    echo "fake rustc: RUSTUP_HOME was not the harness-owned directory" >&2
    exit 100
fi
case "$*" in
    -vV)
        printf '%s\n' 'rustc 1.85.0-fake' 'host: x86_64-unknown-linux-gnu'
        ;;
    '--print sysroot')
        printf '%s\n' /tmp/toolchain-make-contract-sysroot
        ;;
    *)
        echo "fake rustc: unsupported arguments: $*" >&2
        exit 101
        ;;
esac
EOF
chmod +x "$work_dir/bin/rustc"

: >"$work_dir/preflight.log"
: >"$work_dir/rustup.log"
: >"$fixture_rustup_home/rustc.log"

run_env() {
    label=$1
    directory=$2
    shift 2
    if (
        cd "$directory"
        PATH="$fixture_path" \
            RUSTUP_AUTO_INSTALL=1 \
            RUSTUP_TOOLCHAIN=caller-selected \
            RUSTUP_HOME="$fixture_rustup_home" \
            FAKE_RUSTUP_HOME="$fixture_rustup_home" \
            FAKE_PREFLIGHT_LABEL="$label" \
            FAKE_PREFLIGHT_LOG="$work_dir/preflight.log" \
            FAKE_RUSTUP_LOG="$work_dir/rustup.log" \
            "$@" \
            "PATH=$fixture_path" \
            "RUSTUP_HOME=$fixture_rustup_home" \
            "FAKE_RUSTUP_HOME=$fixture_rustup_home" \
            "FAKE_PREFLIGHT_LOG=$work_dir/preflight.log" \
            "FAKE_RUSTUP_LOG=$work_dir/rustup.log"
    ) >"$work_dir/$label.out" 2>&1; then
        pass "$label env is read-only"
    else
        fail "$label env must invoke read-only preflight without Rustup mutation"
        cat "$work_dir/$label.out" >&2
    fi
}

run_env root "$fixture_root" make env
run_env os "$fixture_root/os" make env
run_env rv64 "$fixture_root/os" make -f make/toolchain-contract.mk -f make/rv64.mk env
run_env rv64-probe "$fixture_root/os" make -f make/toolchain-contract.mk -f make/rv64.mk toolchain-contract-rustc-probe
run_env la64 "$fixture_root/os" make -f make/la64.mk env
run_env user "$fixture_root/user" make env

expected_labels='root
os
rv64
rv64-probe
la64
user'
if cmp -s "$work_dir/preflight.log" /dev/null; then
    fail 'fake preflight was not invoked'
elif [ "$(sort "$work_dir/preflight.log")" = "$(printf '%s\n' "$expected_labels" | sort)" ]; then
    pass 'all env targets receive the constrained Rustup environment'
else
    fail 'all env targets must invoke fake preflight exactly once'
fi

if [ ! -s "$work_dir/rustup.log" ]; then
    pass 'fake Rustup received no normal-path invocation'
else
    fail 'fake Rustup was invoked by a normal env target'
    cat "$work_dir/rustup.log" >&2
fi

if awk -F '\t' '
    $1 == "-vV" && $2 == "0" && $3 == "unset" { host = 1 }
    $1 == "--print sysroot" && $2 == "0" && $3 == "unset" { sysroot = 1 }
    END { exit(host && sysroot ? 0 : 1) }
' "$fixture_rustup_home/rustc.log"; then
    pass 'fake rustc handled constrained RV64 parse-time probes'
else
    fail 'RV64 parse-time probes must use the constrained fake rustc'
    cat "$fixture_rustup_home/rustc.log" >&2
fi

exit "$overall"
