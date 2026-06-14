# AGENTS.md

Native code is project-owned C++ and uses C++20.

- Export only C ABI functions from DLL boundaries.
- Do not expose C++ classes, `std::string`, `std::vector`, exceptions, templates, or COM objects through ABI.
- Keep RAII and C++ helper types internal to the DLL.
- Every exported function must catch exceptions and return an explicit status code.
- Use fixed-width integer types, UTF-8 strings at the boundary, and view + owner for returned buffers.
- Memory allocated by native code must be released by native release functions.
