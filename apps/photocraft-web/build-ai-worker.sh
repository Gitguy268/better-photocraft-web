#!/bin/sh
# Trunk post_build hook of apps/photocraft-web: build the AI worker (apps/photocraft-ml-worker, a
# separate Trunk project so its SIMD build flags apply to it alone) and put its generated loader,
# glue and wasm into the site.
#
# The files go into a folder named after the wasm's checksum, so hosts can cache them forever like
# the app's own hashed files, and ai-worker.txt (not cached) tells the app the folder's name.
set -eu
: "${TRUNK_STAGING_DIR:?run by trunk build (apps/photocraft-web/Trunk.toml)}"
STAGING="$TRUNK_STAGING_DIR"
HERE="${TRUNK_SOURCE_DIR:-$(cd "$(dirname "$0")" && pwd)}"
WORKER="$HERE/../photocraft-ml-worker"
OUT="$HERE/../../dist/ml-worker"

# The nested trunk must not inherit this build's settings.
(cd "$WORKER" && env -u TRUNK_STAGING_DIR -u TRUNK_DIST_DIR -u TRUNK_SOURCE_DIR -u TRUNK_HTML_FILE -u TRUNK_PUBLIC_URL -u TRUNK_PROFILE trunk build --release)

SUM=$(cksum <"$OUT/photocraft-ml-worker_bg.wasm" | cut -d' ' -f1)
DIR="ai-worker-$SUM"
mkdir -p "$STAGING/$DIR"
cp "$OUT/photocraft-ml-worker_bg.wasm" "$OUT/photocraft-ml-worker.js" "$OUT/photocraft-ml-worker_loader.js" "$STAGING/$DIR/"
printf '%s\n' "$DIR" >"$STAGING/ai-worker.txt"
echo "AI worker: $DIR ($(wc -c <"$OUT/photocraft-ml-worker_bg.wasm" | tr -d ' ') bytes)"
