#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
#define DA_EXTERN_C extern "C"
#else
#define DA_EXTERN_C
#endif

#ifdef _WIN32
#ifdef DBGATLAS_NATIVE_BUILD
#define DA_EXPORT DA_EXTERN_C __declspec(dllexport)
#else
#define DA_EXPORT DA_EXTERN_C __declspec(dllimport)
#endif
#else
#define DA_EXPORT DA_EXTERN_C __attribute__((visibility("default")))
#endif

typedef enum DA_Status {
    DA_OK = 0,
    DA_ERR_INVALID_ARGUMENT = 1,
    DA_ERR_BUFFER_TOO_SMALL = 2,
    DA_ERR_INTERNAL = 500
} DA_Status;

typedef struct DA_Version {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t abi_patch;
} DA_Version;

typedef struct DA_TextView {
    uint32_t struct_size;
    uint32_t flags;
    const char* data;
    size_t len;
    void* owner;
} DA_TextView;

DA_EXPORT int32_t da_abi_version(DA_Version* out);
DA_EXPORT int32_t da_native_hello(const char* input_utf8, DA_TextView* out);
DA_EXPORT void da_release_view(void* owner);
DA_EXPORT int32_t da_last_error(char* buffer, size_t buffer_len, size_t* required_len);
