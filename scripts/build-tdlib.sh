#!/bin/sh
# Builds TDLib (the version vendor/tdlib-rs/tl/api.tl describes) into
# third_party/tdlib, where .cargo/config.toml points the build.
# Needs: git, cmake, clang (or g++), gperf, openssl and zlib headers.
set -eu
cd "$(dirname "$0")/.."
COMMIT=42e6a5259551178d1dab54a22ad96d14bd906e20 # TDLib 1.8.67
if [ ! -d third_party/td ]; then
    git clone https://github.com/tdlib/td.git third_party/td
fi
if [ "$(git -C third_party/td rev-parse HEAD)" != "$COMMIT" ]; then
    if ! git -C third_party/td diff --quiet || ! git -C third_party/td diff --cached --quiet; then
        echo "Refusing to switch TDLib revisions with local source changes" >&2
        exit 1
    fi
    git -C third_party/td fetch --depth 1 origin "$COMMIT"
    git -C third_party/td checkout -q "$COMMIT"
fi
for PATCH in "$PWD/scripts/security/tdlib-google-dns-https.patch" \
             "$PWD/scripts/security/tdlib-credentialed-proxy-dns.patch" \
             "$PWD/scripts/security/tdlib-google-dns-answer.patch"; do
    # The answer patch moves the HTTPS patch's hunks; skip its check once both are applied.
    if [ "$PATCH" = "$PWD/scripts/security/tdlib-google-dns-https.patch" ] &&
       git -C third_party/td apply --reverse --check "$PWD/scripts/security/tdlib-google-dns-answer.patch"; then
        continue
    fi
    if git -C third_party/td apply --reverse --check "$PATCH"; then
        : # Already patched; preserve local source changes.
    elif git -C third_party/td apply --check "$PATCH"; then
        git -C third_party/td apply "$PATCH"
    else
        echo "TDLib source does not match pinned security patch: $PATCH" >&2
        exit 1
    fi
done
mkdir -p third_party/td/build
cd third_party/td/build
CC=${CC:-clang} CXX=${CXX:-clang++} cmake -DCMAKE_BUILD_TYPE=Release ..
# Parallel jobs take ~2 GB of memory each.
cmake --build . --target tdjson -j"${JOBS:-4}"
cd ../../..
mkdir -p third_party/tdlib/lib third_party/tdlib/include
cp third_party/td/build/libtdjson.so.1.8.67 third_party/tdlib/lib/
strip --strip-unneeded third_party/tdlib/lib/libtdjson.so.1.8.67
ln -sf libtdjson.so.1.8.67 third_party/tdlib/lib/libtdjson.so
cp third_party/td/td/telegram/td_json_client.h third_party/td/build/td/telegram/tdjson_export.h \
    third_party/tdlib/include/
echo "TDLib is in third_party/tdlib"
