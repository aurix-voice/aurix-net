# Aurix Net — Unreal Engine plugin

Unreal NetDriver over Aurix relay rooms plus a Blueprint subsystem for rooms, grants and events.
See [Docs/QuickStart.md](Docs/QuickStart.md). Written for engine 5.3-5.8; Win64, Linux, Mac,
Android.

Status: beta. Built and run with one engine so far: UE 5.8.2 on Linux. `RunUAT BuildPlugin` passes
there (editor, game Development and Shipping), the packaged plugin loads in the editor, and a
headless listen server with two clients went through login, replication and host migration against
a 0.1.0 node. Other engine versions (5.3-5.7) and Win64, Mac, Android and iOS have not been built,
and the Blueprint nodes and the session interface have not been exercised. CI runs the engine-free
checks only (native staging, `sdk/unreal/scripts/check_plugin.py`). See
[Docs/Packaging.md](Docs/Packaging.md).
