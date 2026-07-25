#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

required_makefiles='Makefile os/Makefile os/make/rv64.mk os/make/la64.mk user/Makefile'
makefiles=$(find "$repo_root" \
    \( -path "$repo_root/.git" -o -path "$repo_root/dependency" -o -path "$repo_root/testresult" -o -path "$repo_root/testresults" \) -prune -o \
    -type f \( -name Makefile -o -name GNUmakefile -o -name '*.mk' \) -print \
    | LC_ALL=C sort \
    | while IFS= read -r makefile; do
        printf '%s\n' "${makefile#"$repo_root"/}"
    done)
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
        /^all:[[:space:]]*toolchain-setup[[:space:]]*$/ {
            header = 1
            next
        }
        header && /^\t/ {
            if (stage == 0 && $0 ~ /\$\(MAKE\)[[:space:]]+prepare-cargo-config/) {
                stage = 1
            } else if ($0 ~ /\$\(MAKE\)[[:space:]]+clean/) {
                clean = 1
            } else if (stage == 1 && $0 ~ /\$\(MAKE\)[[:space:]]+-C[[:space:]]+os[[:space:]]+all/) {
                stage = 2
            }
            next
        }
        header && /^[^[:space:]#]/ {
            exit(stage == 2 && !clean ? 0 : 1)
        }
        END { exit(header && stage == 2 && !clean ? 0 : 1) }
    ' "$repo_root/Makefile"; then
        pass 'root all orders setup, prepare-cargo-config, then os all without clean'
    else
        fail 'root all must order setup, prepare-cargo-config, then os all without a clean edge'
    fi
}

for makefile in $required_makefiles; do
    if [ -r "$repo_root/$makefile" ]; then
        pass "read $makefile"
    else
        fail "missing Makefile $makefile"
    fi
done

common_toolchain=$repo_root/os/make/common/toolchain.mk
if [ -r "$common_toolchain" ]; then
    require_match os/make/common/toolchain.mk \
        '^[[:space:]]*export[[:space:]]+RUSTUP_AUTO_INSTALL[[:space:]]*:=[[:space:]]*0[[:space:]]*$' \
        'common/toolchain.mk declares RUSTUP_AUTO_INSTALL=0 authoritatively'
    require_match os/make/common/toolchain.mk \
        '^[[:space:]]*unexport[[:space:]]+RUSTUP_TOOLCHAIN[[:space:]]*$' \
        'common/toolchain.mk unexports RUSTUP_TOOLCHAIN authoritatively'
else
    for makefile in $required_makefiles; do
        require_match "$makefile" \
            '^[[:space:]]*export[[:space:]]+RUSTUP_AUTO_INSTALL[[:space:]]*:=[[:space:]]*0[[:space:]]*$' \
            "$makefile exports RUSTUP_AUTO_INSTALL=0"
        require_match "$makefile" \
            '^[[:space:]]*unexport[[:space:]]+RUSTUP_TOOLCHAIN[[:space:]]*$' \
            "$makefile unexports RUSTUP_TOOLCHAIN"
    done
fi

require_absent 'rustup[[:space:]]+(default|override[[:space:]]+set|toolchain[[:space:]]+install|target[[:space:]]+add|component[[:space:]]+add)' \
    'no first-party Makefile provisions or mutates Rustup'
require_absent '^[[:space:]]*[-@]*-[[:space:]]*\(?rustup[[:space:]]' \
    'no first-party Makefile ignores Rustup failures with a Make error prefix'
require_absent 'rustup[[:space:]].*\|\|[[:space:]]*(true|:)' \
    'no first-party Makefile ignores Rustup failures'

setup_calls=0
for makefile in $makefiles; do
    count=$(grep -Ec '^[[:space:]]*[^#].*rustup-setup\.sh' \
        "$repo_root/$makefile" || true)
    setup_calls=$((setup_calls + count))
