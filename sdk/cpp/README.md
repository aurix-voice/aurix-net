# Aurix Net: C++ (header-only over the C ABI)

`aurix_net.hpp` wraps `libaurix_net` (crate `net-ffi`): relay rooms, `aurix::net::Sync` (replicated world),
`aurix::net::Lockstep`, plus aurix-sim (`aurix_sim.h`). Build the library once from the repo root:

```bash
cargo build --release -p net-ffi        # target/release/libaurix_net.{so,dylib} / aurix_net.dll
```

Live checks against a local node (README quickstart; `NET_HTTP` defaults to `http://127.0.0.1:8090`):

```bash
export NET_API_KEY=an_...
for t in echo_test sync_test lockstep_test; do
  g++ -std=c++17 -O1 sdk/cpp/$t.cpp -Isdk/cpp -Ltarget/release -laurix_net -lpthread \
      -Wl,-rpath,$PWD/target/release -o /tmp/$t && /tmp/$t
done
# plain C: crates/net-ffi/ctest/echo.c (relay) and ctest/sim.c (offline, six golden hashes)
gcc -std=c11 -Wall -Wextra -Werror crates/net-ffi/ctest/sim.c -Ltarget/release -laurix_net -lm \
    -Wl,-rpath,$PWD/target/release -o /tmp/sim_ctest && /tmp/sim_ctest
```

`echo_test` also leaves a room and joins another one over a slow link when `NET_SLOW_HTTP` points
at a delaying proxy in front of the node: `node sdk/web/test/slow-link.mjs 100 8197` (100 ms each
way), then `NET_SLOW_HTTP=http://127.0.0.1:8197`. The managed C# and Godot checks take the same
variable.

`rehost_test.cpp` and the plain C `ctest/rehost.c` need two nodes and are built and run by
`scripts/sdk-rehost-test.sh`: the node hands the room over, the client forgets it and joins it again
through the other node.

API walk-through: [docs/sdk.md](../../docs/sdk.md) (sections "Lockstep rooms from Rust, C++ and Godot",
"Sync rooms from C++ and Godot", "aurix-sim from C / C++ / Godot").
