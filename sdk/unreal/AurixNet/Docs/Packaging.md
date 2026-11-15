# Packaging

1. Stage natives: `sdk/unreal/scripts/build_native.sh` per platform (`--target x86_64-pc-windows-msvc`,
   `aarch64-apple-darwin`, `aarch64-linux-android`); headers are copied from `crates/net-ffi/include`.
2. Engine-free checks: `python3 sdk/unreal/scripts/check_plugin.py` (descriptor, modules, UHT conventions,
   dependency direction, staged natives).
3. Per engine version (5.3 – 5.8): `RunUAT.sh BuildPlugin -Plugin=<repo>/sdk/unreal/AurixNet/AurixNet.uplugin
   -Package=<out>/AurixNet-5.x -TargetPlatforms=Win64+Linux+Mac+Android`; set `EngineVersion` in the packaged
   descriptor. Staged `include/` and `lib/` are built per host and are not committed.
