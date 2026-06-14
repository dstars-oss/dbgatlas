# Third-party Dependencies

This directory is reserved for third-party C++ dependencies used by native adapters.

Initial MVP 0 does not vendor any third-party dependency.

Expected layout:

- `cpp/`: vendored source.
- `prebuilt/`: prebuilt libraries or SDK-adjacent binaries when vendoring source is not practical.
- `patches/`: local patches with rationale.
- `licenses/`: dependency licenses.
- `cmake/`: CMake glue for third-party integration.
