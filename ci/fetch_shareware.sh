#!/usr/bin/env bash
# Fetch id's shareware Quake 1.06 and put its pak where the tests, the oracle
# and the deploy recipes look for it: quake-data/ID1/PAK0.PAK, with the
# shareware licence beside it (quake-data/SLICNSE.TXT, which a public demo
# serves with the pak: LICENSE's notice, README "License").
#
#   ci/fetch_shareware.sh            # into quake-data/ at the repository's root
#   ci/fetch_shareware.sh DIR        # into DIR/
#
# The same steps as README's "Build and run it", step 1, with the hashes
# checked: quake106.zip as id released it, and the pak inside it. Needs curl,
# unzip and bsdtar (Debian's libarchive-tools: the zip holds an LZH archive).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST=${1:-$ROOT/quake-data}
URL=https://raw.githubusercontent.com/Jason2Brownlee/QuakeOfficialArchive/main/bin/quake106.zip
ZIP_SHA256=ec6c9d34b1ae0252ac0066045b6611a7919c2a0d78a3a66d9387a8f597553239
PAK_SHA256=35a9c55e5e5a284a159ad2a62e0e8def23d829561fe2f54eb402dbc0a9a946af

if [ -f "$DEST/ID1/PAK0.PAK" ] && echo "$PAK_SHA256  $DEST/ID1/PAK0.PAK" | sha256sum -c --quiet 2>/dev/null; then
    echo "$DEST/ID1/PAK0.PAK: already there"
    exit 0
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/quake106.XXXXXX")
trap 'rm -rf -- "${WORK:?}"' EXIT
curl -sSfL --retry 3 -o "$WORK/quake106.zip" "$URL"
echo "$ZIP_SHA256  $WORK/quake106.zip" | sha256sum -c --quiet
unzip -q -o "$WORK/quake106.zip" resource.1 -d "$WORK"
(cd "$WORK" && bsdtar -xf resource.1 ID1/PAK0.PAK SLICNSE.TXT)
echo "$PAK_SHA256  $WORK/ID1/PAK0.PAK" | sha256sum -c --quiet
mkdir -p "$DEST/ID1"
mv "$WORK/ID1/PAK0.PAK" "$DEST/ID1/PAK0.PAK"
mv "$WORK/SLICNSE.TXT" "$DEST/SLICNSE.TXT"
echo "$DEST/ID1/PAK0.PAK: id's shareware pak, sha256 ${PAK_SHA256:0:8}"
