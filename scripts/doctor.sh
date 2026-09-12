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

# --- output helper (honours -q) -----------------------------------------------
out() { [ "$quiet" = 1 ] || printf '%s\n' "$*"; }
outf() { [ "$quiet" = 1 ] || printf "$@"; }

missing=()
present=0

# report <tool-name> <executable> <version-command> <required:0|1>
report() {
    local name="$1" exe="$2" vercmd="$3" required="$4"
    local path version

    path="$(command -v "$exe" 2>/dev/null || true)"
    if [ -z "$path" ]; then
        outf '%-22s %-10s %s\n' "$name" "MISSING" "(required: $exe)"
        [ "$required" = "1" ] && missing+=("$name")
        return
    fi

    version="$(eval "$vercmd" 2>&1 | head -n1 | tr -s ' ' | sed 's/^ *//; s/ *$//')"
    [ -z "$version" ] && version="(no version output)"
    outf '%-22s %-10s %s\n' "$name" "ok" "$version"
    outf '%-22s %-10s %s\n' "" "" "$path"
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
        outf '%-22s %-10s %s\n' "$name" "ok" "version matches /$want/"
    else
        outf '%-22s %-10s %s (wanted /%s/)\n' "$name" "WRONG VER" "$got" "$want"
        missing+=("$name (wrong version)")
    fi
}

# --- BUG-006 helper: extract major version number -----------------------------
# _major_version <executable> [--flag-for-version-output]
# Prints the first integer from the version output, or "" if not found.
_major_version() {
    local exe="$1" flag="${2:---version}"
    local out
    command -v "$exe" >/dev/null 2>&1 || return 1
    out="$("$exe" "$flag" 2>&1 | head -n1)"
    # Extract first standalone integer (the major)
    printf '%s' "$out" | grep -oE '[0-9]+' | head -n1
}

# --- BUG-006: relaxed check — accept versioned name OR system name >= min_major
# check_tool_relaxed <display-name> <versioned-exe> <system-exe> <min-major> <required:0|1>
#
# Priority: versioned name first; fall back to system name if >= min_major.
# Returns 0 if tool satisfied, 1 if missing.
check_tool_relaxed() {
    local name="$1" versioned="$2" sysname="$3" min_major="$4" required="$5"
    local path major

    # Priority 1: versioned name exists
    path="$(command -v "$versioned" 2>/dev/null || true)"
    if [ -n "$path" ]; then
        local ver
        ver="$("$versioned" --version 2>&1 | head -n1 | tr -s ' ' | sed 's/^ *//; s/ *$//')"
        outf '%-22s %-10s %s\n' "$name" "ok" "$ver"
        outf '%-22s %-10s %s\n' "" "" "$path"
        present=$((present + 1))
        return 0
    fi

    # Priority 2: system name with sufficient major
    path="$(command -v "$sysname" 2>/dev/null || true)"
    if [ -n "$path" ]; then
        major="$(_major_version "$sysname")"
        if [ -n "$major" ] && [ "$major" -ge "$min_major" ] 2>/dev/null; then
            local ver
            ver="$("$sysname" --version 2>&1 | head -n1 | tr -s ' ' | sed 's/^ *//; s/ *$//')"
            outf '%-22s %-10s %s\n' "$name" "ok" "$ver (system, >=$min_major)"
            outf '%-22s %-10s %s\n' "" "" "$path"
            present=$((present + 1))
            return 0
        fi
    fi

    # Neither satisfied
    outf '%-22s %-10s %s\n' "$name" "MISSING" "(need $versioned or $sysname >= $min_major)"
    [ "$required" = "1" ] && missing+=("$name")
    return 1
}

