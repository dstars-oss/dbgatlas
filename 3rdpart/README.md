# Third-party Dependencies

This directory is reserved for third-party C++ dependencies used by native adapters.

Vendored dependencies:

- `cpp/ida-sdk/include`: IDA SDK header snapshot from `dstars-oss/ida-sdk`
  tag `v9.3.1-release`, commit `acacbbcc8fa349d919cc185d88b1ab3710ca252d`.
  License: MIT, copied to `licenses/ida-sdk-MIT.txt`.
  DbgAtlas only vendors `src/include` because the native IDA adapter compiles
  against SDK declarations but still dynamically loads the user's local
  `ida.dll` / `idalib.dll` / Hex-Rays dispatcher at runtime. The SDK `src/lib`,
  samples, docs, build system, and nested `src/cmake` submodule are intentionally
  not vendored.

To refresh the IDA SDK header snapshot:

1. Clone or fetch `https://github.com/dstars-oss/ida-sdk.git`.
2. Check out the intended tag or commit.
3. Replace `cpp/ida-sdk/include` with that checkout's `src/include`.
4. Copy `LICENSE` to `licenses/ida-sdk-MIT.txt`.
5. Update this record with the exact tag, commit, and rationale.

Expected layout:

- `cpp/`: vendored source.
- `prebuilt/`: prebuilt libraries or SDK-adjacent binaries when vendoring source is not practical.
- `patches/`: local patches with rationale.
- `licenses/`: dependency licenses.
- `cmake/`: CMake glue for third-party integration.
