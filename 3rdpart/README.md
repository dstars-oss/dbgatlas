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
- `cpp/nlohmann-json/include/nlohmann/json.hpp`: nlohmann/json single-header
  release `v3.12.0` from `https://github.com/nlohmann/json`.
  License: MIT, copied to `licenses/nlohmann-json-MIT.txt`.
  DbgAtlas only vendors the generated single header because native adapters only
  need in-process JSON parsing/serialization and do not need the upstream build
  system, tests, docs, package metadata, or multi-file include tree.

To refresh the IDA SDK header snapshot:

1. Clone or fetch `https://github.com/dstars-oss/ida-sdk.git`.
2. Check out the intended tag or commit.
3. Replace `cpp/ida-sdk/include` with that checkout's `src/include`.
4. Copy `LICENSE` to `licenses/ida-sdk-MIT.txt`.
5. Update this record with the exact tag, commit, and rationale.

To refresh nlohmann/json:

1. Download `single_include/nlohmann/json.hpp` from the intended upstream
   release tag.
2. Replace `cpp/nlohmann-json/include/nlohmann/json.hpp`.
3. Copy `LICENSE.MIT` to `licenses/nlohmann-json-MIT.txt`.
4. Update this record with the exact release tag and rationale.

Expected layout:

- `cpp/`: vendored source.
- `prebuilt/`: prebuilt libraries or SDK-adjacent binaries when vendoring source is not practical.
- `patches/`: local patches with rationale.
- `licenses/`: dependency licenses.
- `cmake/`: CMake glue for third-party integration.
