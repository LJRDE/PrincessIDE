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
#
# Tools it makes available (see docs/reports/a1-clangd16.md, docs/reports/a9-gdb16.md):
#   cargo rustc rustfmt clippy        Rust toolchain
#   qemu-system-x86_64 nasm gdb       P0 fixture build / boot / debug
#   clang clangd ld.lld               LLVM 14  (P0 smoke level, unchanged)
#   clang-16 clangd-16                LLVM 16  (IDE language service, D6)
#   bear                              compile_commands.json producer (D8)
#   princess-gdb                      gdb 16.3 with built-in DAP (IDE debug
#                                     backend, D11; runs in an isolated rootfs)

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
PRINCESSIDE_BIN="$PRINCESSIDE_TOOLCHAIN/bin"         # workspace-local launchers
# A9: gdb 16.3 lives in its own rootfs (it needs a newer glibc than the host's)
# and is reached ONLY through $PRINCESSIDE_BIN/princess-gdb.  Note that this
# rootfs is deliberately absent from LD_LIBRARY_PATH below -- see the export
# block at the bottom of this file.
PRINCESSIDE_GDB16_ROOT="$PRINCESSIDE_TOOLCHAIN/gdb16/rootfs"
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
# Order matters, and it is deliberate.
#
#   .toolchain/bin            launchers first (bear needs its unpacked paths
#                             filled in; the real binary stays at
#                             $PRINCESSIDE_PREFIX/usr/bin/bear)
#   cargo/bin                 rust toolchain
#   usr/lib/llvm-14/bin       *unversioned* clang / clangd stay on 14 here, so
#   usr/lib/llvm-16/bin       P0 behaviour is unchanged; the LLVM 16 tree comes
#                             after it and is reached through the versioned
#                             names below.  Never swap these two: the two clangd
#                             majors must not be confused (see D6).
#   usr/bin                   versioned symlinks: clang-14/clang-16,
#                             clangd-14/clangd-16, ld.lld*, gdb, qemu, nasm, ...
#                             (Debian ships /usr/bin/clangd-16 ->
#                             ../lib/llvm-16/bin/clangd, and dpkg-deb -x keeps
#                             the symlink, so the versioned names just work)
#   usr/sbin                  qemu bridge helper etc.
#
# The IDE language service must call `clangd-16` (D6); it is exported below as
# $PRINCESSIDE_CLANGD16 / $PRINCESSIDE_LANG_SERVICE_CLANGD so a client never has
# to guess which major it is talking to.
_princesside_path_prepend="$PRINCESSIDE_BIN"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_TOOLCHAIN/cargo/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/usr/lib/llvm-14/bin"
_princesside_path_prepend="$_princesside_path_prepend:$PRINCESSIDE_PREFIX/usr/lib/llvm-16/bin"
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

# (Same empty-element guard as LD_LIBRARY_PATH below: a trailing ':' would put
# the current directory on PATH.)
_princesside_path_prior="$(_princesside_strip_own "${PATH:-}")"
if [ -n "${_princesside_path_prior}" ]; then
    PATH="$_princesside_path_prepend:$_princesside_path_prior"
else
    PATH="$_princesside_path_prepend"
fi
unset _princesside_path_prepend _princesside_path_prior
export PATH

# ------------------------------------------------------------ LD_LIBRARY_PATH
# Only the workspace prefix is added; /etc/ld.so.conf is deliberately untouched.
#
# *** NEVER add $PRINCESSIDE_GDB16_ROOT (or anything under .toolchain/gdb16/) to
# *** this list.  That rootfs carries Debian trixie's glibc 2.41, while the host
# *** and every other tool here run bookworm's glibc 2.36.  Putting a second
# *** libc.so.6 on the host loader path breaks EVERY process on the machine:
# ***     head: symbol lookup error: .../libc.so.6:
# ***           undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE
# *** gdb 16.3 is reached through $PRINCESSIDE_BIN/princess-gdb instead, which
# *** runs it under trixie's own loader with its own --library-path.  See A9.
_princesside_ld=""
for _d in \
  "$PRINCESSIDE_PREFIX/usr/lib/x86_64-linux-gnu" \
  "$PRINCESSIDE_PREFIX/lib/x86_64-linux-gnu" \
  "$PRINCESSIDE_PREFIX/usr/lib" \
  "$PRINCESSIDE_PREFIX/lib" \
  "$PRINCESSIDE_PREFIX/usr/lib/llvm-16/lib" \
  "$PRINCESSIDE_PREFIX/usr/lib/llvm-14/lib" ; do
  [ -d "$_d" ] && _princesside_ld="$_princesside_ld:$_d"
