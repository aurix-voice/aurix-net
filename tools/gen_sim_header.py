#!/usr/bin/env python3
"""Generate the aurix-sim C header from the `#[no_mangle] pub extern "C"` functions of
crates/net-sim-wasm (lib.rs + sim3.rs) into crates/net-ffi/include/aurix_sim.h and copy it to
sdk/cpp and sdk/godot/src. Run from the repository root after changing the sim ABI."""
import re
import shutil
from pathlib import Path

TYPES = {
    "u32": "uint32_t", "i32": "int32_t", "u64": "uint64_t", "i64": "int64_t", "u8": "uint8_t",
    "usize": "size_t", "bool": "bool", "()": "void",
    "*const u8": "const uint8_t*", "*mut u8": "uint8_t*", "*const i64": "const int64_t*",
    "*mut i64": "int64_t*", "*const u32": "const uint32_t*", "*mut u32": "uint32_t*",
    "*const c_char": "const char*",
}
SOURCES = ["crates/net-sim-wasm/src/lib.rs", "crates/net-sim-wasm/src/sim3.rs"]
OUT = Path("crates/net-ffi/include/aurix_sim.h")
COPIES = [Path("sdk/cpp/aurix_sim.h"), Path("sdk/godot/src/aurix_sim.h")]
FN = re.compile(
    r'((?:[ \t]*///[^\n]*\n)*)[ \t]*#\[no_mangle\]\s*(?:#\[[^\]]*\]\s*)*pub (?:unsafe )?extern "C" fn (\w+)\s*\(([^)]*)\)\s*(?:->\s*([^{\s]+))?\s*\{'
)
HEAD = '''// aurix-sim C ABI: deterministic fixed-point (Q32.32, raw int64 values) physics 2D/3D, navmesh,
// flow fields, crowd and reference golden hashes. Exported by libaurix_net (crates/net-ffi) and by
// libnet_sim_wasm / net_sim_wasm.wasm (crates/net-sim-wasm). Generated from the Rust sources by
// tools/gen_sim_header.py - edit there. World handles are 1-based (0 = failure); body/joint/agent
// ids are returned as 1-based too where the Rust doc says so; positions and velocities are raw Fx
// (value * 2^32).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AURIX_FX_ONE ((int64_t)1 << 32)
static inline int64_t aurix_fx_from_double(double v) { return (int64_t)(v * 4294967296.0); }
static inline double aurix_fx_to_double(int64_t v) { return (double)v / 4294967296.0; }

'''
TAIL = '''

#ifdef __cplusplus
}
#endif
'''


def main() -> None:
    decls = []
    for src in SOURCES:
        text = Path(src).read_text()
        for doc, name, params, ret in FN.findall(text):
            doc = " ".join(l.strip().lstrip("/").strip() for l in doc.strip().splitlines() if l.strip())
            args = []
            for p in (x.strip() for x in params.split(",") if x.strip()):
                n, t = (x.strip() for x in p.split(":", 1))
                args.append(f"{TYPES[t]} {n}")
            r = TYPES[ret.strip()] if ret else "void"
            line = f'{r} {name}({", ".join(args) if args else "void"});'
            decls.append((f"/* {doc} */\n" if doc else "") + line)
    OUT.write_text(HEAD + "\n\n".join(decls) + TAIL)
    for c in COPIES:
        shutil.copyfile(OUT, c)
    print(f"{len(decls)} functions -> {OUT}")


if __name__ == "__main__":
    main()
