#!/usr/bin/env bash
# PrincessIDE P0 — toolchain bootstrap.
#
# LC_ALL=C is forced for the whole script, and that is load-bearing rather than
# cosmetic.  Several checks parse the *human-readable* output of apt, which apt
# translates: on a zh_CN.UTF-8 host `apt-cache policy gdb` prints "候选: 16.3-1"
# instead of "Candidate: 16.3-1", so the `sed -n 's/^  Candidate: //p'` in
# gdb16_index_usable matched nothing, the index was declared unusable, and
# bootstrap reset the index, failed the check again and exited 1 -- on a machine
# where the index was perfectly fine and the fetch had just succeeded.
# Forcing the C locale makes every child command (apt, apt-cache, dpkg, sort,
# grep) emit the untranslated strings these parsers expect.
export LC_ALL=C
#
# Builds a fully self-contained development toolchain inside the workspace:
#   .toolchain/rustup        RUSTUP_HOME
#   .toolchain/cargo         CARGO_HOME  (cargo, rustc, rustfmt, clippy)
#   .toolchain/prefix/       root of the extracted .deb tree
#   .toolchain/debs/         .deb download cache
#   .toolchain/bin/          workspace-local launchers (bear, princess-gdb)
#   .toolchain/gdb16/        a SECOND, self-contained rootfs: Debian trixie's
#                            gdb 16.3 plus its entire dependency closure
#                            (including trixie's own glibc).  It is never put on
#                            the shared LD_LIBRARY_PATH; see 2c.
#
# Three tool sets live here:
#   * P0 (fixture build + boot): qemu/nasm/clang+clangd 14/lld/gdb/xorriso/mtools
#   * A1 (IDE language service): clangd-16 + clang-16 + bear   [D6 / D8]
#   * A9 (IDE debug backend):    gdb 16.3 in an isolated rootfs [D11]
# The clangd majors are deliberately kept side by side and never swapped:
# unversioned `clangd`/`clang` stay on 14 (P0 behaviour, no silent change) and
# the language service uses the *versioned* commands `clangd-16` / `clang-16`.
# Likewise unversioned `gdb` stays on the bookworm 13.1 the rest of the toolchain
# already used; the DAP-capable gdb 16.3 is reached as `princess-gdb`.
#
# Design constraints honoured here:
#   * NO system packages are installed.  Missing tools are fetched as .deb
#     archives (apt-get download works unprivileged / without dpkg) and unpacked
#     into the workspace prefix with dpkg-deb -x.
#   * /etc/ld.so.conf is never touched; unpacked libraries are surfaced through
#     LD_LIBRARY_PATH only (see scripts/env.sh).
#   * Core runtime libraries that the host already provides (libc6, libgcc-s1,
#     libstdc++6, libtinfo6, zlib1g, ...) are never unpacked, so the workspace
#     prefix can never shadow the host's own runtime.
#   * Idempotent: re-running skips anything already present and never fails just
#     because work was done before.
#   * Non-interactive, network operations are retried.
#
# Usage:  bash scripts/bootstrap-toolchain.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TOOLCHAIN="$ROOT/.toolchain"
PREFIX="$TOOLCHAIN/prefix"
STAGE="$TOOLCHAIN/.stage"
STAMPS="$TOOLCHAIN/.stamps"
DEBCACHE="$TOOLCHAIN/debs"
# A9: an isolated, *second* glibc world for gdb 16.3.  Kept out of $PREFIX on
# purpose -- mixing a second libc.so.6 into the prefix that env.sh puts on
# LD_LIBRARY_PATH would poison every host process (see the comment above
# install_gdb16_launcher()).
GDB16_ROOT="$TOOLCHAIN/gdb16"           # rootfs/ + debs/ + .stamps/
GDB16_PREFIX="$GDB16_ROOT/rootfs"
GDB16_DEBS="$GDB16_ROOT/debs"
GDB16_STAMPS="$GDB16_ROOT/.stamps"
GDB16_APT="$GDB16_ROOT/.apt"            # private apt lists + sources.list
export RUSTUP_HOME="$TOOLCHAIN/rustup"
export CARGO_HOME="$TOOLCHAIN/cargo"

# Target packages that must end up available.
#
# P0 set (built the reference kernel fixture):
PKGS="qemu-system-x86 nasm clang clangd lld gdb xorriso mtools"
#
# A1 language-service set (decisions D6 + D8):
#   clangd-16  the version that can actually carry IDE language service
#              (bookworm main = 1:16.0.6-15~deb12u1).  The P0 clangd 14.0.6 is
#              smoke-test only and must stay installed and usable as clangd-14.
#   clang-16   the real clang driver for LLVM 16 + its resource dir: clangd
#              resolves its builtin freestanding headers (<stdint.h> ...) from
#              <clang-16 tree>/lib/clang/16/include, which is what makes
#              -nostdlibinc viable (D7).  clang-16 pulls libclang-common-16-dev
#              (the builtin headers) and llvm-16-linker-tools.
#   bear       preferred compile_commands.json producer (D8, bear 3.1.1-1);
#              pulls libear (the LD_PRELOAD interceptor) + libspdlog1.10.
PKGS="$PKGS clangd-16 clang-16 bear"

