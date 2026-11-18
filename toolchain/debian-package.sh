#!/bin/sh
# A pinned Debian package's files: the package toolchain/pins names as
# NAME_URL and NAME_SHA256 (Debian's snapshot archive, or another archive's
# such as WineHQ's), fetched by digest and unpacked (its data, no install,
# no scripts) once into CACHE.
# The same files on any host, where a copy of the build machine's /usr was
# whatever its distribution installed, and nothing on a host that is not
# Linux.
#
# The packages live in GEX_PACKAGE_CACHE when it is set (page/build.sh keeps
# its caller's store there while its own CACHE is its work folder), else in
# CACHE. Some hold paths that differ only in case (linux-libc-dev's
# netfilter headers: xt_CONNMARK.h, a target, beside xt_connmark.h, a match),
# so the store must be on a case-sensitive file system: on another, one of
# each pair would overwrite the other, and unpacking such a package there is
# refused.
#
#   toolchain/debian-package.sh NAME       -> prints the unpacked root (its usr/ or opt/)
set -eu
HERE=$(dirname "$(realpath "$0")")
name=${1:?NAME}
eval "$(sed -n "s/^\(${name}_URL\|${name}_SHA256\) *:= *\(.*\)\$/\1=\2/p" "$HERE/pins")"
eval "url=\${${name}_URL:?no ${name}_URL in toolchain/pins} sum=\${${name}_SHA256:?no ${name}_SHA256 in toolchain/pins}"
CACHE=${GEX_PACKAGE_CACHE:-${CACHE:-$HOME/.cache/gex}}
dir=$CACHE/debian-$(basename "$url" .deb)-$(printf %.12s "$sum")
mkdir -p "$CACHE"
exec 8>"$dir.lock"
flock 8
if [ ! -f "$dir/.done" ]; then
  "$HERE/../wine/dl.sh" "$url" "$sum" "$CACHE/dl" >&2
  deb=$CACHE/dl/$(basename "$url")
  member=$(ar t "$deb" | grep '^data\.tar')
  case $member in
    *.xz) z=-J ;; *.zst) z=--zstd ;; *.gz) z=-z ;; *) z= ;;
  esac
  rm -rf "$dir.tmp" && mkdir -p "$dir.tmp"
  # A case-insensitive store would merge the package's case-only pairs.
  touch "$dir.tmp/.case-probe"
  if [ -e "$dir.tmp/.CASE-PROBE" ]; then
    pairs=$(ar p "$deb" "$member" | tar -t $z | sort -f | uniq -di)
    if [ -n "$pairs" ]; then
      echo "debian-package: $(basename "$url") has paths that differ only in case, and $CACHE is on a case-insensitive file system; set GEX_PACKAGE_CACHE (or CACHE) to a case-sensitive one:" >&2
      echo "$pairs" | head -5 >&2
      rm -rf "$dir.tmp"
      exit 1
    fi
  fi
  rm -f "$dir.tmp/.case-probe"
  ar p "$deb" "$member" | tar -x $z -C "$dir.tmp"
  touch "$dir.tmp/.done"
  rm -rf "$dir" && mv "$dir.tmp" "$dir"
fi
echo "$dir"
