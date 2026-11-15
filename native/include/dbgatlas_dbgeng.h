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
    DA_DBGENG_ERR_STALE_ACTIVITY = 3,
    DA_DBGENG_ERR_BUSY = 4,
    DA_DBGENG_ERR_INTERNAL = 500,
    DA_DBGENG_ERR_OUTPUT_AFTER_SUCCESS = 501,
    DA_DBGENG_ERR_OUTPUT_AFTER_FAILURE = 502
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

typedef struct DA_DbgEngArtifactResult {
    uint32_t struct_size;
    uint32_t flags;
    DA_DbgEngTextView preview;
    uint64_t full_byte_len;
    uint64_t inline_byte_limit;
    int32_t truncated;
} DA_DbgEngArtifactResult;

typedef struct DA_DbgEngSessionHandle DA_DbgEngSessionHandle;
typedef struct DA_DbgEngControlHandle DA_DbgEngControlHandle;

/* Only control handles may cross threads. Session calls remain owner-thread only.
   An activity epoch is never reused. Interrupt acceptance is not completion. */
typedef struct DA_DbgEngEvent {
    uint64_t sequence;
    uint64_t epoch;
    uint64_t argument;
    uint32_t kind; /* 1 breakpoint, 2 exception, 3 exit, 4 engine, 5 input, 6 gap */
    uint32_t code;
} DA_DbgEngEvent;

typedef struct DA_DbgEngEvents {
    uint32_t struct_size;
    uint32_t output_error; /* Win32 error writing asynchronous output */
    const DA_DbgEngEvent* data;
    size_t len;
    void* owner;
    uint64_t output_byte_len;
} DA_DbgEngEvents;

/* Configure once, after opening, before publishing the control handle. */
DA_DBGENG_EXPORT int32_t da_dbgeng_session_enable_interaction(
    DA_DbgEngSessionHandle* handle, const char* output_path_utf8,
    DA_DbgEngControlHandle** out_control);
DA_DBGENG_EXPORT int32_t da_dbgeng_control_release(DA_DbgEngControlHandle* handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_control_interrupt(
    DA_DbgEngControlHandle* handle, uint64_t expected_epoch,
    uint32_t kind, int32_t* native_hresult); /* 0 ACTIVE, 1 PASSIVE, 2 EXIT */
/* Drains cached events only; never calls the engine. One consumer per handle. */
DA_DBGENG_EXPORT int32_t da_dbgeng_control_read_events(
    DA_DbgEngControlHandle* handle, DA_DbgEngEvents* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_begin_activity(
    DA_DbgEngSessionHandle* handle, uint32_t kind, uint64_t* epoch); /* 1 command, 2 event wait */
DA_DBGENG_EXPORT int32_t da_dbgeng_session_end_activity(
    DA_DbgEngSessionHandle* handle, uint64_t epoch, int32_t* native_hresult);
/* Always waits indefinitely. API wait budgets must never reach this call.
   native_hresult preserves S_OK, E_PENDING, E_UNEXPECTED and other engine outcomes. */
DA_DBGENG_EXPORT int32_t da_dbgeng_session_wait_event(
    DA_DbgEngSessionHandle* handle, uint64_t epoch, int32_t* native_hresult);

typedef struct DA_DbgEngState {
    uint32_t struct_size;
    uint32_t target_state; /* 0 unknown, 1 running, 2 stopped, 3 no debuggee */
    uint32_t process_id;
    uint32_t thread_id; /* debugger thread index; UINT32_MAX when unknown */
    uint32_t frame_index; /* UINT32_MAX when unknown */
} DA_DbgEngState;
DA_DBGENG_EXPORT int32_t da_dbgeng_session_state(DA_DbgEngSessionHandle* handle, DA_DbgEngState* out);
/* On failure the handle remains valid and may be retried or explicitly disposed. */
DA_DBGENG_EXPORT int32_t da_dbgeng_session_commit_launch(DA_DbgEngSessionHandle* handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_detach(DA_DbgEngSessionHandle* handle);

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
/* invasive: 0 preserves non-invasive/no-suspend behavior; 1 takes debug control. */
DA_DBGENG_EXPORT int32_t da_dbgeng_session_attach_process_mode(
    uint32_t pid, uint32_t invasive, DA_DbgEngSessionHandle** out_handle);

DA_DBGENG_EXPORT int32_t da_dbgeng_session_attach_process(
    uint32_t pid,
    DA_DbgEngSessionHandle** out_handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_launch_process(
    const char* command_line_utf8,
    DA_DbgEngSessionHandle** out_handle);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_execute(
    DA_DbgEngSessionHandle* handle,
    const char* command_utf8,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_execute_to_file(
    DA_DbgEngSessionHandle* handle,
    const char* command_utf8,
    const char* output_path_utf8,
    uint64_t inline_byte_limit,
    DA_DbgEngArtifactResult* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_add_symbols(
    DA_DbgEngSessionHandle* handle,
    const char* symbol_path_utf8,
    int32_t reload,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_add_symbols_to_file(
    DA_DbgEngSessionHandle* handle,
    const char* symbol_path_utf8,
    int32_t reload,
    const char* output_path_utf8,
    uint64_t inline_byte_limit,
    DA_DbgEngArtifactResult* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_read_virtual(
    DA_DbgEngSessionHandle* handle,
    uint64_t address,
    uint32_t length,
    DA_DbgEngTextView* out);
DA_DBGENG_EXPORT int32_t da_dbgeng_session_close(DA_DbgEngSessionHandle* handle);