# Core runtime / host-provided packages: never unpack, even if the recursive
# dependency walk reports them (they must come from the host so that unpacking
# cannot shadow or break the running system).
CORE_EXCLUDE="
adduser apt base-files base-passwd bash bsdutils coreutils dash debconf
debianutils diffutils dpkg e2fsprogs findutils gcc-12-base gpgv grep gzip
hostname init-system-helpers libacl1 libatomic1 libattr1 libaudit1 libblkid1
libbz2-1.0 libc-bin libc-dev-bin libc6 libc6-dev libcap-ng0 libcap2 libcap2-bin
libcom-err2 libcrypt1 libctf-nobfd0 libctf0 libdb5.3 libdebconfclient0
libext2fs2 libffi8 libgcc-12-dev libgcc-s1 libgcrypt20 libgmp10 libgomp1
libgpg-error0 libhogweed6 libitm1 liblz4-1 liblzma5 libmount1 libncursesw6
libnettle8 libnsl-dev libobjc-12-dev libobjc4 libpam-modules libpam-modules-bin
libpam0g libpcre2-8-0 libprocps8 libquadmath0 libseccomp2 libselinux1
libsemanage-common libsemanage2 libsepol2 libsmartcols1 libss2 libstdc++-12-dev
libstdc++6 libsystemd0 libtasn1-6 libtextwrap1 libtinfo6 libtirpc-common
libtirpc-dev libtirpc3 libudev1 libunistring2 libuuid1 libxxhash0 libzstd1
linux-libc-dev login logsave mawk mount ncurses-base ncurses-bin passwd perl
perl-base perl-modules-5.36 procps rpcsvc-proto sed sensible-utils sysvinit-utils
tar tzdata usr-is-merged util-linux zlib1g
"

# =============================================================================
# A9: gdb 16.3 from Debian trixie (decision D11 -- P4 needs `gdb -i=dap`)
# =============================================================================
# WHY A SECOND DISTRIBUTION: DAP was only introduced in gdb 14, and this host
# runs Debian bookworm, whose newest gdb is 13.1 ("Interpreter `dap'
# unrecognized").  The only DAP-capable build available as a *binary* is
# trixie's gdb 16.3.
#
# WHY IT CANNOT SHARE $PREFIX: trixie's gdb links against trixie's C library:
#   gdb 16.3 Depends: libc6 (>= 2.38), libstdc++6 (>= 14), libreadline8t64, ...
# and this host's bookworm libc is 2.36.  So the usual "skip anything dpkg says
# is installed" rule (see download_debs()) is WRONG here and is deliberately not
# applied: it would keep the host's 2.36 libc6, and gdb 16.3 would then die at
# startup on a missing GLIBC_2.38 symbol.  gdb16 therefore gets its OWN complete
# closure -- libc6 included -- unpacked under .toolchain/gdb16/rootfs/.
#
# WHY THE HOST IS STILL SAFE: that rootfs is never added to LD_LIBRARY_PATH.
# It is only ever reached through .toolchain/bin/princess-gdb, which re-executes
# gdb under *trixie's own* dynamic loader.  Putting trixie's libc.so.6 on the
# host loader path is the exact failure this task had to avoid:
#     head: symbol lookup error: .../libc.so.6:
#           undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE
GDB16_SUITE="${PRINCESSIDE_GDB16_SUITE:-trixie}"
GDB16_MIRROR="${PRINCESSIDE_GDB16_MIRROR:-https://mirrors.ustc.edu.cn/debian}"
GDB16_PKG="${PRINCESSIDE_GDB16_PKG:-gdb}"
# Prefer an explicit version when more than one candidate ever exists.
GDB16_MIN_MAJOR="${PRINCESSIDE_GDB16_MIN_MAJOR:-14}"

# Note for users: if you're behind a firewall or have slow access to
# USTC mirror, override with a local mirror:
#   PRINCESSIDE_GDB16_MIRROR=https://deb.debian.org/debian  (official, slower)
#   PRINCESSIDE_GDB16_MIRROR=https://mirrors.tuna.tsinghua.edu.cn/debian
# The suite 'trixie' (Debian testing) can also be specified as 'testing'.

# rustup download endpoints.  static.rust-lang.org measured at ~62 B/s from this
# host (unusable).  Override with RUSTUP_DIST_SERVER / RUSTUP_UPDATE_ROOT in the
# environment to change this.
#
# History, because this default has now broken twice:
#   * mirrors.tuna.tsinghua.edu.cn/rustup started returning HTTP 403 for every
#     URL under it (rustup-init and dist/channel-rust-stable.toml alike), which
#     made this script die after three futile retries.
#   * the campus joint mirror (校园网联合镜像站) serves the rust tree under
#     /rustup -- NOT /rust-static, which 404s.
#
# Measured on this host 2026-09-13 by downloading the full 21 MB rustup-init:
#   mirrors.cernet.edu.cn/rustup          5.7 s  (3.7 MB/s)   <-- selected
#   mirrors.ustc.edu.cn/rust-static       7.2 s  (2.9 MB/s)
#   mirrors.tuna.tsinghua.edu.cn/rustup   HTTP 403
#
# cernet redirects /rustup to cmcc.mirrors.ustc.edu.cn/rust-static/, so plain
# USTC remains the fallback if the joint mirror is ever unreachable.
: "${PRINCESSIDE_RUSTUP_DIST_SERVER:=https://mirrors.cernet.edu.cn/rustup}"
RUSTUP_DIST_SERVER="$PRINCESSIDE_RUSTUP_DIST_SERVER"
export RUSTUP_DIST_SERVER
export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-$RUSTUP_DIST_SERVER/rustup}"

log()  { printf '[bootstrap] %s\n' "$*"; }
warn() { printf '[bootstrap][warn] %s\n' "$*" >&2; }
die()  { printf '[bootstrap][error] %s\n' "$*" >&2; exit 1; }

retry() {
  local n=0
  until "$@"; do
    n=$((n + 1))
    if [ "$n" -ge 4 ]; then
      warn "gave up after $n attempts: $*"
      return 1
    fi
    warn "attempt $n failed, retrying in $((n * 3))s: $*"
    sleep $((n * 3))
  done
}