# --- BUG-005: GUI dev library check via pkg-config ----------------------------
# check_pkgconfig <display-name> <module> <apt-package>
check_pkgconfig() {
    local name="$1" module="$2" aptpkg="$3"
    local version

    # If pkg-config itself is missing, we already reported it above.
    command -v pkg-config >/dev/null 2>&1 || return 0

    version="$(pkg-config --modversion "$module" 2>/dev/null || true)"
    if [ -n "$version" ]; then
        outf '%-22s %-10s %s\n' "$name" "ok" "v$version"
        present=$((present + 1))
    else
        outf '%-22s %-10s %s\n' "$name" "MISSING" "(need apt package: $aptpkg)"
        missing+=("$name ($aptpkg)")
    fi
}

# ===========================================================================
out 'PrincessIDE toolchain doctor'
out "workspace : $ROOT"
out "prefix    : $PRINCESSIDE_PREFIX"
out "RUSTUP_HOME=$RUSTUP_HOME"
out "CARGO_HOME =$CARGO_HOME"
out "-------------------------------------------------------------------------------"
outf '%-22s %-10s %s\n' "TOOL" "STATUS" "VERSION / PATH"
out "-------------------------------------------------------------------------------"

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

# --- IDE language service (A1, BUG-006 relaxed) --------------------------------
# BUG-006: clangd-16 / clang-16 accept either the versioned name or a system
# clangd/clang with major >= 16.  The versioned name is preferred when present.
# clangd-14 (legacy) is kept as-is (always versioned, no relaxation needed).
check_tool_relaxed 'clangd-16 (LSP)'  clangd-16  clangd  16  1
check_tool_relaxed 'clang-16'         clang-16   clang   16  1
report 'clangd-14 (legacy)' clangd-14        'clangd-14 --version'                    1
report 'bear (CDB)'       bear               'bear --version'                         1

# Major versions are part of the contract, not cosmetics (D6).
# For clangd-16 and clang-16, check whichever resolved (versioned first).
_clangd16_check() {
    local exe="clangd-16"
    command -v "$exe" >/dev/null 2>&1 || exe="clangd"
    command -v "$exe" >/dev/null 2>&1 || return 0
    local major
    major="$(_major_version "$exe")"
    if [ -n "$major" ] && [ "$major" -ge 16 ] 2>/dev/null; then
        outf '%-22s %-10s %s\n' 'clangd-16 ver' "ok" "major=$major (>=16)"
    else
        outf '%-22s %-10s %s\n' 'clangd-16 ver' "TOO OLD" "major=${major:-?} (need >=16)"
        missing+=("clangd-16 (too old)")
    fi
}
_clangd16_check
_clang16_check() {
    local exe="clang-16"
    command -v "$exe" >/dev/null 2>&1 || exe="clang"
    command -v "$exe" >/dev/null 2>&1 || return 0
    local major
    major="$(_major_version "$exe")"
    if [ -n "$major" ] && [ "$major" -ge 16 ] 2>/dev/null; then
        outf '%-22s %-10s %s\n' 'clang-16 ver' "ok" "major=$major (>=16)"
    else
        outf '%-22s %-10s %s\n' 'clang-16 ver' "TOO OLD" "major=${major:-?} (need >=16)"
        missing+=("clang-16 (too old)")
    fi
}
_clang16_check

expect_version 'clangd-14 (legacy)' clangd-14 'clangd version 14\.'
expect_version 'clangd (P0 default)' clangd 'clangd version 14\.'
expect_version 'bear (CDB)'      bear      'bear 3\.'

# --- IDE debug backend (A9, BUG-006 relaxed) ----------------------------------
# BUG-006: DAP gdb accepts either `princess-gdb` or system `gdb` with major >= 14.
# The DAP capability probe is ALWAYS performed — version alone is not enough (D11).
check_tool_relaxed 'gdb DAP'          princess-gdb  gdb  14  1
report 'gdb (P0 legacy)'  gdb                'gdb --version'                          1

