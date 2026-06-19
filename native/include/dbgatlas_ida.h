#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
#define DA_IDA_EXTERN_C extern "C"
#else
#define DA_IDA_EXTERN_C
#endif

#ifdef _WIN32
#ifdef DBGATLAS_NATIVE_BUILD
#define DA_IDA_EXPORT DA_IDA_EXTERN_C __declspec(dllexport)
#else
#define DA_IDA_EXPORT DA_IDA_EXTERN_C __declspec(dllimport)
#endif
#else
#define DA_IDA_EXPORT DA_IDA_EXTERN_C __attribute__((visibility("default")))
#endif

typedef enum DA_IdaStatus {
    DA_IDA_OK = 0,
    DA_IDA_ERR_INVALID_ARGUMENT = 1,
    DA_IDA_ERR_BUFFER_TOO_SMALL = 2,
    DA_IDA_ERR_NOT_FOUND = 3,
    DA_IDA_ERR_DYNAMIC_LOAD = 4,
    DA_IDA_ERR_IDA = 5,
    DA_IDA_ERR_INTERNAL = 500
} DA_IdaStatus;

typedef struct DA_IdaVersion {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t abi_patch;
    uint32_t ida_major;
    uint32_t ida_minor;
    uint32_t ida_build;
} DA_IdaVersion;

typedef struct DA_IdaTextView {
    uint32_t struct_size;
    uint32_t flags;
    const char* data;
    size_t len;
    void* owner;
} DA_IdaTextView;

typedef struct DA_IdaFunctionLookup {
    uint32_t struct_size;
    uint32_t flags;
    uint64_t runtime_address;
    uint64_t runtime_module_base;
    uint64_t rva;
    uint64_t ida_image_base;
    uint64_t ida_ea;
    uint64_t function_start;
    uint64_t function_end;
    int32_t found;
    DA_IdaTextView function_name;
} DA_IdaFunctionLookup;

typedef struct DA_IdaCoreResult {
    uint32_t struct_size;
    uint32_t flags;
    DA_IdaTextView result_json;
} DA_IdaCoreResult;

typedef struct DA_IdaSessionHandle DA_IdaSessionHandle;

DA_IDA_EXPORT int32_t da_ida_abi_version(DA_IdaVersion* out);
DA_IDA_EXPORT void da_ida_release_view(void* owner);
DA_IDA_EXPORT int32_t da_ida_last_error(
    char* buffer,
    size_t buffer_len,
    size_t* required_len);
DA_IDA_EXPORT int32_t da_ida_session_open(
    const char* install_dir_utf8,
    const char* database_path_utf8,
    DA_IdaSessionHandle** out_handle);
DA_IDA_EXPORT int32_t da_ida_lookup_function(
    DA_IdaSessionHandle* handle,
    uint64_t runtime_address,
    uint64_t runtime_module_base,
    uint64_t ida_image_base,
    DA_IdaFunctionLookup* out);
DA_IDA_EXPORT int32_t da_ida_core_function(
    DA_IdaSessionHandle* handle,
    const char* function_utf8,
    const char* arguments_json_utf8,
    DA_IdaCoreResult* out);
DA_IDA_EXPORT int32_t da_ida_session_close(DA_IdaSessionHandle* handle);
