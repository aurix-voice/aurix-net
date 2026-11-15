#!/usr/bin/env bash
# Build libaurix_net (crates/net-ffi) and stage it with the C headers into the Unreal plugin's
# ThirdParty module. Run from anywhere inside the aurix-net repository.
#
#   sdk/unreal/scripts/build_native.sh                      # host platform, release
#   sdk/unreal/scripts/build_native.sh --target x86_64-pc-windows-msvc
#   PLUGIN_DIR=/path/to/MyGame/Plugins/AurixNet sdk/unreal/scripts/build_native.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PLUGIN_DIR="${PLUGIN_DIR:-$ROOT/sdk/unreal/AurixNet}"
THIRD_PARTY="$PLUGIN_DIR/Source/ThirdParty/AurixNetLibrary"
PROFILE="release"
TARGET=""
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --debug) PROFILE="debug"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$TARGET" ] || TARGET="$(rustc -vV | sed -n 's/^host: //p')"
LINK_ARGS=""
NDK_ABI=""
case "$TARGET" in
  *-unknown-linux-*) UE_PLATFORM="Linux"; ARTIFACTS=("libaurix_net.so"); LINK_ARGS="-C link-arg=-Wl,-soname,libaurix_net.so" ;;
  *-pc-windows-msvc) UE_PLATFORM="Win64"; ARTIFACTS=("aurix_net.dll" "aurix_net.dll.lib") ;;
  *-apple-darwin)    UE_PLATFORM="Mac";   ARTIFACTS=("libaurix_net.dylib"); LINK_ARGS="-C link-arg=-Wl,-install_name,@rpath/libaurix_net.dylib" ;;
  aarch64-linux-android|armv7-linux-androideabi|x86_64-linux-android)
    UE_PLATFORM="Android"; ARTIFACTS=("libaurix_net.so")
    case "$TARGET" in aarch64-*) NDK_ABI="arm64-v8a" ;; armv7-*) NDK_ABI="armeabi-v7a" ;; *) NDK_ABI="x86_64" ;; esac ;;
  *) echo "unsupported target for the Unreal plugin: $TARGET" >&2; exit 2 ;;
esac
PROFILE_FLAG=(); [ "$PROFILE" = "release" ] && PROFILE_FLAG=(--release)
case "$TARGET" in
  *-linux-android*)
    command -v cargo-ndk >/dev/null || { echo "cargo-ndk is required (cargo install cargo-ndk)" >&2; exit 2; }
    ( cd "$ROOT" && cargo ndk -t "$NDK_ABI" -o /dev/null build -p net-ffi --lib "${PROFILE_FLAG[@]}" --target "$TARGET" ) ;;
  *)
    ( cd "$ROOT" && RUSTFLAGS="${RUSTFLAGS:-} $LINK_ARGS" cargo build -p net-ffi --lib "${PROFILE_FLAG[@]}" --target "$TARGET" ) ;;
esac
OUT="$ROOT/target/$TARGET/$PROFILE"
DEST="$THIRD_PARTY/lib/$UE_PLATFORM"; [ -n "$NDK_ABI" ] && DEST="$DEST/$NDK_ABI"
mkdir -p "$DEST" "$THIRD_PARTY/include"
for f in "${ARTIFACTS[@]}"; do cp -f "$OUT/$f" "$DEST/$f"; done
cp -f "$ROOT/crates/net-ffi/include/aurix_net.h" "$ROOT/crates/net-ffi/include/aurix_sim.h" "$THIRD_PARTY/include/"
echo "staged ${ARTIFACTS[*]} for $UE_PLATFORM into $DEST"
