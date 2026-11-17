# Aurix Net — Unreal Engine plugin

Unreal NetDriver over Aurix relay rooms plus a Blueprint subsystem for rooms, grants and events.
See [Docs/QuickStart.md](Docs/QuickStart.md). Written for engine 5.3-5.8; Win64, Linux, Mac,
Android.

Status: beta. The plugin has not been built with an engine yet: CI runs the engine-free checks
only (native staging, `sdk/unreal/scripts/check_plugin.py`), so 5.3-5.8 is the range the source
targets, not a tested one. It stays beta until `RunUAT BuildPlugin` passes
([Docs/Packaging.md](Docs/Packaging.md)).
