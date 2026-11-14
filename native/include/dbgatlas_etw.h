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
    DA_ETW_CAP_EVENT_STACK_TRACE = 1u << 3,
    DA_ETW_CAP_CPU_SAMPLING = 1u << 4
} DA_EtwCapabilityFlags;

typedef enum DA_EtwPresetFlags {
    DA_ETW_PRESET_PROCESS = 1u << 0,
    DA_ETW_PRESET_THREAD = 1u << 1,
    DA_ETW_PRESET_IMAGE = 1u << 2,
    DA_ETW_PRESET_FILE = 1u << 3,
    DA_ETW_PRESET_REGISTRY = 1u << 4,
    DA_ETW_PRESET_NETWORK = 1u << 5,
    DA_ETW_PRESET_CPU = 1u << 6
} DA_EtwPresetFlags;

typedef struct DA_EtwSessionOptions {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t cpu_sampling_interval_us;
    uint32_t maximum_trace_size_mb;
} DA_EtwSessionOptions;

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
    uint32_t stack_frames_total;
    uint32_t stack_frames_resolved;
    uint32_t stack_frames_unresolved;
    uint32_t file_path_resolved;
    uint32_t file_path_unresolved;
    /* Extraction quality signals, not hard errors. Rust/service records them
       in recording metadata so traces can be reviewed or replayed if needed. */
    uint32_t matched_op_end;
    uint32_t unmatched_op_end;
    uint32_t incomplete_io;
    uint32_t reused_irp;
    uint32_t dropped_stack_walk;
    uint32_t cpu_samples_written;
    uint32_t cpu_samples_with_stack;
    uint32_t cpu_samples_leaf_only;
    uint32_t cpu_samples_out_of_scope;
    uint32_t cpu_samples_cache_flushed;
} DA_EtwEventExtractionResult;

typedef struct DA_EtwStackTraceStatus {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t requested;
    uint32_t enabled;
    /* Provider/kernel stack tracing is best-effort. When permissions, OS
       version, or provider support are insufficient, collection continues
       and warning_count exposes the downgrade. */
    uint32_t provider_stack_enabled;
    uint32_t provider_stack_warning_count;
    uint32_t kernel_stack_enabled;
    uint32_t kernel_stack_warning_count;
} DA_EtwStackTraceStatus;

typedef struct DA_EtwCpuSamplingStatus {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t requested;
    uint32_t enabled;
    uint32_t stack_enabled;
    uint32_t requested_interval_us;
    uint32_t applied_interval_us;
    uint32_t previous_interval_us;
    uint32_t maximum_trace_size_mb;
    uint32_t warning_count;
} DA_EtwCpuSamplingStatus;

typedef struct DA_EtwSessionStopResult {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t events_lost;
    uint32_t buffers_written;
    uint32_t log_buffers_lost;
    uint32_t realtime_buffers_lost;
    uint32_t interval_restore_attempted;
    uint32_t interval_restored;
    uint32_t interval_restore_skipped;
    uint32_t warning_count;
    uint32_t events_written;
    uint32_t skipped_events;
    uint32_t cpu_samples_written;
    uint32_t cpu_samples_with_stack;
    uint32_t cpu_samples_leaf_only;
    uint32_t cpu_samples_out_of_scope;
    uint32_t cpu_samples_cache_flushed;
} DA_EtwSessionStopResult;

typedef struct DA_EtwSessionHandle DA_EtwSessionHandle;

DA_ETW_EXPORT int32_t da_etw_abi_version(DA_EtwVersion* out);
DA_ETW_EXPORT int32_t da_etw_profile_interval(uint32_t* interval_us);
DA_ETW_EXPORT int32_t da_etw_recover_session(const char* session_name_utf8, const char* expected_trace_path_utf8,
    uint32_t previous_interval_us, uint32_t applied_interval_us);
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
DA_ETW_EXPORT int32_t da_etw_session_start_file_trace_ex(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags,
    const DA_EtwSessionOptions* options,
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
DA_ETW_EXPORT int32_t da_etw_session_cpu_sampling_status(
    DA_EtwSessionHandle* handle,
    DA_EtwCpuSamplingStatus* out);
DA_ETW_EXPORT int32_t da_etw_session_stop(DA_EtwSessionHandle* handle);
DA_ETW_EXPORT int32_t da_etw_session_stop_ex(
    DA_EtwSessionHandle* handle,
    DA_EtwSessionStopResult* out);
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
