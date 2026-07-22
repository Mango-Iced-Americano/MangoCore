#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

overall=0

fail() {
    echo "FAIL: $*" >&2
    overall=1
}

pass() {
    echo "PASS: $*"
}

makefiles=$(find "$repo_root" \
    \( -path "$repo_root/.git" -o -path "$repo_root/dependency" -o -path "$repo_root/user/tools" \) -prune -o \
    -type f \( -name Makefile -o -name GNUmakefile -o -name '*.mk' \) -print \
    | LC_ALL=C sort)

if [ -z "$makefiles" ]; then
    fail 'no first-party Makefiles discovered'
fi

check_prepare_cargo_config_purity() {
    source_path=$1
    makefile_name=${source_path#"$repo_root"/}

    restore_violations=$(awk '
        /^prepare-cargo-config[[:space:]]*:/ {
            in_recipe = 1
            next
        }
        in_recipe && /^\t/ {
            line = $0
            sub(/^[[:space:]]*/, "", line)
            if (line ~ /^#/) {
                next
            }
            sub(/^[-@]+[[:space:]]*/, "", line)
            if (line ~ /^sh[[:space:]]+[^[:space:]]*restore-cargo-vendor-checksums\.sh[[:space:]]+restore([[:space:]]|$)/) {
                print FILENAME ":" NR ":" $0
            }
            next
        }
        in_recipe {
            in_recipe = 0
        }
    ' "$source_path")

    if [ -n "$restore_violations" ]; then
        fail "$makefile_name prepare-cargo-config recipe invokes restore-cargo-vendor-checksums.sh restore"
        printf '%s\n' "$restore_violations" >&2
    else
        pass "$makefile_name prepare-cargo-config recipe has no restore invocation"
    fi

    command_violations=$(awk '
        /^prepare-cargo-config[[:space:]]*:/ {
            in_recipe = 1
            next
        }
        in_recipe && /^\t/ {
            line = $0
            sub(/^[[:space:]]*/, "", line)
            if (line ~ /^#/) {
                next
            }
            sub(/^[-@]+[[:space:]]*/, "", line)
            if (line != "") {
                print FILENAME ":" NR ":" $0
            }
            next
        }
        in_recipe {
            in_recipe = 0
        }
    ' "$source_path")

    if [ -n "$command_violations" ]; then
        fail "$makefile_name prepare-cargo-config recipe contains executable command"
        printf '%s\n' "$command_violations" >&2
    else
        pass "$makefile_name prepare-cargo-config recipe has no executable command"
    fi
}

check_lang_items_unified() {
    source=$1
    label=$2

    if grep -E '^mod lang_items;' "$source" >/dev/null 2>&1 && \
       ! grep -F '[path = "lang_items.rs.' "$source" >/dev/null 2>&1; then
        pass "$label uses unified lang_items (no variant paths)"
    else
        fail "$label still uses variant-specific lang_items #[path]"
    fi
}

check_variant_copy_recipes() {
    source_path=$1
    makefile_name=${source_path#"$repo_root"/}
    violations=$(awk '
        function flush_recipe(    command) {
            if (recipe == "") {
                return
            }

            command = recipe
            gsub(/["\047]/, "", command)
            sub(/^[[:space:]]*[-@[:space:]]*[[:space:]]*/, "", command)
            if (command ~ /(^|[[:space:];])(cp|\/bin\/cp|\$\(CP\))([[:space:]]+-[^[:space:]]+)*[[:space:]]+[^[:space:]]*src\/lang_items\.rs\.(rv|la)[[:space:]]+[^[:space:]]*src\/lang_items\.rs([[:space:];]|$)/) {
                print FILENAME ":" recipe_start ": " recipe
            }
            recipe = ""
        }
        {
            line = $0
            is_recipe = line ~ /^\t/
            if (line ~ /^[[:alnum:]_.%\/-]+[[:space:]]*:[^=]*;/) {
                is_recipe = 1
            }
            if (continued) {
                is_recipe = 1
            }
            if (!is_recipe) {
                flush_recipe()
                continued = 0
                next
            }

            sub(/^\t/, "", line)
            if (recipe == "") {
                recipe_start = NR
                recipe = line
            } else {
                recipe = recipe " " line
            }
            if (line ~ /\\[[:space:]]*$/) {
                sub(/\\[[:space:]]*$/, "", recipe)
                continued = 1
            } else {
                continued = 0
                flush_recipe()
            }
        }
        END { flush_recipe() }
    ' "$source_path")

    if [ -n "$violations" ]; then
        fail "$makefile_name recipes copy variant lang_items into tracked active lang_items.rs"
        printf '%s\n' "$violations" >&2
    else
        pass "$makefile_name recipes do not copy variant lang_items into active files"
    fi
}

check_profile_build_mutations() {
    source_path=$1
    makefile_name=${source_path#"$repo_root"/}
    violations=$(awk '
        /^\t/ {
            line = $0
            if (line ~ /(^|[[:space:]])(cp|mv|touch|sed[[:space:]]+-i|patch|git[[:space:]]+(checkout|restore))([[:space:]]|$)/ &&
                line ~ /src\/hal\/arch\/(riscv|loongarch64)\/linker\.ld|src\/initramfs-regression-(rv|la)\.S/) {
                print FILENAME ":" NR ":" $0
            }
            if (line ~ /\$\((LWEXT4_DIR|LWEXT4_LA_DIR)\)\/(toolchain|src)\// &&
                line ~ /(^|[[:space:]])(cp|mv|touch|sed[[:space:]]+-i|patch)([[:space:]]|$)/) {
                print FILENAME ":" NR ":" $0
            }
        }
    ' "$source_path")

    if [ -n "$violations" ]; then
        fail "$makefile_name normal, ktest, or regression recipe mutates tracked linker/initramfs/vendor inputs"
        printf '%s\n' "$violations" >&2
    else
        pass "$makefile_name normal, ktest, and regression recipes write only declared outputs"
    fi
}

for required_makefile in Makefile os/Makefile user/Makefile; do
    if [ -r "$repo_root/$required_makefile" ]; then
        pass "read $required_makefile"
    else
        fail "missing Makefile $required_makefile"
    fi
done

for discovered_makefile in $makefiles; do
    check_prepare_cargo_config_purity "$discovered_makefile"
    check_variant_copy_recipes "$discovered_makefile"
done

for profile_makefile in os/make/rv64.mk os/make/la64.mk; do
    check_profile_build_mutations "$repo_root/$profile_makefile"
done

check_lang_items_unified "$repo_root/os/src/main.rs" 'os/src/main.rs'
check_lang_items_unified "$repo_root/user/src/lib.rs" 'user/src/lib.rs'

run_adversarial_fixture() {
    fixture_parent=$(mktemp -d "${TMPDIR:-/tmp}/source-purity-make-contract.XXXXXX")
    trap 'rm -rf "$fixture_parent"' EXIT HUP INT TERM
    fixture_root=$fixture_parent/repo
    mkdir -p "$fixture_root/os/src" "$fixture_root/user/src" "$fixture_root/modules"

    cat >"$fixture_root/Makefile" <<'EOF'
prepare-cargo-config:

EOF
    cat >"$fixture_root/os/Makefile" <<'EOF'
prepare-cargo-config:

EOF
    cat >"$fixture_root/user/Makefile" <<'EOF'
prepare-cargo-config:

EOF
    cat >"$fixture_root/os/src/main.rs" <<'EOF'
mod lang_items;
EOF
    cat >"$fixture_root/user/src/lib.rs" <<'EOF'
mod lang_items;
EOF
    cat >"$fixture_root/modules/variant-copy.mk" <<'EOF'
plain:
	cp src/lang_items.rs.rv src/lang_items.rs
dash-prefix:
	-@cp src/lang_items.rs.la src/lang_items.rs
at-space-prefix:
	@ cp src/lang_items.rs.rv src/lang_items.rs
make-variable:
	$(CP) "src/lang_items.rs.la" "src/lang_items.rs"
absolute-quoted:
	/bin/cp 'src/lang_items.rs.rv' 'src/lang_items.rs'
EOF

    set +e
    SOURCE_PURITY_MAKE_CONTRACT_CHILD=1 sh "$0" "$fixture_root" \
        >"$fixture_parent/fixture.out" 2>&1
    fixture_status=$?
    set -e

    if [ "$fixture_status" -eq 0 ]; then
        fail 'adversarial fixture must be rejected by the production scanner'
        cat "$fixture_parent/fixture.out" >&2
        return
    fi

    fixture_file="$fixture_root/modules/variant-copy.mk"
    fixture_lines='2 4 6 8 10'
    fixture_coverage=1
    for line in $fixture_lines; do
        if ! grep -Fq "$fixture_file:$line:" "$fixture_parent/fixture.out"; then
            fixture_coverage=0
        fi
    done
    if [ "$fixture_coverage" -eq 1 ]; then
        pass 'adversarial fixture rejects plain, -@cp, @ cp, $(CP), /bin/cp, and quoted variant copies'
        pass 'fixture modules/variant-copy.mk is outside os/Makefile and would have been missed by the former os/Makefile-only scanner'
    else
        fail 'adversarial fixture must report every variant-copy spelling with file:line diagnostics'
        cat "$fixture_parent/fixture.out" >&2
    fi
}

if [ "${SOURCE_PURITY_MAKE_CONTRACT_CHILD:-0}" != 1 ]; then
    run_adversarial_fixture
fi

exit "$overall"
