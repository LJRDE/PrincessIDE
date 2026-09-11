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

# expect_version <tool-name> <executable> <extended-regexp>
#
# "It resolved" is not enough for the two clangd majors: a `clangd-16` that
# answers 14.0.6 would mean PATH or the unpacked prefix got mixed up -- exactly
# the failure D6 exists to prevent -- and a doctor that stayed silent about it
# would be worse than no doctor.  Only reported when the executable exists (a
# missing one is already reported, loudly, by report()).
expect_version() {
    local name="$1" exe="$2" want="$3" got
    command -v "$exe" >/dev/null 2>&1 || return 0
    got="$("$exe" --version 2>&1 | head -n1 | tr -s ' ' | sed 's/^ *//; s/ *$//')"
    if printf '%s' "$got" | grep -Eq "$want"; then
        printf '%-22s %-10s %s\n' "$name" "ok" "version matches /$want/"
    else
        printf '%-22s %-10s %s (wanted /%s/)\n' "$name" "WRONG VER" "$got" "$want"
        missing+=("$name (wrong version)")
    fi
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

# --- IDE language service (A1) -----------------------------------------------
# Both clangd majors must resolve *separately*: `clangd`/`clang` above stay on
# 14 for P0 compatibility, while the language service uses clangd-16 (D6).
report 'clangd-16 (LSP)'  clangd-16          'clangd-16 --version'                    1
report clang-16           clang-16           'clang-16 --version'                     1
report 'clangd-14 (legacy)' clangd-14        'clangd-14 --version'                    1
report 'bear (CDB)'       bear               'bear --version'                         1

# Major versions are part of the contract, not cosmetics (D6).
expect_version 'clangd-16 (LSP)' clangd-16 'clangd version 16\.'
expect_version 'clangd-14 (legacy)' clangd-14 'clangd version 14\.'
expect_version 'clangd (P0 default)' clangd 'clangd version 14\.'
expect_version 'clang-16'        clang-16  'clang version 16\.'
expect_version 'bear (CDB)'      bear      'bear 3\.'

# --- IDE debug backend (A9) --------------------------------------------------
# The P0 `gdb` above stays on bookworm's 13.1 (no DAP).  The DAP-capable gdb is
# a *separate* trixie 16.3 living in its own rootfs and reached through the
# princess-gdb launcher; it must never be confused with the 13.1 on PATH.
report 'gdb-16.3 (DAP)'   princess-gdb       'princess-gdb --version'                 1
report 'gdb (P0 legacy)'  gdb                'gdb --version'                          1

# D11: DAP needs gdb >= 14, so the version is part of the contract.
expect_version 'gdb-16.3 (DAP)' princess-gdb 'GNU gdb .* 1[4-9]\.|GNU gdb .* [2-9][0-9]\.'
# The legacy one must still be the old, non-DAP build -- if this ever answers
# >= 14 something has silently swapped the two and P0 behaviour changed.
expect_version 'gdb (P0 legacy)' gdb 'GNU gdb .* 13\.'

# The DAP interpreter itself.  A gdb can report 16 and still have been built
# without it, so probe the capability rather than trusting the number.
gdb_dap_check() {
    local exe="${PRINCESSIDE_DEBUG_GDB:-princess-gdb}" out
    command -v "$exe" >/dev/null 2>&1 || return 0
    out="$(printf 'Content-Length: 2\r\n\r\n{}' \
             | timeout 30 "$exe" -q -i=dap 2>&1 || true)"
    if printf '%s' "$out" | grep -qi 'unrecognized'; then
        printf '%-22s %-10s %s\n' 'gdb DAP (-i=dap)' "NO DAP" "interpreter not built in"
        missing+=("gdb DAP (-i=dap)")
    else
        printf '%-22s %-10s %s\n' 'gdb DAP (-i=dap)' "ok" "built-in DAP interpreter present"
    fi
}
gdb_dap_check

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
