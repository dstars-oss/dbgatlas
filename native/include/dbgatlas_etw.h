#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
#define DA_ETW_EXTERN_C extern "C"
#else
#define DA_ETW_EXTERN_C
#endif

#ifdef _WIN32
#ifdef DBGATLAS_NATIVE_BUILD
#define DA_ETW_EXPORT DA_ETW_EXTERN_C __declspec(dllexport)
#else
#define DA_ETW_EXPORT DA_ETW_EXTERN_C __declspec(dllimport)
#endif
#else
#define DA_ETW_EXPORT DA_ETW_EXTERN_C __attribute__((visibility("default")))
#endif

typedef enum DA_EtwStatus {
    DA_ETW_OK = 0,
    DA_ETW_ERR_INVALID_ARGUMENT = 1,
    DA_ETW_ERR_NOT_IMPLEMENTED = 2,
    DA_ETW_ERR_BUFFER_TOO_SMALL = 3,
    DA_ETW_ERR_INTERNAL = 500
} DA_EtwStatus;

typedef enum DA_EtwCapabilityFlags {
    DA_ETW_CAP_REALTIME_CONSUME = 1u << 0,
    DA_ETW_CAP_FILE_TRACE = 1u << 1,
    DA_ETW_CAP_PROCESS_TREE_FILTER = 1u << 2,
    DA_ETW_CAP_EVENT_STACK_TRACE = 1u << 3
} DA_EtwCapabilityFlags;

typedef enum DA_EtwPresetFlags {
    DA_ETW_PRESET_PROCESS = 1u << 0,
    DA_ETW_PRESET_THREAD = 1u << 1,
    DA_ETW_PRESET_IMAGE = 1u << 2,
    DA_ETW_PRESET_FILE = 1u << 3,
    DA_ETW_PRESET_REGISTRY = 1u << 4,
    DA_ETW_PRESET_NETWORK = 1u << 5
} DA_EtwPresetFlags;

typedef struct DA_EtwVersion {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t abi_patch;
} DA_EtwVersion;

typedef struct DA_EtwAdapterInfo {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t capability_flags;
} DA_EtwAdapterInfo;

typedef struct DA_EtwEventExtractionResult {
    uint32_t struct_size;
    uint32_t events_written;
    uint32_t files_written;
    uint32_t skipped_events;
} DA_EtwEventExtractionResult;

typedef struct DA_EtwStackTraceStatus {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t requested;
    uint32_t enabled;
    uint32_t provider_stack_enabled;
    uint32_t provider_stack_warning_count;
    uint32_t kernel_stack_enabled;
    uint32_t kernel_stack_warning_count;
} DA_EtwStackTraceStatus;

typedef struct DA_EtwSessionHandle DA_EtwSessionHandle;

DA_ETW_EXPORT int32_t da_etw_abi_version(DA_EtwVersion* out);
DA_ETW_EXPORT int32_t da_etw_adapter_info(DA_EtwAdapterInfo* out);
DA_ETW_EXPORT int32_t da_etw_last_error(
    char* buffer,
    size_t buffer_len,
    size_t* required_len);
DA_ETW_EXPORT int32_t da_etw_write_minimal_file_trace(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags);
DA_ETW_EXPORT int32_t da_etw_session_start_file_trace(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags,
    DA_EtwSessionHandle** out_handle);
DA_ETW_EXPORT int32_t da_etw_session_start_realtime_consumer(
    DA_EtwSessionHandle* handle,
    const char* events_dir_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid);
DA_ETW_EXPORT int32_t da_etw_session_stack_trace_status(
    DA_EtwSessionHandle* handle,
    DA_EtwStackTraceStatus* out);
DA_ETW_EXPORT int32_t da_etw_session_stop(DA_EtwSessionHandle* handle);
DA_ETW_EXPORT int32_t da_etw_extract_file_events(
    const char* trace_path_utf8,
    const char* events_dir_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid,
    DA_EtwEventExtractionResult* out);
DA_ETW_EXPORT int32_t da_etw_filter_trace_file(
    const char* input_trace_path_utf8,
    const char* output_trace_path_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid,
    DA_EtwEventExtractionResult* out);
