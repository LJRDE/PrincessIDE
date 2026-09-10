#!/usr/bin/env bash
# PrincessIDE P0 — toolchain doctor.
#
# Prints, for every tool P0 depends on, the version it reports and the absolute
# path it actually resolves to.  Exits non-zero if any *required* tool is
# missing, so CI can gate on it.
#
# Works in a clean shell: it locates and sources scripts/env.sh itself.
#
# Usage:  bash scripts/doctor.sh [-q]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

quiet=0
[ "${1:-}" = "-q" ] && quiet=1

missing=()
present=0

# report <tool-name> <executable> <version-command> <required:0|1>
report() {
    local name="$1" exe="$2" vercmd="$3" required="$4"
    local path version

    path="$(command -v "$exe" 2>/dev/null || true)"
    if [ -z "$path" ]; then
        printf '%-22s %-10s %s\n' "$name" "MISSING" "(required: $exe)"
        [ "$required" = "1" ] && missing+=("$name")
        return
    fi

    version="$(eval "$vercmd" 2>&1 | head -n1 | tr -s ' ' | sed 's/^ *//; s/ *$//')"
    [ -z "$version" ] && version="(no version output)"
    printf '%-22s %-10s %s\n' "$name" "ok" "$version"
    printf '%-22s %-10s %s\n' "" "" "$path"
    present=$((present + 1))
}

printf 'PrincessIDE toolchain doctor\n'
printf 'workspace : %s\n' "$ROOT"
printf 'prefix    : %s\n' "$PRINCESSIDE_PREFIX"
printf 'RUSTUP_HOME=%s\n' "$RUSTUP_HOME"
printf 'CARGO_HOME =%s\n' "$CARGO_HOME"
printf '%s\n' "-------------------------------------------------------------------------------"
printf '%-22s %-10s %s\n' "TOOL" "STATUS" "VERSION / PATH"
printf '%s\n' "-------------------------------------------------------------------------------"

# --- Rust toolchain (installed by scripts/bootstrap-toolchain.sh) -------------
report cargo              cargo              'cargo --version'                        1
report rustc              rustc              'rustc --version'                        1
report rustfmt            rustfmt            'rustfmt --version'                      1
report clippy             cargo-clippy       'cargo-clippy --version'                 1

# --- headless emulator and kernel build/test tools ---------------------------
report qemu-system-x86_64 qemu-system-x86_64 'qemu-system-x86_64 --version'            1
report nasm               nasm               'nasm -v'                                1
report clang              clang              'clang --version'                        1
report clangd             clangd             'clangd --version'                       1
report ld.lld             ld.lld             'ld.lld --version'                       1
report gdb                gdb                'gdb --version'                          1
report xorriso            xorriso            'xorriso --version'                      1
report mtools             mformat            'mformat --version'                      1
report grub-mkrescue      grub-mkrescue      'grub-mkrescue --version'                1

# --- host build tools --------------------------------------------------------
report gcc                gcc                'gcc --version'                          1
report ld                 ld                 'ld --version'                           1
report objdump            objdump            'objdump --version'                      1
report addr2line          addr2line          'addr2line --version'                    1
report readelf            readelf            'readelf --version'                      1
report make               make               'make --version'                         1
report cmake              cmake              'cmake --version'                        1
report git                git                'git --version'                          1

# --- node toolchain (Web frontend, later phases) -----------------------------
report node               node               'node --version'                         0
report npm                npm                'npm --version'                          0
report pnpm               pnpm               'pnpm --version'                         0

printf '%s\n' "-------------------------------------------------------------------------------"

if [ "${#missing[@]}" -ne 0 ]; then
    printf 'doctor: %d tool(s) present, MISSING REQUIRED: %s\n' "$present" "${missing[*]}" >&2
    printf 'doctor: run  bash %s/bootstrap-toolchain.sh  to install them.\n' "$SCRIPT_DIR" >&2
    exit 1
fi

printf 'doctor: all required tools present (%d resolved).\n' "$present"
exit 0