done
# Count how many makefiles define a toolchain-setup target that calls the script.
# The root all target and os all target are both legitimate setup callers.
setup_target_makefiles=0
for makefile in Makefile os/Makefile; do
    if awk '
        /^[^[:space:]#][^:]*:/ {
            in_setup_target = $0 ~ /^toolchain-setup:[[:space:]]*$/
            next
        }
        in_setup_target && /^\t/ \
            && $0 ~ /^[[:space:]]*@?sh[[:space:]]+(scripts|\.\.\/scripts)\/rustup-setup\.sh[[:space:]]*$/ {
            printf "found"
        }
    ' "$repo_root/$makefile" | grep -q "found"; then
        setup_target_makefiles=$((setup_target_makefiles + 1))
    fi
done
# Every rustup-setup.sh call must be inside a toolchain-setup target
if [ "$setup_calls" -eq "$setup_target_makefiles" ] && [ "$setup_target_makefiles" -ge 1 ]; then
    pass 'all rustup-setup.sh calls are inside toolchain-setup targets'
else
    fail "rustup-setup.sh found $setup_calls times but only $setup_target_makefiles toolchain-setup targets call it"
fi

require_match Makefile \
    '^[[:space:]]*RUSTUP_HOME[[:space:]]*\?=[[:space:]]*\$\(HOME\)/\.rustup[[:space:]]*$' \
    'root Makefile defaults RUSTUP_HOME from HOME without replacing overrides'
require_match Makefile \
    '^[[:space:]]*CARGO_HOME[[:space:]]*\?=[[:space:]]*\$\(HOME\)/\.cargo[[:space:]]*$' \
    'root Makefile defaults CARGO_HOME from HOME without replacing overrides'
require_match Makefile \
    '^[[:space:]]*export[[:space:]].*RUSTUP_HOME' \
    'root Makefile exports RUSTUP_HOME'
require_match Makefile \
    '^[[:space:]]*export[[:space:]].*CARGO_HOME' \
    'root Makefile exports CARGO_HOME'

require_dependency Makefile all toolchain-setup
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
if awk '
    /^all:[[:space:]]+prepare-cargo-config[[:space:]]*$/ { all = 1; next }
    all && /^[[:space:]]*\$\(MAKE\)[[:space:]]+rv64_all[[:space:]]*$/ { rv = 1; next }
    rv && /^[[:space:]]*\$\(MAKE\)[[:space:]]+la64_all[[:space:]]*$/ { la = 1; next }
    la && /^[[:space:]]*\$\(MAKE\)[[:space:]]+publish-compatibility[[:space:]]*$/ { publish = 1 }
    END { exit(all && rv && la && publish ? 0 : 1) }
' "$repo_root/os/Makefile"; then
    pass 'os all serializes prepare-cargo-config, rv64_all, la64_all, then publication'
else
    fail 'os all must serialize prepare-cargo-config, rv64_all, la64_all, then publication'
fi
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

