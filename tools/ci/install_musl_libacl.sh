#!/usr/bin/env bash
# Install musl-native static libacl and libattr into a musl sysroot, from the
# same Alpine packages the release cross image installs.
#
# The package list is read from the release image's Dockerfile, so CI links
# the same libacl as the shipped musl binary. Every package must have a
# digest in DIGESTS (sha256sum format) and match it; a Dockerfile URL without
# a digest, or a download that does not match, fails the job. There is one
# download attempt per package: a CDN outage fails the job with curl's error.
# PREFIX is the sysroot the image uses (/usr/local/x86_64-linux-musl), which
# crates/metadata/build.rs adds to the link search path for musl targets.
#
# Usage: install_musl_libacl.sh DOCKERFILE DIGESTS PREFIX
set -euo pipefail

usage='usage: install_musl_libacl.sh DOCKERFILE DIGESTS PREFIX'
dockerfile=${1:?${usage}}
digests=${2:?${usage}}
prefix=${3:?${usage}}

mapfile -t urls < <(grep -oE 'https://dl-cdn\.alpinelinux\.org/[^ ]+\.apk' "${dockerfile}")
if [ "${#urls[@]}" -eq 0 ]; then
  printf '::error::no Alpine package URLs found in %s\n' "${dockerfile}" >&2
  exit 1
fi

work=$(mktemp -d)
mkdir -p "${work}/packages" "${work}/root" "${prefix}/lib" "${prefix}/include"
: > "${work}/expected.sha256"

for url in "${urls[@]}"; do
  name=${url##*/}
  entry=$(awk -v n="${name}" '$2 == n' "${digests}")
  if [ -z "${entry}" ]; then
    printf '::error::%s is listed in %s but has no digest in %s\n' \
      "${name}" "${dockerfile}" "${digests}" >&2
    exit 1
  fi
  printf '%s\n' "${entry}" >> "${work}/expected.sha256"
  curl --fail --silent --show-error --location --max-time 120 \
    --output "${work}/packages/${name}" "${url}"
done

if ! (cd "${work}/packages" && sha256sum --check --strict "${work}/expected.sha256"); then
  printf '::error::Alpine package digests do not match %s\n' "${digests}" >&2
  exit 1
fi

for apk in "${work}"/packages/*.apk; do
  # APK files are gzip-compressed tar streams whose signature members GNU tar
  # warns about; the payload extracts as a normal tree.
  tar -xzf "${apk}" -C "${work}/root" --warning=no-unknown-keyword
done

cp -R "${work}/root/usr/include/." "${prefix}/include/"
cp "${work}"/root/usr/lib/*.a "${prefix}/lib/"

for lib in libacl.a libattr.a; do
  if [ ! -f "${prefix}/lib/${lib}" ]; then
    printf '::error::%s missing after unpacking %s\n' "${lib}" "${dockerfile}" >&2
    exit 1
  fi
done
printf 'musl libacl and libattr installed under %s\n' "${prefix}"