# D11: DAP needs gdb >= 14, so the version is part of the contract.
# Check whichever DAP gdb resolved (versioned first).
_gdb_dap_version_check() {
    local exe="princess-gdb"
    command -v "$exe" >/dev/null 2>&1 || {
        # Fall back to system gdb if major >= 14
        local major
        major="$(_major_version gdb)"
        if [ -n "$major" ] && [ "$major" -ge 14 ] 2>/dev/null; then
            exe="gdb"
        else
            return 0
        fi
    }
    expect_version 'gdb DAP ver' "$exe" 'GNU gdb .* 1[4-9]\.|GNU gdb .* [2-9][0-9]\.'
}
_gdb_dap_version_check

# The legacy one must still be the old, non-DAP build -- if this ever answers
# >= 14 something has silently swapped the two and P0 behaviour changed.
expect_version 'gdb (P0 legacy)' gdb 'GNU gdb .* 13\.'

# The DAP interpreter itself.  A gdb can report 16 and still have been built
# without it, so probe the capability rather than trusting the number.
# BUG-006: Try princess-gdb first, fall back to system gdb if major >= 14.
gdb_dap_check() {
    local exe="${PRINCESSIDE_DEBUG_GDB:-princess-gdb}" out
    # If princess-gdb not available, try system gdb with major >= 14
    if ! command -v "$exe" >/dev/null 2>&1; then
        local major
        major="$(_major_version gdb)"
        if [ -n "$major" ] && [ "$major" -ge 14 ] 2>/dev/null; then
            exe="gdb"
        else
            return 0
        fi
    fi
    out="$(printf 'Content-Length: 2\r\n\r\n{}' \
             | timeout 30 "$exe" -q -i=dap 2>&1 || true)"
    if printf '%s' "$out" | grep -qi 'unrecognized'; then
        outf '%-22s %-10s %s\n' 'gdb DAP (-i=dap)' "NO DAP" "interpreter not built in"
        missing+=("gdb DAP (-i=dap)")
    else
        outf '%-22s %-10s %s\n' 'gdb DAP (-i=dap)' "ok" "built-in DAP interpreter present"
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

# --- GUI / desktop development libraries (BUG-005) ---------------------------
out "-------------------------------------------------------------------------------"
out "# GUI / 桌面开发库（Tauri 外壳必需）"

# First check pkg-config itself
report 'pkg-config'        pkg-config         'pkg-config --version'                   1

check_pkgconfig 'webkit2gtk-4.1'          webkit2gtk-4.1          'libwebkit2gtk-4.1-dev'
check_pkgconfig 'gtk+-3.0'               gtk+-3.0                'libgtk-3-dev'
check_pkgconfig 'javascriptcoregtk-4.1'  javascriptcoregtk-4.1   'libjavascriptcoregtk-4.1-dev'
check_pkgconfig 'libsoup-3.0'            libsoup-3.0             'libsoup-3.0-dev'
check_pkgconfig 'librsvg-2.0'            librsvg-2.0             'librsvg2-dev'

out "-------------------------------------------------------------------------------"

if [ "${#missing[@]}" -ne 0 ]; then
    outf 'doctor: %d tool(s) present, MISSING REQUIRED: %s\n' "$present" "${missing[*]}" >&2
    # If GUI libs are missing, give the apt fix command
    for m in "${missing[@]}"; do
        case "$m" in
            *webkit2gtk*|*gtk+-3*|*javascriptcoregtk*|*libsoup*|*librsvg*|*pkg-config*)
                outf '\nTo fix GUI library issues, run:\n' >&2
                out '  sudo apt-get update -y && sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev librsvg2-dev libayatana-appindicator3-dev libxdo-dev libssl-dev pkg-config build-essential file wget curl' >&2
                break
                ;;
        esac
    done
    outf 'doctor: run  bash %s/bootstrap-toolchain.sh  to install them.\n' "$SCRIPT_DIR" >&2
    exit 1
fi

outf 'doctor: all required tools present (%d resolved).\n' "$present"
exit 0
