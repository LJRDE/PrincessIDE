#!/usr/bin/env bash
# PrincessIDE P0 — toolchain bootstrap.
#
# Builds a fully self-contained development toolchain inside the workspace:
#   .toolchain/rustup        RUSTUP_HOME
#   .toolchain/cargo         CARGO_HOME  (cargo, rustc, rustfmt, clippy)
#   .toolchain/prefix/       root of the extracted .deb tree
#   .toolchain/debs/         .deb download cache
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
export RUSTUP_HOME="$TOOLCHAIN/rustup"
export CARGO_HOME="$TOOLCHAIN/cargo"

# Target packages that must end up available.
PKGS="qemu-system-x86 nasm clang clangd lld gdb xorriso mtools"

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

# rustup download endpoints.  static.rust-lang.org measured at ~62 B/s from this
# host (unusable); the Tsinghua mirror measured at ~7 MB/s.  Override with
# RUSTUP_DIST_SERVER / RUSTUP_UPDATE_ROOT in the environment to change this.
: "${PRINCESSIDE_RUSTUP_DIST_SERVER:=https://mirrors.tuna.tsinghua.edu.cn/rustup}"
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
  log "deb: downloading ${#wanted[@]} package(s) into $DEBCACHE"

  # apt-get download works without dpkg; run it from the cache directory so
  # archives land in the workspace.  APT::Sandbox::User=root stops apt from
  # dropping to the unprivileged _apt user, which cannot write into the
  # workspace and would otherwise emit a warning on every single download.
  for p in "${wanted[@]}"; do
    ( cd "$DEBCACHE" && retry apt-get download -q \
        -o APT::Sandbox::User=root \
        --no-install-recommends "$p" ) \
      || warn "could not download $p"
  done
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
# 3. Verification
# =============================================================================
verify() {
  log "verifying toolchain"
  local fail=0
  export PATH="$CARGO_HOME/bin:$PREFIX/usr/lib/llvm-14/bin:$PREFIX/usr/bin:$PATH"
  export LD_LIBRARY_PATH="$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

  local check
  for check in "cargo --version" "rustc --version" "rustfmt --version" \
               "cargo-clippy --version" "qemu-system-x86_64 --version" \
               "nasm -v" "clang --version" "clangd --version" "ld.lld --version" \
               "gdb --version" "xorriso --version" "mformat --version"; do
    if out="$(eval "$check" 2>&1 | head -n1)"; then
      log "  ok   $check  ->  $out"
    else
      warn "  FAIL $check"
      fail=1
    fi
  done
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
verify

log "done.  Activate with:  source $ROOT/scripts/env.sh"
