# Aurix Net — C++ (header-only over the C ABI)

`aurix_net.hpp` wraps `libaurix_net` (crate `net-ffi`): relay rooms, `aurix::net::Sync` (replicated world),
`aurix::net::Lockstep`, plus aurix-sim (`aurix_sim.h`). Build the library once from the repo root:

```bash
cargo build --release -p net-ffi        # target/release/libaurix_net.{so,dylib} / aurix_net.dll
```

Live checks against a local node (README quickstart; `NET_HTTP` defaults to `http://127.0.0.1:8090`):

```bash
export NET_API_KEY=an_...
for t in echo_test sync_test; do
  g++ -std=c++17 -O1 sdk/cpp/$t.cpp -Isdk/cpp -Ltarget/release -laurix_net -lpthread \
      -Wl,-rpath,$PWD/target/release -o /tmp/$t && /tmp/$t
done
# plain C: crates/net-ffi/ctest/echo.c (relay) and ctest/sim.c (offline, six golden hashes)
gcc -std=c11 -Wall -Wextra -Werror crates/net-ffi/ctest/sim.c -Ltarget/release -laurix_net -lm \
    -Wl,-rpath,$PWD/target/release -o /tmp/sim_ctest && /tmp/sim_ctest
```

API walk-through: [docs/sdk.md](../../docs/sdk.md) (sections "Lockstep rooms from Rust, C++ and Godot",
"Sync rooms from C++ and Godot", "aurix-sim from C / C++ / Godot").
