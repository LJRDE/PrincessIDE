#!/usr/bin/env bash
# PrincessIDE P0 — development environment activation.
#
# Usage:   source /root/PrincessIDE/scripts/env.sh
#          . scripts/env.sh
#
# The script locates the workspace root from its own path, so it works from any
# cwd and does not depend on the caller's PATH.  It is idempotent: sourcing it
# twice does not stack duplicate PATH / LD_LIBRARY_PATH entries.
#
# Everything it exports lives under the workspace (self-contained, no system
# packages are installed and no file outside the workspace is touched).

# ---------------------------------------------------------------- root lookup
_princesside_self="${BASH_SOURCE[0]:-$0}"
while [ -L "$_princesside_self" ]; do
  _princesside_link="$(readlink "$_princesside_self")"
  case "$_princesside_link" in
    /*) _princesside_self="$_princesside_link" ;;
    *)  _princesside_self="$(dirname "$_princesside_self")/$_princesside_link" ;;
  esac
done
PRINCESSIDE_SCRIPTS_DIR="$(cd "$(dirname "$_princesside_self")" && pwd)"
PRINCESSIDE_ROOT="$(cd "$PRINCESSIDE_SCRIPTS_DIR/.." && pwd)"
unset _princesside_self _princesside_link

# ------------------------------------------------------------------- layouts
PRINCESSIDE_TOOLCHAIN="$PRINCESSIDE_ROOT/.toolchain"
PRINCESSIDE_PREFIX="$PRINCESSIDE_TOOLCHAIN/prefix"   # extracted .deb tree
PRINCESSIDE_DEBCACHE="$PRINCESSIDE_TOOLCHAIN/debs"   # cached .deb archives
PRINCESSIDE_FIXTURES="$PRINCESSIDE_ROOT/fixtures"
PRINCESSIDE_REFKERNEL="$PRINCESSIDE_FIXTURES/refkernel"

# Rust: both homes live inside the workspace so nothing leaks into /root/.cargo
# or /root/.rustup (which the file sandbox would deny anyway).
export RUSTUP_HOME="$PRINCESSIDE_TOOLCHAIN/rustup"
export CARGO_HOME="$PRINCESSIDE_TOOLCHAIN/cargo"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"

# QEMU looks for BIOS / option ROM data files (bios-256k.bin, multiboot.bin,
# linuxboot_dma.bin, ...) under the configured data dir.  They are extracted
# under the workspace prefix, so scripts pass "-L $PRINCESSIDE_QEMU_DATA".
PRINCESSIDE_QEMU_DATA="$PRINCESSIDE_PREFIX/usr/share/qemu"
export PRINCESSIDE_QEMU_DATA

# ---------------------------------------------------------------------- PATH
# LLVM-on-Debian keeps its real binaries in /usr/lib/llvm-14/bin; putting that
# directory first also makes clang's relative resource-dir lookup
# (../lib/clang/14.0.0) resolve inside our prefix.
_princesside_path_prepend="$PRINCESSIDE_TOOLCHAIN/cargo/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/usr/lib/llvm-14/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/usr/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/usr/sbin"

# Remove any of our own entries from a ':'-separated list, so that sourcing this
# file repeatedly does not stack duplicate PATH / LD_LIBRARY_PATH entries.
_princesside_strip_own() {
    _princesside_out=""
    _princesside_saved_ifs="$IFS"
    IFS=':'
    for _princesside_part in $1; do
        case "$_princesside_part" in
            "$PRINCESSIDE_TOOLCHAIN"/*|"$PRINCESSIDE_PREFIX"/*) continue ;;
        esac
        [ -n "$_princesside_part" ] && \
            _princesside_out="${_princesside_out:+$_princesside_out:}$_princesside_part"
    done
    IFS="$_princesside_saved_ifs"
    printf '%s' "$_princesside_out"
}

PATH="$_princesside_path_prepend:$(_princesside_strip_own "$PATH")"
unset _princesside_path_prepend
export PATH

# ------------------------------------------------------------ LD_LIBRARY_PATH
# Only the workspace prefix is added; /etc/ld.so.conf is deliberately untouched.
_princesside_ld=""
for _d in \
  "$PRINCESSIDE_PREFIX/usr/lib/x86_64-linux-gnu" \
  "$PRINCESSIDE_PREFIX/lib/x86_64-linux-gnu" \
  "$PRINCESSIDE_PREFIX/usr/lib" \
  "$PRINCESSIDE_PREFIX/lib" \
  "$PRINCESSIDE_PREFIX/usr/lib/llvm-14/lib" ; do
  [ -d "$_d" ] && _princesside_ld="$_princesside_ld:$_d"
done
LD_LIBRARY_PATH="${_princesside_ld#:}:$(_princesside_strip_own "${LD_LIBRARY_PATH:-}")"
unset _princesside_ld _d
export LD_LIBRARY_PATH

unset -f _princesside_strip_own 2>/dev/null
unset _princesside_out _princesside_saved_ifs _princesside_part 2>/dev/null

# Keep the two systems from being confused by a stale system cargo/rustc.
export PRINCESSIDE_ENV_SOURCED=1
