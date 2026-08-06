#pragma once

// DbgAtlas uses the vendored IDA SDK headers for compile-time type/layout
// declarations, while all IDA/Hex-Rays calls are still resolved dynamically
// from the user's IDA installation at runtime.

#include <string>

void* (*dbgatlas_ida_hexrays_dispatcher())(int code, ...);
#define HEXDSP dbgatlas_ida_hexrays_dispatcher()

#include <auto.hpp>
#include <bytes.hpp>
#include <expr.hpp>
#include <frame.hpp>
#include <funcs.hpp>
#include <hexrays.hpp>
#include <idalib.hpp>
#include <kernwin.hpp>
#include <lines.hpp>
#include <loader.hpp>
#include <name.hpp>
#include <nalt.hpp>
#include <segment.hpp>
#include <strlist.hpp>
#include <typeinf.hpp>
#include <xref.hpp>

#ifndef DBGATLAS_IDA_EXPECTED_SDK_VERSION
#error DBGATLAS_IDA_EXPECTED_SDK_VERSION must be defined by the provider target
#endif

#ifndef DBGATLAS_IDA_RUNTIME_MAJOR
#error DBGATLAS_IDA_RUNTIME_MAJOR must be defined by the provider target
#endif

#ifndef DBGATLAS_IDA_RUNTIME_MINOR
#error DBGATLAS_IDA_RUNTIME_MINOR must be defined by the provider target
#endif

static_assert(
    IDA_SDK_VERSION == DBGATLAS_IDA_EXPECTED_SDK_VERSION,
    "DbgAtlas IDA provider was compiled with the wrong SDK headers");
static_assert(sizeof(ea_t) == 8, "DbgAtlas IDA adapter only supports 64-bit ea_t");
static_assert(sizeof(qvector<char>) == 24, "qvector ABI layout must match IDA x64");
static_assert(sizeof(qstring) == 24, "qstring ABI layout must match IDA x64");
static_assert(sizeof(cfuncptr_t) == sizeof(void*), "cfuncptr_t must stay pointer-sized");

void dbgatlas_ida_runtime_load(const std::wstring& install_dir);
void dbgatlas_validate_ida_runtime_version();
bool dbgatlas_init_hexrays_plugin(int flags = 0);
void dbgatlas_term_hexrays_plugin();
asize_t dbgatlas_get_item_size(ea_t ea);
ea_t dbgatlas_get_imagebase();