has_unapproved_setup_dependency=0
for makefile in $makefiles; do
    if ! awk -v root_makefile="$repo_root/Makefile" '
    /^[^[:space:]#][^:]*:/ && $0 !~ /^[[:space:]]*(\.PHONY|toolchain-setup)[[:space:]]*:/ \
        && index($0, "toolchain-setup") {
        split($0, parts, ":")
        count = split(parts[1], targets, /[[:space:]]+/)
        for (item = 1; item <= count; item++) {
            if (targets[item] != "all" || FILENAME != root_makefile) {
                violates = 1
            }
        }
    }
    END { exit(violates ? 1 : 0) }
' "$repo_root/$makefile"; then
        has_unapproved_setup_dependency=1
    fi
done
if [ "$has_unapproved_setup_dependency" -eq 0 ]; then
    pass 'only root all may depend on toolchain-setup'
else
    fail 'toolchain-setup must not be a dependency outside root all'
fi

fixture_root=$work_dir/fixture
fixture_rustup_home=$work_dir/rustup-home
fixture_home=$work_dir/home
fixture_path=$work_dir/bin:/usr/bin:/bin
mkdir -p "$fixture_root/os/make" "$fixture_root/user/src" "$fixture_root/scripts" \
    "$work_dir/bin" "$fixture_rustup_home" "$fixture_home"
for makefile in $makefiles; do
    if [ -r "$repo_root/$makefile" ]; then
        mkdir -p "$fixture_root/$(dirname "$makefile")"
        cp "$repo_root/$makefile" "$fixture_root/$makefile"
    else
        fail "first-party Make/module disappeared before fixture copy: $makefile"
    fi
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
: >"$fixture_root/user/src/lang_items.rs"

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
expected_rustup_home=${FAKE_EXPECTED_RUSTUP_HOME:-${FAKE_RUSTUP_HOME:?}}
if [ "${RUSTUP_HOME-}" != "$expected_rustup_home" ]; then
    echo "fake preflight: RUSTUP_HOME did not match the expected home" >&2
    exit 92
fi
if [ "${FAKE_PREFLIGHT_REQUIRES_SETUP:-0}" = 1 ] \
    && [ ! -s "${FAKE_SETUP_LOG:?}" ]; then
    echo "fake preflight: root all reached preflight without setup" >&2
    exit 93
fi
printf '%s\n' "${FAKE_PREFLIGHT_LABEL:?}" >>"${FAKE_PREFLIGHT_LOG:?}"
EOF
chmod +x "$fixture_root/scripts/rustup-preflight.sh"

cat >"$fixture_root/scripts/rustup-setup.sh" <<'EOF'
#!/bin/sh
set -eu

if [ -z "${RUSTUP_HOME-}" ] || [ -z "${CARGO_HOME-}" ]; then
    echo "fake setup: root all did not export nonempty Rustup and Cargo homes" >&2
    exit 94
fi
printf 'setup\t%s\t%s\n' "$RUSTUP_HOME" "$CARGO_HOME" >>"${FAKE_SETUP_LOG:?}"
printf '%s\n' setup >>"${FAKE_ORDER_LOG:?}"
if [ "${FAKE_SETUP_FAIL:-0}" -ne 0 ]; then
    exit "$FAKE_SETUP_FAIL"
fi
EOF
chmod +x "$fixture_root/scripts/rustup-setup.sh"

cat >"$work_dir/bin/make" <<'EOF'
#!/bin/sh
set -eu

case " $* " in
    *' prepare-cargo-config ')
        printf '%s\n' prepare-cargo-config >>"${FAKE_SUBMAKE_LOG:?}"
        printf '%s\n' prepare-cargo-config >>"${FAKE_ORDER_LOG:?}"
        ;;
    *' -C os all ')
        printf '%s\n' os-all >>"${FAKE_SUBMAKE_LOG:?}"
        printf '%s\n' os-all >>"${FAKE_ORDER_LOG:?}"
        ;;
    *)
        echo "fake make: unexpected recursive invocation: $*" >&2
        exit 95
        ;;
esac
EOF
chmod +x "$work_dir/bin/make"

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
: >"$work_dir/setup.log"
: >"$work_dir/submake.log"
: >"$work_dir/order.log"
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

run_env root "$fixture_root" /usr/bin/make env
run_env os "$fixture_root/os" /usr/bin/make env
run_env rv64 "$fixture_root/os" /usr/bin/make -f make/toolchain-contract.mk -f make/rv64.mk env
run_env rv64-probe "$fixture_root/os" /usr/bin/make -f make/toolchain-contract.mk -f make/rv64.mk toolchain-contract-rustc-probe
run_env la64 "$fixture_root/os" /usr/bin/make -f make/la64.mk env
run_env user "$fixture_root/user" /usr/bin/make env

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

if grep -Fqx os "$work_dir/preflight.log" && [ ! -s "$work_dir/setup.log" ]; then
    pass 'direct make -C os env remains preflight-only and never provisions'
else
    fail 'direct make -C os env must not invoke toolchain-setup'
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