# Parallel download helper.
# $1 = debs directory (where to download to)
# $2 = apt options file (optional, for gdb16)
# $3 = file containing package names (one per line)
# $4 = log prefix (for error messages)
# Returns 0 if all packages downloaded successfully, 1 if any failed
#
# Packages are handed to apt-get in BATCHES (PRINCESSIDE_DL_BATCH, default 32)
# instead of one process per package.  apt's start-up cost is per process, not
# per package: measured on this host, `apt-get download --print-uris` costs
# ~0.33 s whether given one package name or eight, because it re-reads the
# package lists either way.  The main toolchain resolves to ~240 packages, so
# one-process-per-package burned ~80 s of pure start-up before any byte moved.
download_parallel() {
  local debs_dir="$1"
  local apt_opts_file="$2"
  local pkg_list_file="$3"
  local log_prefix="$4"

  if [ ! -s "$pkg_list_file" ]; then
    return 0
  fi

  local pkg_count
  pkg_count=$(wc -l < "$pkg_list_file")
  local jobs="${PRINCESSIDE_DL_JOBS:-8}"
  local batch="${PRINCESSIDE_DL_BATCH:-32}"

  # Create a temporary DIRECTORY to track failures.  Each failing package gets a
  # file named after it inside this directory (e.g. $fail_dir/badpkg).  Touching
  # a file is atomic (no concurrent-write corruption) and the directory is
  # visible to the workers because its path is exported into the environment.
  local fail_dir
  fail_dir=$(mktemp -d)

  # Drop everything already cached before spending an apt call on it.  This runs
  # in the parent so a batch never contains packages that are already on disk.
  local pending
  pending=$(mktemp)
  local pkg
  while IFS= read -r pkg; do
    [ -n "$pkg" ] || continue
    if [ -n "$(ls "$debs_dir/${pkg}_"*.deb 2>/dev/null | head -n1 || true)" ]; then
      continue
    fi
    printf '%s\n' "$pkg"
  done < "$pkg_list_file" > "$pending"

  local todo
  todo=$(wc -l < "$pending")
  log "$log_prefix: $pkg_count package(s), $todo to fetch, $jobs parallel jobs, $batch per apt call"

  if [ "$todo" -eq 0 ]; then
    rm -f "$pending"
    rm -rf "$fail_dir"
    return 0
  fi

  # One apt process per batch.  The batch's package names arrive as positional
  # args; the fixed paths travel through exported variables so they survive the
  # xargs -> bash -c boundary (a shell variable would not).
  download_batch() {
    local -a pkgs=()
    local p
    for p in "$@"; do
      [ -n "$p" ] || continue
      # A concurrent batch may have fetched it already.
      if [ -n "$(ls "$PRINCESSIDE_DL_DEBS/${p}_"*.deb 2>/dev/null | head -n1 || true)" ]; then
        continue
      fi
      pkgs+=("$p")
    done
    [ "${#pkgs[@]}" -eq 0 ] && return 0

    local apt_cmd="apt-get"
    if [ -n "$PRINCESSIDE_DL_OPTS" ] && [ -f "$PRINCESSIDE_DL_OPTS" ]; then
      apt_cmd="apt-get $(cat "$PRINCESSIDE_DL_OPTS")"
    fi

    # Happy path: the whole batch in one apt invocation.
    if ( cd "$PRINCESSIDE_DL_DEBS" && retry $apt_cmd download -q \
           -o APT::Sandbox::User=root \
           --no-install-recommends "${pkgs[@]}" >/dev/null 2>&1 ); then
      return 0
    fi

    # The batch failed as a whole -- apt aborts the entire call when one name
    # cannot be resolved.  Re-run it one package at a time so a single bad name
    # neither hides the rest nor loses the ones that would have downloaded.
    for p in "${pkgs[@]}"; do
      if [ -n "$(ls "$PRINCESSIDE_DL_DEBS/${p}_"*.deb 2>/dev/null | head -n1 || true)" ]; then
        continue
      fi
      if ( cd "$PRINCESSIDE_DL_DEBS" && retry $apt_cmd download -q \
             -o APT::Sandbox::User=root \
             --no-install-recommends "$p" >/dev/null 2>&1 ); then
        continue
      fi
      warn "$PRINCESSIDE_DL_PREFIX: could not download $p"
      # Record failure: one file per package -- atomic, no flock needed.
      touch "$PRINCESSIDE_DL_FAIL/$p"
    done
    return 0
  }

  export PRINCESSIDE_DL_DEBS="$debs_dir"
  export PRINCESSIDE_DL_OPTS="$apt_opts_file"
  export PRINCESSIDE_DL_PREFIX="$log_prefix"
  export PRINCESSIDE_DL_FAIL="$fail_dir"
  export -f download_batch warn retry log

  # -n $batch = packages per apt call, -P $jobs = batches in flight.
  xargs -a "$pending" -n "$batch" -P "$jobs" bash -c 'download_batch "$@"' _ || true

  rm -f "$pending"

  # Count failures: number of files in fail_dir = number of failed packages.
  local fail_count=0
  if [ -n "$(ls -A "$fail_dir" 2>/dev/null)" ]; then
    fail_count=$(ls -1 "$fail_dir" | wc -l)
    warn "$log_prefix: $fail_count package(s) failed to download"
    for f in "$fail_dir"/*; do
      [ -e "$f" ] || continue
      warn "  failed: $(basename "$f")"
    done
  fi

  rm -rf "$fail_dir"

  return $([ "$fail_count" -eq 0 ] && echo 0 || echo 1)
}

mkdir -p "$TOOLCHAIN" "$PREFIX" "$DEBCACHE" "$STAMPS" "$RUSTUP_HOME" "$CARGO_HOME"

# =============================================================================
# 1. Rust toolchain (rustup -> workspace-local RUSTUP_HOME / CARGO_HOME)
# =============================================================================
install_rust() {
  if [ -x "$CARGO_HOME/bin/cargo" ] && [ -x "$CARGO_HOME/bin/rustc" ]; then
    log "rust: already installed, skipping"
    return 0
  fi

  log "rust: installing stable toolchain (profile=minimal, +rustfmt +clippy)"
  log "rust: dist server = $RUSTUP_DIST_SERVER"

  if [ ! -x "$TOOLCHAIN/rustup-init" ]; then
    retry curl -fsSL --retry 5 --retry-delay 3 \
      -o "$TOOLCHAIN/rustup-init" \
      "$RUSTUP_DIST_SERVER/rustup/dist/x86_64-unknown-linux-gnu/rustup-init" \
      || die "could not download rustup-init"
    chmod +x "$TOOLCHAIN/rustup-init"
  fi

  # --no-modify-path keeps the installer from writing to ~/.profile (outside the
  # workspace, which the file sandbox refuses).
  "$TOOLCHAIN/rustup-init" -y --no-modify-path \
    --profile minimal --default-toolchain stable \
    -c rustfmt -c clippy \
    || die "rustup-init failed"
}

# =============================================================================
# 2. .deb based tools
# =============================================================================
is_installed() {
  local st
  st="$(dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true)"
  case "$st" in *"install ok installed"*) return 0 ;; *) return 1 ;; esac
}

is_excluded() {
  local p="$1"
  case " $(echo $CORE_EXCLUDE) " in *" $p "*) return 0 ;; *) return 1 ;; esac
}

deb_file() { # $1 = package name -> path of the .deb in the cache (may be empty)
  local f
  f="$(ls "$DEBCACHE/$1"_*.deb 2>/dev/null | head -n1 || true)"
  printf '%s' "$f"
}

download_debs() {
  local wanted=()
  local p
  for p in $(apt-cache depends --recurse \
        --no-recommends --no-suggests --no-conflicts --no-breaks \
        --no-replaces --no-enhances $PKGS 2>/dev/null \
      | grep '^[A-Za-z0-9]' | sort -u); do
    if is_installed "$p"; then continue; fi       # host already provides it
    if is_excluded  "$p"; then
      warn "skip (core runtime, must come from host): $p"
      continue
    fi
    if [ -n "$(deb_file "$p")" ]; then continue; fi  # already cached
    wanted+=("$p")
  done

  if [ "${#wanted[@]}" -eq 0 ]; then
    log "deb: nothing to download"
    return 0
  fi
  
  # Write package list to temporary file for parallel download
  local pkg_list_file
  pkg_list_file=$(mktemp)
  printf '%s\n' "${wanted[@]}" > "$pkg_list_file"
  
  # Download in parallel (no apt opts file for main toolchain)
  download_parallel "$DEBCACHE" "" "$pkg_list_file" "deb"
  local exit_code=$?
  
  rm -f "$pkg_list_file"
  return $exit_code
}

extract_debs() {
  local f p base stamp count=0 skipped=0

  # One stamp per unpacked archive so that a second run skips the work instead
  # of re-extracting several hundred megabytes.  If the prefix itself is gone
  # but the stamps survived, start over from scratch.
  mkdir -p "$STAMPS"
  if [ -z "$(ls -A "$PREFIX" 2>/dev/null)" ]; then
    rm -rf "$STAMPS"
    mkdir -p "$STAMPS"
  fi

  for f in "$DEBCACHE"/*.deb; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    p="${base%%_*}"
    if is_excluded "$p"; then continue; fi

    stamp="$STAMPS/$base"
    if [ -f "$stamp" ]; then
      skipped=$((skipped + 1))
      continue
    fi

    # dpkg-deb -x untars into the prefix and overwrites existing files rather
    # than failing, so repeating it is safe (files already contributed by a
    # concurrent or earlier bootstrap are simply rewritten).
    if dpkg-deb -x "$f" "$PREFIX" 2>/dev/null; then
      touch "$stamp"
      count=$((count + 1))
    else
      warn "dpkg-deb -x failed: $base"
    fi
  done
  log "deb: unpacked $count archive(s), skipped $skipped already unpacked"

  # Safety net: drop any library that the host already provides, so that
  # LD_LIBRARY_PATH can never shadow a host runtime library.
  local libdir n removed=0
  for libdir in "$PREFIX/usr/lib/x86_64-linux-gnu" "$PREFIX/lib/x86_64-linux-gnu" \
                "$PREFIX/usr/lib" "$PREFIX/lib"; do
    [ -d "$libdir" ] || continue
    while IFS= read -r -d '' l; do
      n="$(basename "$l")"
      if [ -e "/usr/lib/x86_64-linux-gnu/$n" ] || [ -e "/usr/lib/$n" ]; then
        rm -f "$l"; removed=$((removed + 1))
      fi
    done < <(find "$libdir" -maxdepth 1 -name '*.so*' -print0 2>/dev/null)
  done
  log "deb: removed $removed library file(s) that the host already provides"
}

# QEMU resolves BIOS/option-ROM data files relative to its data dir.  seabios
# ships them under /usr/share/seabios in Debian, qemu-system-data under
# /usr/share/qemu, and qemu only searches one -L directory, so make the
# seabios blobs visible from the qemu data dir.
link_qemu_data() {
  local qdir="$PREFIX/usr/share/qemu"
  local sdir="$PREFIX/usr/share/seabios"
  [ -d "$qdir" ] || { warn "qemu data dir missing: $qdir"; return 0; }
  if [ -d "$sdir" ]; then
    local f
    for f in "$sdir"/*; do
      [ -e "$f" ] || continue
      ln -sf "$f" "$qdir/$(basename "$f")" 2>/dev/null || true
    done
    log "qemu: linked seabios data files into $qdir"
  fi
  # iPXE option ROMs likewise live in their own package directory.
  local idir="$PREFIX/usr/lib/ipxe/qemu"
  if [ -d "$idir" ]; then
    local f
    for f in "$idir"/*; do
      [ -e "$f" ] || continue
      ln -sf "$f" "$qdir/$(basename "$f")" 2>/dev/null || true
    done
    log "qemu: linked ipxe option ROMs into $qdir"
  fi
}

# =============================================================================
# 2b. bear: workspace-local launcher
# =============================================================================
# Debian's bear 3.1.1 hard-codes its *installed* locations for the four
# artifacts it needs at run time:
#
#   --bear-path   /usr/bin/bear
#   --library     /usr/$LIB/bear/libexec.so          (LD_PRELOAD interceptor)
#   --wrapper     /usr/lib/x86_64-linux-gnu/bear/wrapper
#   --wrapper-dir /usr/lib/x86_64-linux-gnu/bear/wrapper.d
#
# We unpack into the workspace prefix instead of installing, so all four
# defaults are wrong and every `bear -- make` would fail (or worse, silently
# produce an empty compile_commands.json).  `.toolchain/bin/bear` is a tiny
# launcher that supplies them; scripts/env.sh puts .toolchain/bin *before*
# the prefix's usr/bin so the launcher wins while the real `bear` stays
# reachable as `$PREFIX/usr/bin/bear`.
install_bear_launcher() {
  local real="$PREFIX/usr/bin/bear"
  local libexec="$PREFIX/usr/lib/x86_64-linux-gnu/bear/libexec.so"
  local wrapper="$PREFIX/usr/lib/x86_64-linux-gnu/bear/wrapper"
  local wrapperdir="$PREFIX/usr/lib/x86_64-linux-gnu/bear/wrapper.d"
  local launcher="$TOOLCHAIN/bin/bear"

  if [ ! -x "$real" ]; then
    warn "bear: $real is missing, not writing a launcher"
    return 0
  fi
  # Missing runtime bits would make bear fail *at build time* (rc != 0) or, for
  # libexec.so, silently record nothing -- so say it out loud here.
  local p
  for p in "$libexec" "$wrapper" "$wrapperdir"; do
    [ -e "$p" ] || warn "bear: expected runtime path is missing: $p"
  done

  mkdir -p "$TOOLCHAIN/bin"
  cat >"$launcher" <<EOF
#!/bin/sh
# Generated by scripts/bootstrap-toolchain.sh -- do not edit by hand.
#
# Wrapper around the workspace-local bear (3.1.1).  It only fills in the four
# path options bear cannot guess when it is unpacked into a prefix instead of
# installed into /usr.
PREFIX="$PREFIX"

# --version / --help / bare invocation: no build is observed, pass straight
# through so the output is byte-identical to the real binary's.
case "\${1:-}" in
  --version|--help|-h|"") exec "\$PREFIX/usr/bin/bear" "\$@" ;;
esac

exec "\$PREFIX/usr/bin/bear" \\
  --bear-path  "\$PREFIX/usr/bin/bear" \\
  --library    "\$PREFIX/usr/lib/x86_64-linux-gnu/bear/libexec.so" \\
  --wrapper    "\$PREFIX/usr/lib/x86_64-linux-gnu/bear/wrapper" \\
  --wrapper-dir "\$PREFIX/usr/lib/x86_64-linux-gnu/bear/wrapper.d" \\
  "\$@"
EOF
  chmod +x "$launcher"
  log "bear: launcher written to $launcher"
}

# =============================================================================
# 2c. gdb 16.3: isolated trixie rootfs + `princess-gdb` launcher (A9 / D11)
# =============================================================================
# The whole closure is fetched from a *private* apt source list written inside
# the workspace.  Nothing under /etc is read or written for this, so the host's
# bookworm package state stays exactly as it was (and `apt-get download` still
# works unprivileged: it never touches dpkg).
gdb16_apt_opts() {
  printf '%s\n' \
    -o "Dir::Etc::sourcelist=$GDB16_APT/sources.list" \
    -o "Dir::Etc::sourceparts=-" \
    -o "Dir::State::lists=$GDB16_APT/lists" \
    -o "APT::Sandbox::User=root" \
    -o "APT::Get::List-Cleanup=0"
}

install_gdb16() {
  mkdir -p "$GDB16_PREFIX" "$GDB16_DEBS" "$GDB16_STAMPS" "$GDB16_APT/lists/partial"

  # --- private apt source list (idempotent: content, not timestamp) ----------
  local sources="$GDB16_APT/sources.list"
  local want_line="deb $GDB16_MIRROR/ $GDB16_SUITE main"
  if [ ! -f "$sources" ] || [ "$(cat "$sources")" != "$want_line" ]; then
    printf '%s\n' "$want_line" >"$sources"
  fi

  # --- make sure the private index is present AND usable ----------------------
  # "the directory is not empty" is not a safe test: apt writes the Packages file
  # late, and a half-written or stale cache makes `apt-cache depends --recurse`
  # silently return an *incomplete* closure -- which is how an earlier revision
  # of this script produced a rootfs missing gdb's Python support.  So probe for
  # a real answer and refetch when it is not there.
  gdb16_index_usable() {
    apt-cache $(gdb16_apt_opts) policy "$GDB16_PKG" 2>/dev/null \
      | sed -n 's/^  Candidate: //p' | grep -qv '^(none)$'
  }

  if ! gdb16_index_usable; then
    log "gdb16: fetching $GDB16_SUITE package index from $GDB16_MIRROR"
    retry apt-get $(gdb16_apt_opts) update >/dev/null 2>&1 \
      || { warn "gdb16: could not fetch the $GDB16_SUITE package index"; return 1; }
  fi
  if ! gdb16_index_usable; then
    warn "gdb16: '$GDB16_PKG' still has no candidate in $GDB16_SUITE after an index refresh"
    return 1
  fi
  # Force the whole Packages file to be read once, up front.  apt writes the
  # index into its cache lazily and the `depends --recurse` walk below forks one
  # apt-cache per dependency; if the index is still settling, individual lookups
  # in that walk have been observed to come back empty, which silently drops a
  # real package from the closure.  A single full dump loads the cache first.
  apt-cache $(gdb16_apt_opts) dump >/dev/null 2>&1 || true

  # The walk must be stable: it is the input to what gets installed, so a
  # flapping result is a correctness bug, not a cosmetic one.
  local walk_a walk_b
  do_gdb16_walk() {
    apt-cache $(gdb16_apt_opts) depends --recurse \
      --no-recommends --no-suggests --no-conflicts --no-breaks \
      --no-replaces --no-enhances "$GDB16_PKG" 2>/dev/null \
      | grep '^[A-Za-z0-9]' | sort -u
  }
  walk_a="$(do_gdb16_walk)"
  walk_b="$(do_gdb16_walk)"
  if [ "$walk_a" != "$walk_b" ]; then
    warn "gdb16: dependency walk is unstable; retrying after a cache flush"
    apt-cache $(gdb16_apt_opts) dump >/dev/null 2>&1 || true
    walk_a="$(do_gdb16_walk)"
    walk_b="$(do_gdb16_walk)"
    [ "$walk_a" = "$walk_b" ] \
      || { warn "gdb16: dependency walk does not stabilise ($(printf '%s\n' "$walk_a" | wc -l) vs $(printf '%s\n' "$walk_b" | wc -l) packages)"; return 1; }
  fi

  # --- resolve the recursive dependency closure ------------------------------
  local cand
  cand="$(apt-cache $(gdb16_apt_opts) policy "$GDB16_PKG" 2>/dev/null \
          | sed -n 's/^  Candidate: //p')"
  log "gdb16: $GDB16_PKG candidate in $GDB16_SUITE is $cand"

  # Resolve against the *suite* only.  apt-cache consults the host dpkg status
  # database too, so a package that is installed locally at a different version
  # can change what is reported as the dependency set; pinning the pin priority
  # and the architecture keeps the walk deterministic across runs.
  #
  # Membership is decided from ONE dump of the suite index, kept in a file, and
  # never from per-package `apt-cache show` calls: those fork a fresh apt-cache
  # each time and were observed to occasionally come back empty for a package
  # that is definitely in the suite.  That flakiness silently dropped libc6 from
  # the closure and produced a rootfs with no dynamic loader in it.
  local idx="$GDB16_APT/in-suite.txt"
  apt-cache $(gdb16_apt_opts) dump 2>/dev/null \
    | sed -n 's/^Package: //p' | sort -u >"$idx" || true
  if [ ! -s "$idx" ]; then
    warn "gdb16: could not enumerate the $GDB16_SUITE package index"
    return 1
  fi

  local closure=() p
  local uncached=()
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    # NOTE: no is_installed() filter here, on purpose -- see the header comment
    # above.  gdb 16.3 needs trixie's libc6/libstdc++6, not the host's older
    # ones, so *every* dependency comes from trixie to keep the rootfs coherent.
    if ! grep -qxF "$p" "$idx"; then
      # Not a real package in this suite: a virtual name, or an obsolete
      # transitional package.  Safe to skip -- but say so, because a *missing*
      # real dependency here is exactly how the rootfs goes incomplete.
      warn "gdb16: '$p' is not a package in $GDB16_SUITE (virtual/host-only), skipping"
      continue
    fi
    # Already have the archive?  It is settled -- accept it and never ask apt
    # about it again (a second bootstrap run must not touch the network).
    if [ -n "$(ls "$GDB16_DEBS/$p"_*.deb 2>/dev/null | head -n1 || true)" ]; then
      closure+=("$p")
      continue
    fi
    uncached+=("$p")
  done < <(printf '%s\n' "$walk_a")

  # A name can exist in the suite *and* still be unfetchable: apt then only knows
  # the version already in the host's dpkg status (trixie's `mime-support` is
  # such a stub -- it resolves to the locally installed 3.66 and has no source in
  # the suite at all).  Asking for it can only ever fail, and leaving it in would
  # make every later run re-attempt a download it can never win.
  #
  # `apt-get download --print-uris --no-download` resolves without fetching, so
  # the whole uncached set can be classified offline in a single apt invocation.
  if [ "${#uncached[@]}" -gt 0 ]; then
    local fetchable="$GDB16_APT/fetchable.txt"
    ( cd "$GDB16_DEBS" && apt-get $(gdb16_apt_opts) download -q \
        --no-install-recommends --print-uris "${uncached[@]}" 2>/dev/null ) \
      | sed -n "s#.*/\([^/']*\)_.*#\1#p" | sed 's/%3a/:/g' | sort -u >"$fetchable" || true
    for p in "${uncached[@]}"; do
      if grep -qxF "$p" "$fetchable"; then
        closure+=("$p")
      else
        # Fall back to a per-package probe: print-uris prints the *filename*, and
        # a package whose .deb is named differently from its package name must
        # not be dropped just because the name match above missed.
        if ( cd "$GDB16_DEBS" && apt-get $(gdb16_apt_opts) download -q \
               --no-install-recommends --print-uris "$p" >/dev/null 2>&1 ); then
          closure+=("$p")
        else
          warn "gdb16: '$p' has no fetchable source, skipping"
        fi
      fi
    done
  fi

  if [ "${#closure[@]}" -eq 0 ]; then
    warn "gdb16: dependency resolution returned nothing"
    return 1
  fi
  log "gdb16: resolved ${#closure[@]} package(s) in the $GDB16_SUITE closure"

  # --- download what is not cached yet ---------------------------------------
  local missing=()
  for p in "${closure[@]}"; do
    [ -n "$(ls "$GDB16_DEBS/$p"_*.deb 2>/dev/null | head -n1 || true)" ] && continue
    missing+=("$p")
  done
  if [ "${#missing[@]}" -eq 0 ]; then
    log "gdb16: nothing to download (${#closure[@]} package(s) already cached)"
  else
    # Write package list to temporary file for parallel download
    local pkg_list_file
    pkg_list_file=$(mktemp)
    printf '%s\n' "${missing[@]}" > "$pkg_list_file"
    
    # Create apt options file for gdb16
    local apt_opts_file
    apt_opts_file=$(mktemp)
    gdb16_apt_opts > "$apt_opts_file"
    
    # Download in parallel
    download_parallel "$GDB16_DEBS" "$apt_opts_file" "$pkg_list_file" "gdb16"
    local exit_code=$?
    
    rm -f "$pkg_list_file" "$apt_opts_file"
    
    if [ "$exit_code" -ne 0 ]; then
      warn "gdb16: some packages failed to download; the loader check below decides whether that matters"
    fi
  fi

  # --- unpack, one stamp per archive (idempotent) ----------------------------
  local f base count=0 skipped=0
  if [ -z "$(ls -A "$GDB16_PREFIX" 2>/dev/null)" ]; then
    rm -rf "$GDB16_STAMPS"; mkdir -p "$GDB16_STAMPS"
  fi
  for f in "$GDB16_DEBS"/*.deb; do
    [ -e "$f" ] || continue
    base="$(basename "$f")"
    [ -f "$GDB16_STAMPS/$base" ] && { skipped=$((skipped + 1)); continue; }
    if dpkg-deb -x "$f" "$GDB16_PREFIX" 2>/dev/null; then
      touch "$GDB16_STAMPS/$base"; count=$((count + 1))
    else
      warn "gdb16: dpkg-deb -x failed: $base"
    fi
  done
  log "gdb16: unpacked $count archive(s), skipped $skipped already unpacked"

  install_gdb16_launcher

  if [ ! -x "$GDB16_PREFIX/usr/bin/gdb" ]; then
    warn "gdb16: $GDB16_PREFIX/usr/bin/gdb is missing"
    return 1
  fi

  # --- completeness: every shared library gdb needs must resolve inside here --
  # This is the assertion that actually matters.  Dependency resolution can be
  # defeated (an index that was not ready yet, a virtual package name, a name
  # that only exists in the host's dpkg status), and the failure mode is a gdb
  # that dies at startup with "error while loading shared libraries".  Rather
  # than trusting the walk, ask the rootfs's own loader what it can resolve.
  gdb16_check_libraries || return 1
}

# `ld-linux --list` reports, for the given binary, every library it would load.
# Any "not found" line means the rootfs is incomplete and gdb16 is not usable.
gdb16_check_libraries() {
  local loader="$GDB16_PREFIX/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
  local libs="$GDB16_PREFIX/usr/lib/x86_64-linux-gnu:$GDB16_PREFIX/usr/lib:$GDB16_PREFIX/usr/lib64:$GDB16_PREFIX/lib/x86_64-linux-gnu:$GDB16_PREFIX/lib"
  local out unresolved

  [ -x "$loader" ] || { warn "gdb16: loader $loader missing"; return 1; }

  out="$("$loader" --library-path "$libs" --list "$GDB16_PREFIX/usr/bin/gdb" 2>&1 || true)"
  unresolved="$(printf '%s\n' "$out" | grep 'not found' || true)"
  if [ -n "$unresolved" ]; then
    warn "gdb16: gdb has unresolved libraries -- the $GDB16_SUITE closure is incomplete:"
    printf '%s\n' "$unresolved" | while IFS= read -r l; do warn "  $l"; done
    return 1
  fi
  log "gdb16: all $(printf '%s\n' "$out" | grep -c '=>') shared libraries resolve inside the rootfs"

  # The DAP interpreter is implemented in Python *inside gdb*, so a rootfs that
  # cannot import gdb's Python modules would still start but silently have no
  # DAP.  Check the module tree is actually there.
  if [ ! -d "$GDB16_PREFIX/usr/share/gdb/python/gdb/dap" ]; then
    warn "gdb16: $GDB16_PREFIX/usr/share/gdb/python/gdb/dap is missing (no DAP)"
    return 1
  fi
  log "gdb16: gdb's DAP python package is present"
  return 0
}

# `princess-gdb` is the one command a caller has to know.  It is the whole point
# of the isolation: the caller never has to learn the rootfs layout, never has to
# set LD_LIBRARY_PATH, and above all never has to put trixie's libc.so.6 on the
# host's library path (which breaks *every* host process, including `head`).
install_gdb16_launcher() {
  local loader="$GDB16_PREFIX/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
  local real="$GDB16_PREFIX/usr/bin/gdb"
  local launcher="$TOOLCHAIN/bin/princess-gdb"

  if [ ! -x "$real" ]; then
    warn "princess-gdb: $real is missing, not writing a launcher"
    return 0
  fi
  if [ ! -x "$loader" ]; then
    warn "princess-gdb: $loader is missing, not writing a launcher"
    return 0
  fi

  mkdir -p "$TOOLCHAIN/bin"
  cat >"$launcher" <<EOF
#!/bin/sh
# Generated by scripts/bootstrap-toolchain.sh -- do not edit by hand.
#
# PrincessIDE debug backend: GNU gdb 16.3 (Debian $GDB16_SUITE), the first gdb
# with the built-in DAP interpreter that P4 requires (decision D11).
#
# This gdb is built against $GDB16_SUITE's glibc, which is NEWER than the host's.
# It is therefore started through $GDB16_SUITE's own dynamic loader with an
# explicit --library-path, and the host environment is left completely untouched:
#   * trixie's libc.so.6 is NEVER put on the host LD_LIBRARY_PATH -- doing that
#     breaks every process on the machine ("undefined symbol:
#     __tunable_is_initialized, version GLIBC_PRIVATE").
#   * LD_LIBRARY_PATH is cleared for the child so a caller's own value cannot
#     leak a second libc into the mix.
#
# Usage:
#   princess-gdb --version
#   princess-gdb -q -i=dap                      # DAP over stdio
#   princess-gdb -q vmlinux -ex 'target remote :1234'
GDB16_ROOT="$GDB16_PREFIX"
LOADER="\$GDB16_ROOT/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"

# Library search order inside the isolated rootfs: the multiarch dir carries the
# real libc/loader set, then the generic dirs for anything else.
GDB16_LIBS="\$GDB16_ROOT/usr/lib/x86_64-linux-gnu:\$GDB16_ROOT/usr/lib:\$GDB16_ROOT/usr/lib64:\$GDB16_ROOT/lib/x86_64-linux-gnu:\$GDB16_ROOT/lib"

# gdb's Python support needs its own stdlib, which is inside the rootfs too.
# PYTHONHOME/PYTHONPATH are exported only for this child.
PYTHONHOME="\$GDB16_ROOT/usr"
PYTHONPATH="\$GDB16_ROOT/usr/lib/python3.13:\$GDB16_ROOT/usr/lib/python3/dist-packages"
export PYTHONHOME PYTHONPATH

# Deliberately NOT exporting LD_LIBRARY_PATH to the host: the loader below is
# given the search path as an argument instead, and the variable is cleared so
# the caller's value cannot interfere.
unset LD_LIBRARY_PATH

exec "\$LOADER" --library-path "\$GDB16_LIBS" "\$GDB16_ROOT/usr/bin/gdb" "\$@"
EOF
  chmod +x "$launcher"
  log "princess-gdb: launcher written to $launcher"
}

# =============================================================================
# 3. Verification
# =============================================================================
verify() {
  log "verifying toolchain"
  local fail=0
  export PATH="$TOOLCHAIN/bin:$CARGO_HOME/bin:$PREFIX/usr/lib/llvm-14/bin:$PREFIX/usr/lib/llvm-16/bin:$PREFIX/usr/bin:$PATH"
  export LD_LIBRARY_PATH="$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/lib/x86_64-linux-gnu:$PREFIX/usr/lib/llvm-16/lib:$PREFIX/usr/lib/llvm-14/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

  local check
  for check in "cargo --version" "rustc --version" "rustfmt --version" \
               "cargo-clippy --version" "qemu-system-x86_64 --version" \
               "nasm -v" "clang --version" "clangd --version" "ld.lld --version" \
               "gdb --version" "xorriso --version" "mformat --version" \
               "clang-16 --version" "clangd-16 --version" "clangd-14 --version" \
               "bear --version"; do
    if out="$(eval "$check" 2>&1 | head -n1)"; then
      log "  ok   $check  ->  $out"
    else
      warn "  FAIL $check"
      fail=1
    fi
  done

  # A1: the two clangd majors must stay distinguishable and must report what
  # their names promise -- "it ran" is not enough here.
  local v14 v16
  v14="$(clangd-14 --version 2>&1 | head -n1)"
  v16="$(clangd-16 --version 2>&1 | head -n1)"
  case "$v14" in
    *14.*) log "  ok   clangd-14 reports 14.x" ;;
    *)     warn "  FAIL clangd-14 does not report 14.x: $v14"; fail=1 ;;
  esac
  case "$v16" in
    *16.*) log "  ok   clangd-16 reports 16.x" ;;
    *)     warn "  FAIL clangd-16 does not report 16.x: $v16"; fail=1 ;;
  esac

  # A9: `princess-gdb` must run *inside its isolated rootfs* and must be >= 14,
  # because DAP (the whole reason it exists) only appears in gdb 14.
  if [ -x "$TOOLCHAIN/bin/princess-gdb" ]; then
    local gv gmaj
    gv="$("$TOOLCHAIN/bin/princess-gdb" --version 2>&1 | head -n1)"
    case "$gv" in
      *"GNU gdb"*) log "  ok   princess-gdb --version  ->  $gv" ;;
      *) warn "  FAIL princess-gdb did not report a GNU gdb version: $gv"; fail=1 ;;
    esac
    gmaj="$(printf '%s' "$gv" | grep -oE '[0-9]+\.[0-9]+' | head -n1 | cut -d. -f1)"
    if [ -n "$gmaj" ] && [ "$gmaj" -ge "$GDB16_MIN_MAJOR" ] 2>/dev/null; then
      log "  ok   princess-gdb reports >= $GDB16_MIN_MAJOR (got $gmaj)"
    else
      warn "  FAIL princess-gdb is < $GDB16_MIN_MAJOR (got '${gmaj:-?}'): $gv"
      fail=1
    fi
    # The DAP interpreter is the capability the version number implies; a gdb 14
    # build configured without it would still print "14".  Check it directly.
    if printf 'Content-Length: 2\r\n\r\n{}' \
         | timeout 30 "$TOOLCHAIN/bin/princess-gdb" -q -i=dap >/dev/null 2>&1; then
      log "  ok   princess-gdb -i=dap starts (DAP interpreter present)"
    else
      # A DAP session fed garbage exits non-zero, but must not say "unrecognized"
      if printf 'Content-Length: 2\r\n\r\n{}' \
           | timeout 30 "$TOOLCHAIN/bin/princess-gdb" -q -i=dap 2>&1 \
           | grep -qi 'unrecognized'; then
        warn "  FAIL princess-gdb has no DAP interpreter"
        fail=1
      else
        log "  ok   princess-gdb -i=dap starts (DAP interpreter present)"
      fi
    fi
  else
    warn "  FAIL princess-gdb launcher is missing"
    fail=1
  fi

  if [ "$fail" -ne 0 ]; then
    warn "one or more tools failed to run; run scripts/doctor.sh for details"
    return 1
  fi
  log "all tools verified"
}

install_rust
download_debs
extract_debs
link_qemu_data
install_bear_launcher
# gdb16 is optional (requires Debian trixie mirror access)
# If it fails, continue with the rest of the toolchain
if ! install_gdb16 2>/dev/null; then
  warn "gdb16 installation failed (network/mirror issue); skipping"
  warn "P4 debugging feature will not be available until gdb16 is installed"
  warn "To retry: PRINCESSIDE_GDB16_MIRROR=https://mirrors.ustc.edu.cn/debian bash scripts/bootstrap-toolchain.sh"
fi
verify

log "done.  Activate with:  source $ROOT/scripts/env.sh"