done
# Note the explicit emptiness handling: a bare ":<prior>" would leave an *empty*
# LD_LIBRARY_PATH element, which the loader reads as "the current directory".
# That is a real hazard for a language service (a stray libclang-cpp.so.16 in a
# project directory would get picked up), so never emit it.
_princesside_ld_prior="$(_princesside_strip_own "${LD_LIBRARY_PATH:-}")"
if [ -n "${_princesside_ld_prior}" ]; then
  LD_LIBRARY_PATH="${_princesside_ld#:}:$_princesside_ld_prior"
else
  LD_LIBRARY_PATH="${_princesside_ld#:}"
fi
unset _princesside_ld _princesside_ld_prior _d
export LD_LIBRARY_PATH

# ------------------------------------------------------- language service (A1)
# clangd on Debian has RUNPATH=$ORIGIN/../lib, but DT_RUNPATH does not apply to
# indirect dependencies, so a clangd unpacked into our prefix cannot find its
# grpc/protobuf/LLVM libraries on its own.  LD_LIBRARY_PATH above is what makes
# it run; these variables are the stable handles the IDE should use.
PRINCESSIDE_CLANGD16="$PRINCESSIDE_PREFIX/usr/bin/clangd-16"
PRINCESSIDE_CLANGD14="$PRINCESSIDE_PREFIX/usr/bin/clangd-14"
PRINCESSIDE_CLANG16="$PRINCESSIDE_PREFIX/usr/bin/clang-16"
PRINCESSIDE_BEAR="$PRINCESSIDE_BIN/bear"
# The one the IDE's C/C++ language service must spawn (D6).
PRINCESSIDE_LANG_SERVICE_CLANGD="$PRINCESSIDE_CLANGD16"
export PRINCESSIDE_CLANGD16 PRINCESSIDE_CLANGD14 PRINCESSIDE_CLANG16 \
       PRINCESSIDE_BEAR PRINCESSIDE_LANG_SERVICE_CLANGD

# ------------------------------------------------------- Java language service (D31/P-F1)
# jdtls (Eclipse JDT Language Server) for Java language support.
# D31: Language modules are pluggable; jdtls is installed into .toolchain/jdtls/.
PRINCESSIDE_JDTLS="$PRINCESSIDE_BIN/jdtls"
PRINCESSIDE_LANG_SERVICE_JDTLS="$PRINCESSIDE_JDTLS"
export PRINCESSIDE_JDTLS PRINCESSIDE_LANG_SERVICE_JDTLS

# ------------------------------------------------------------ debug backend (A9)
# The IDE's debug backend must use a gdb >= 14, because the built-in DAP
# interpreter P4 relies on (D11) only exists from gdb 14 onwards.  This host's
# bookworm gdb is 13.1, so the DAP-capable one is the trixie 16.3 unpacked under
# .toolchain/gdb16/rootfs/:
#
#   PRINCESSIDE_GDB                    13.1  -- unchanged, still first on PATH
#   PRINCESSIDE_DEBUG_GDB              princess-gdb (16.3, DAP-capable)  <-- use this
#   PRINCESSIDE_GDB16                  absolute path to the 16.3 binary
#
# `gdb` on PATH is deliberately NOT repointed at 16.3.  Just like clangd, the
# versioned/labelled handle takes the new tool and the bare name keeps behaving
# exactly as it did before A9, so nothing that already worked can regress.  The
# two live in different glibc worlds, so a caller must not mix them up.
PRINCESSIDE_GDB="$PRINCESSIDE_PREFIX/usr/bin/gdb"
PRINCESSIDE_GDB16="$PRINCESSIDE_GDB16_ROOT/usr/bin/gdb"
PRINCESSIDE_GDB16_LAUNCHER="$PRINCESSIDE_BIN/princess-gdb"
# The one the IDE's debug adapter must spawn (D11).  It is a self-contained
# launcher: it brings up trixie's own loader and library path internally, so the
# caller does NOT set LD_LIBRARY_PATH and does NOT need to know the rootfs
# layout.  Do not point this at PRINCESSIDE_GDB16 directly.
PRINCESSIDE_DEBUG_GDB="$PRINCESSIDE_GDB16_LAUNCHER"
export PRINCESSIDE_GDB PRINCESSIDE_GDB16 PRINCESSIDE_GDB16_ROOT \
       PRINCESSIDE_GDB16_LAUNCHER PRINCESSIDE_DEBUG_GDB

unset -f _princesside_strip_own 2>/dev/null
unset _princesside_out _princesside_saved_ifs _princesside_part 2>/dev/null

# Keep the two systems from being confused by a stale system cargo/rustc.
export PRINCESSIDE_ENV_SOURCED=1