run_root_all() {
    label=$1
    home_mode=$2
    rustup_home_mode=$3
    cargo_home_mode=$4
    setup_fail=$5
    parallel_mode=$6
    : >"$work_dir/setup.log"
    : >"$work_dir/submake.log"
    : >"$work_dir/order.log"
    set +e
    (
        cd "$fixture_root"
        unset HOME RUSTUP_HOME CARGO_HOME
        case "$home_mode" in
            present)
                HOME=$fixture_home
                export HOME
                ;;
            missing) ;;
            empty)
                HOME=
                export HOME
                ;;
            *)
                echo "invalid HOME fixture mode: $home_mode" >&2
                exit 102
                ;;
        esac
        case "$rustup_home_mode" in
            default)
                expected_rustup_home="$fixture_home/.rustup"
                ;;
            override)
                expected_rustup_home="$work_dir/$label-rustup"
                RUSTUP_HOME=$expected_rustup_home
                export RUSTUP_HOME
                ;;
            *)
                echo "invalid RUSTUP_HOME fixture mode: $rustup_home_mode" >&2
                exit 103
                ;;
        esac
        case "$cargo_home_mode" in
            default)
                expected_cargo_home="$fixture_home/.cargo"
                ;;
            override)
                expected_cargo_home="$work_dir/$label-cargo"
                CARGO_HOME=$expected_cargo_home
                export CARGO_HOME
                ;;
            *)
                echo "invalid CARGO_HOME fixture mode: $cargo_home_mode" >&2
                exit 104
                ;;
        esac
        PATH=$fixture_path
        FAKE_SETUP_LOG="$work_dir/setup.log"
        FAKE_ORDER_LOG="$work_dir/order.log"
        FAKE_SUBMAKE_LOG="$work_dir/submake.log"
        FAKE_PREFLIGHT_LABEL=$label
        FAKE_PREFLIGHT_LOG="$work_dir/preflight.log"
        FAKE_PREFLIGHT_REQUIRES_SETUP=1
        FAKE_EXPECTED_RUSTUP_HOME=$expected_rustup_home
        FAKE_SETUP_FAIL=$setup_fail
        MAKE=make
        export PATH FAKE_SETUP_LOG FAKE_ORDER_LOG FAKE_SUBMAKE_LOG \
            FAKE_PREFLIGHT_LABEL FAKE_PREFLIGHT_LOG FAKE_PREFLIGHT_REQUIRES_SETUP \
            FAKE_EXPECTED_RUSTUP_HOME FAKE_SETUP_FAIL MAKE
        case "$parallel_mode" in
            serial) /usr/bin/make all ;;
            parallel) /usr/bin/make -j 8 all ;;
            *)
                echo "invalid make fixture mode: $parallel_mode" >&2
                exit 105
                ;;
        esac
    ) >"$work_dir/$label.out" 2>&1
    status=$?
    set -e
}

expected_root_submakes='prepare-cargo-config
os-all'
expected_root_order='setup
prepare-cargo-config
os-all'
expected_default_setup=$(printf 'setup\t%s\t%s' "$fixture_home/.rustup" "$fixture_home/.cargo")
expected_override_setup=$(printf 'setup\t%s\t%s' \
    "$work_dir/root-all-overrides-rustup" "$work_dir/root-all-overrides-cargo")

run_root_all root-all-defaults present default default 0 serial
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$expected_default_setup" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ]; then
    pass 'root all derives and exports homes, then serializes setup before downstream makes'
else
    fail 'root all must provision with HOME-derived homes before prepare-cargo-config and os all'
    cat "$work_dir/root-all-defaults.out" >&2
fi

run_root_all root-all-overrides present override override 0 serial
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$expected_override_setup" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ]; then
    pass 'root all preserves explicit Rustup and Cargo home overrides'
else
    fail 'root all must propagate explicit Rustup and Cargo home overrides to setup'
    cat "$work_dir/root-all-overrides.out" >&2
fi

run_root_all root-all-missing-home-default missing default default 0 serial
if [ "$status" -ne 0 ] && [ ! -s "$work_dir/setup.log" ] && [ ! -s "$work_dir/submake.log" ] \
    && grep -Fq 'HOME must be set and non-empty when RUSTUP_HOME is not supplied' \
        "$work_dir/root-all-missing-home-default.out"; then
    pass 'missing HOME rejects default Rustup and Cargo homes before provisioning'
else
    fail 'missing HOME with default homes must fail before toolchain setup'
    cat "$work_dir/root-all-missing-home-default.out" >&2
fi

run_root_all root-all-empty-home-default empty default default 0 serial
if [ "$status" -ne 0 ] && [ ! -s "$work_dir/setup.log" ] && [ ! -s "$work_dir/submake.log" ] \
    && grep -Fq 'HOME must be set and non-empty when RUSTUP_HOME is not supplied' \
        "$work_dir/root-all-empty-home-default.out"; then
    pass 'empty HOME rejects default Rustup and Cargo homes before provisioning'
else
    fail 'empty HOME with default homes must fail before toolchain setup'
    cat "$work_dir/root-all-empty-home-default.out" >&2
