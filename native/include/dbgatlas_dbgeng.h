#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
#define DA_DBGENG_EXTERN_C extern "C"
#else
#define DA_DBGENG_EXTERN_C
#endif

#ifdef _WIN32
#ifdef DBGATLAS_NATIVE_BUILD
#define DA_DBGENG_EXPORT DA_DBGENG_EXTERN_C __declspec(dllexport)
#else
#define DA_DBGENG_EXPORT DA_DBGENG_EXTERN_C __declspec(dllimport)
#endif
#else
#define DA_DBGENG_EXPORT DA_DBGENG_EXTERN_C __attribute__((visibility("default")))
#endif

typedef enum DA_DbgEngStatus {
    DA_DBGENG_OK = 0,
    DA_DBGENG_ERR_INVALID_ARGUMENT = 1,
    DA_DBGENG_ERR_BUFFER_TOO_SMALL = 2,
    DA_DBGENG_ERR_INTERNAL = 500
} DA_DbgEngStatus;

typedef struct DA_DbgEngVersion {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t abi_patch;
} DA_DbgEngVersion;

typedef struct DA_DbgEngTextView {
    uint32_t struct_size;
    uint32_t flags;
    const char* data;
    size_t len;
    void* owner;
} DA_DbgEngTextView;

typedef struct DA_DbgEngSessionHandle DA_DbgEngSessionHandle;

DA_DBGENG_EXPORT int32_t da_dbgeng_abi_version(DA_DbgEngVersion* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_load_runtime(const char* dbgeng_dir_utf8);
DA_DBGENG_EXPORT void da_dbgeng_release_view(void* owner);
DA_DBGENG_EXPORT int32_t da_dbgeng_last_error(
    char* buffer,
    size_t buffer_len,
    size_t* required_len);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_open_file(
    const char* path_utf8,
    DA_DbgEngSessionHandle** out_handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_attach_process(
    uint32_t pid,
    DA_DbgEngSessionHandle** out_handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_execute(
    DA_DbgEngSessionHandle* handle,
    const char* command_utf8,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_add_symbols(
    DA_DbgEngSessionHandle* handle,
    const char* symbol_path_utf8,
    int32_t reload,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_read_virtual(
    DA_DbgEngSessionHandle* handle,
    uint64_t address,
    uint32_t length,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_close(DA_DbgEngSessionHandle* handle);