fi

run_root_all root-all-missing-home-rustup-only missing override default 0 serial
if [ "$status" -ne 0 ] && [ ! -s "$work_dir/setup.log" ] && [ ! -s "$work_dir/submake.log" ] \
    && grep -Fq 'HOME must be set and non-empty when CARGO_HOME is not supplied' \
        "$work_dir/root-all-missing-home-rustup-only.out"; then
    pass 'missing HOME rejects a partial Rustup-only override before provisioning'
else
    fail 'missing HOME with only RUSTUP_HOME overridden must fail before toolchain setup'
    cat "$work_dir/root-all-missing-home-rustup-only.out" >&2
fi

run_root_all root-all-empty-home-cargo-only empty default override 0 serial
if [ "$status" -ne 0 ] && [ ! -s "$work_dir/setup.log" ] && [ ! -s "$work_dir/submake.log" ] \
    && grep -Fq 'HOME must be set and non-empty when RUSTUP_HOME is not supplied' \
        "$work_dir/root-all-empty-home-cargo-only.out"; then
    pass 'empty HOME rejects a partial Cargo-only override before provisioning'
else
    fail 'empty HOME with only CARGO_HOME overridden must fail before toolchain setup'
    cat "$work_dir/root-all-empty-home-cargo-only.out" >&2
fi

run_root_all root-all-missing-home-overrides missing override override 0 serial
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$(printf 'setup\t%s\t%s' \
        "$work_dir/root-all-missing-home-overrides-rustup" \
        "$work_dir/root-all-missing-home-overrides-cargo")" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ]; then
    pass 'missing HOME accepts complete explicit Rustup and Cargo home overrides'
else
    fail 'missing HOME with complete overrides must provision using those overrides'
    cat "$work_dir/root-all-missing-home-overrides.out" >&2
fi

run_root_all root-all-empty-home-overrides empty override override 0 serial
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$(printf 'setup\t%s\t%s' \
        "$work_dir/root-all-empty-home-overrides-rustup" \
        "$work_dir/root-all-empty-home-overrides-cargo")" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ]; then
    pass 'empty HOME accepts complete explicit Rustup and Cargo home overrides'
else
    fail 'empty HOME with complete overrides must provision using those overrides'
    cat "$work_dir/root-all-empty-home-overrides.out" >&2
fi

run_root_all root-all-parallel present default default 0 parallel
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$expected_default_setup" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ] \
    && [ "$(cat "$work_dir/order.log")" = "$expected_root_order" ]; then
    pass 'make -j all serializes setup before prepare-cargo-config and os all'
else
    fail 'make -j all must serialize setup before downstream makes'
    cat "$work_dir/root-all-parallel.out" >&2
fi

run_root_all root-all-repeat-one present default default 0 serial
repeat_one_status=$status
repeat_one_setup=$(cat "$work_dir/setup.log")
repeat_one_submakes=$(cat "$work_dir/submake.log")
run_root_all root-all-repeat-two present default default 0 serial
if [ "$repeat_one_status" -eq 0 ] && [ "$status" -eq 0 ] \
    && [ "$repeat_one_setup" = "$expected_default_setup" ] \
    && [ "$(cat "$work_dir/setup.log")" = "$expected_default_setup" ] \
    && [ "$repeat_one_submakes" = "$expected_root_submakes" ] \
    && [ "$(cat "$work_dir/submake.log")" = "$expected_root_submakes" ]; then
    pass 'repeated root all invocations retain the automatic setup contract'
else
    fail 'repeated root all invocations must each run setup before downstream makes'
    cat "$work_dir/root-all-repeat-one.out" >&2
    cat "$work_dir/root-all-repeat-two.out" >&2
fi

run_root_all root-all-setup-failure present default default 73 serial
if [ "$status" -ne 0 ] \
    && [ "$(cat "$work_dir/setup.log")" = "$expected_default_setup" ] \
    && [ ! -s "$work_dir/submake.log" ]; then
    pass 'root all short-circuits after toolchain-setup failure'
else
    fail 'root all must stop before prepare-cargo-config and os all when setup fails'
    cat "$work_dir/root-all-setup-failure.out" >&2
fi

exit "$overall"
