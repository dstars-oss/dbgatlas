#include "dbgatlas_dbgeng.h"

#ifdef _WIN32
#include <Windows.h>
#include <DbgEng.h>
#endif

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <exception>
#include <mutex>
#include <memory>
#include <new>
#include <string>
#include <vector>

namespace {

thread_local std::string g_last_error;

struct ViewOwner {
    virtual ~ViewOwner() = default;
};

struct BufferOwner final : ViewOwner {
    std::vector<char> bytes;
};

#ifdef _WIN32

using DebugCreateFn = HRESULT(WINAPI*)(REFIID, void**);

std::mutex g_runtime_mutex;
HMODULE g_dbgeng_module = nullptr;
DebugCreateFn g_debug_create = nullptr;
std::wstring g_dbgeng_loaded_path;

int32_t fail(DA_DbgEngStatus status, std::string message) noexcept;

template <typename T>
class ComPtr final {
public:
    ComPtr() = default;
    ~ComPtr() {
        reset();
    }

    ComPtr(const ComPtr&) = delete;
    ComPtr& operator=(const ComPtr&) = delete;

    ComPtr(ComPtr&& other) noexcept : ptr_(other.ptr_) {
        other.ptr_ = nullptr;
    }

    ComPtr& operator=(ComPtr&& other) noexcept {
        if (this != &other) {
            reset();
            ptr_ = other.ptr_;
            other.ptr_ = nullptr;
        }
        return *this;
    }

    T* get() const noexcept {
        return ptr_;
    }

    T** put() noexcept {
        reset();
        return &ptr_;
    }

    T* operator->() const noexcept {
        return ptr_;
    }

    explicit operator bool() const noexcept {
        return ptr_ != nullptr;
    }

    void reset() noexcept {
        if (ptr_ != nullptr) {
            ptr_->Release();
            ptr_ = nullptr;
        }
    }

private:
    T* ptr_ = nullptr;
};

class CapturingOutputCallbacks final : public IDebugOutputCallbacks {
public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        if (iid == __uuidof(IUnknown) || iid == __uuidof(IDebugOutputCallbacks)) {
            *object = static_cast<IDebugOutputCallbacks*>(this);
            AddRef();
            return S_OK;
        }
        *object = nullptr;
        return E_NOINTERFACE;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return InterlockedIncrement(&refs_);
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const auto refs = InterlockedDecrement(&refs_);
        return static_cast<ULONG>(refs);
    }

    HRESULT STDMETHODCALLTYPE Output(ULONG, PCSTR text) override {
        if (text != nullptr) {
            output_ += text;
        }
        return S_OK;
    }

    const std::string& output() const noexcept {
        return output_;
    }

    void clear() {
        output_.clear();
    }

private:
    volatile LONG refs_ = 1;
    std::string output_;
};

class BreakingEventCallbacks final : public DebugBaseEventCallbacks {
public:
    ULONG STDMETHODCALLTYPE AddRef() override {
        return InterlockedIncrement(&refs_);
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const auto refs = InterlockedDecrement(&refs_);
        return static_cast<ULONG>(refs);
    }

    HRESULT STDMETHODCALLTYPE GetInterestMask(PULONG mask) override {
        if (mask == nullptr) {
            return E_POINTER;
        }
        *mask = DEBUG_EVENT_CREATE_PROCESS | DEBUG_EVENT_EXCEPTION | DEBUG_EVENT_BREAKPOINT;
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE Breakpoint(PDEBUG_BREAKPOINT bp) override {
        UNREFERENCED_PARAMETER(bp);
        return DEBUG_STATUS_BREAK;
    }

    HRESULT STDMETHODCALLTYPE Exception(PEXCEPTION_RECORD64 exception, ULONG first_chance) override {
        UNREFERENCED_PARAMETER(exception);
        UNREFERENCED_PARAMETER(first_chance);
        return DEBUG_STATUS_BREAK;
    }

    HRESULT STDMETHODCALLTYPE CreateProcess(
        ULONG64 image_file_handle,
        ULONG64 handle,
        ULONG64 base_offset,
        ULONG module_size,
        PCSTR module_name,
        PCSTR image_name,
        ULONG check_sum,
        ULONG time_date_stamp,
        ULONG64 initial_thread_handle,
        ULONG64 thread_data_offset,
        ULONG64 start_offset) override {
        UNREFERENCED_PARAMETER(image_file_handle);
        UNREFERENCED_PARAMETER(handle);
        UNREFERENCED_PARAMETER(base_offset);
        UNREFERENCED_PARAMETER(module_size);
        UNREFERENCED_PARAMETER(module_name);
        UNREFERENCED_PARAMETER(image_name);
        UNREFERENCED_PARAMETER(check_sum);
        UNREFERENCED_PARAMETER(time_date_stamp);
        UNREFERENCED_PARAMETER(initial_thread_handle);
        UNREFERENCED_PARAMETER(thread_data_offset);
        UNREFERENCED_PARAMETER(start_offset);
        return DEBUG_STATUS_BREAK;
    }

private:
    volatile LONG refs_ = 1;
};

struct DbgEngSession final {
    ComPtr<IDebugClient> client;
    ComPtr<IDebugControl> control;
    ComPtr<IDebugSymbols> symbols;
    ComPtr<IDebugDataSpaces> data_spaces;
    bool detach_processes_on_close = false;

    ~DbgEngSession() {
        if (client) {
            if (detach_processes_on_close) {
                client->DetachProcesses();
            }
            client->EndSession(DEBUG_END_PASSIVE);
        }
    }
};

std::string hresult_message(HRESULT hr) {
    char buffer[256] = {};
    const auto written = FormatMessageA(
        FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
        nullptr,
        static_cast<DWORD>(hr),
        MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
        buffer,
        static_cast<DWORD>(sizeof(buffer)),
        nullptr);
    std::string message = "HRESULT 0x";
    char hr_text[16] = {};
    std::snprintf(hr_text, sizeof(hr_text), "%08X", static_cast<unsigned int>(hr));
    message += hr_text;
    if (written > 0) {
        message += ": ";
        message += buffer;
        while (!message.empty() && (message.back() == '\r' || message.back() == '\n')) {
            message.pop_back();
        }
    }
    return message;
}

std::wstring utf8_to_wide(const char* text) {
    if (text == nullptr || text[0] == '\0') {
        return std::wstring();
    }
    const int required = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, nullptr, 0);
    if (required <= 0) {
        throw std::runtime_error("UTF-8 path conversion failed");
    }
    std::wstring wide(static_cast<size_t>(required), L'\0');
    const int written = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, wide.data(), required);
    if (written <= 0) {
        throw std::runtime_error("UTF-8 path conversion failed");
    }
    if (!wide.empty() && wide.back() == L'\0') {
        wide.pop_back();
    }
    return wide;
}

std::string windows_error_message(DWORD error) {
    char buffer[256] = {};
    const auto written = FormatMessageA(
        FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
        nullptr,
        error,
        MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
        buffer,
        static_cast<DWORD>(sizeof(buffer)),
        nullptr);
    if (written == 0) {
        return "Win32 error " + std::to_string(error);
    }
    std::string message(buffer, written);
    while (!message.empty() && (message.back() == '\r' || message.back() == '\n')) {
        message.pop_back();
    }
    return message;
}

std::string wide_to_utf8(const std::wstring& text) {
    if (text.empty()) {
        return {};
    }
    const int required = WideCharToMultiByte(
        CP_UTF8,
        0,
        text.c_str(),
        static_cast<int>(text.size()),
        nullptr,
        0,
        nullptr,
        nullptr);
    if (required <= 0) {
        return "<unprintable path>";
    }
    std::string output(static_cast<size_t>(required), '\0');
    WideCharToMultiByte(
        CP_UTF8,
        0,
        text.c_str(),
        static_cast<int>(text.size()),
        output.data(),
        required,
        nullptr,
        nullptr);
    return output;
}

std::wstring dbgeng_dll_path_from_dir(const char* dbgeng_dir_utf8) {
    std::wstring dir = utf8_to_wide(dbgeng_dir_utf8);
    if (dir.empty()) {
        return L"dbgeng.dll";
    }
    const wchar_t last = dir.back();
    if (last != L'\\' && last != L'/') {
        dir.push_back(L'\\');
    }
    dir += L"dbgeng.dll";
    return dir;
}

int32_t ensure_dbgeng_runtime_loaded(const char* dbgeng_dir_utf8) {
    std::lock_guard<std::mutex> guard(g_runtime_mutex);
    const std::wstring dll_path = dbgeng_dll_path_from_dir(dbgeng_dir_utf8);
    // DbgEng is process-global once DebugCreate comes from one dbgeng.dll.
    // Version fallback must happen in separate worker processes owned by
    // the Rust service instead of switching DLL paths in this process.
    if (g_debug_create != nullptr) {
        if (dbgeng_dir_utf8 == nullptr || dbgeng_dir_utf8[0] == '\0') {
            return DA_DBGENG_OK;
        }
        if (!g_dbgeng_loaded_path.empty() && !dll_path.empty() && _wcsicmp(g_dbgeng_loaded_path.c_str(), dll_path.c_str()) != 0) {
            return fail(
                DA_DBGENG_ERR_INTERNAL,
                "DbgEng runtime is already loaded from " + wide_to_utf8(g_dbgeng_loaded_path) +
                    "; requested " + wide_to_utf8(dll_path));
        }
        return DA_DBGENG_OK;
    }

    HMODULE module = nullptr;
    if (dll_path == L"dbgeng.dll") {
        module = LoadLibraryW(dll_path.c_str());
    } else {
        module = LoadLibraryExW(dll_path.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    }
    if (module == nullptr) {
        return fail(
            DA_DBGENG_ERR_INTERNAL,
            "LoadLibrary dbgeng.dll failed at " + wide_to_utf8(dll_path) + ": " +
                windows_error_message(GetLastError()));
    }

    auto debug_create = reinterpret_cast<DebugCreateFn>(GetProcAddress(module, "DebugCreate"));
    if (debug_create == nullptr) {
        return fail(
            DA_DBGENG_ERR_INTERNAL,
            "GetProcAddress(DebugCreate) failed: " + windows_error_message(GetLastError()));
    }

    g_dbgeng_module = module;
    g_debug_create = debug_create;
    g_dbgeng_loaded_path = dll_path;
    return DA_DBGENG_OK;
}

#endif

int32_t fail(DA_DbgEngStatus status, std::string message) noexcept {
    g_last_error = std::move(message);
    return static_cast<int32_t>(status);
}

#ifdef _WIN32
int32_t fail_hr(const char* operation, HRESULT hr) noexcept {
    std::string message = operation;
    message += " failed: ";
    message += hresult_message(hr);
    return fail(DA_DBGENG_ERR_INTERNAL, std::move(message));
}
#endif

int32_t fail_unknown() noexcept {
    return fail(DA_DBGENG_ERR_INTERNAL, "unknown native exception");
}

template <typename Fn>
int32_t guard(Fn&& fn) noexcept {
    try {
        return fn();
    } catch (const std::bad_alloc&) {
        return fail(DA_DBGENG_ERR_INTERNAL, "native allocation failed");
    } catch (const std::exception& error) {
        return fail(DA_DBGENG_ERR_INTERNAL, error.what());
    } catch (...) {
        return fail_unknown();
    }
}

void clear_text_view(DA_DbgEngTextView* out) noexcept {
    out->struct_size = sizeof(DA_DbgEngTextView);
    out->flags = 0;
    out->data = nullptr;
    out->len = 0;
    out->owner = nullptr;
}

int32_t set_output(DA_DbgEngTextView* out, const char* data, size_t len) {
    auto owner = std::make_unique<BufferOwner>();
    owner->bytes.assign(data, data + len);
    out->data = owner->bytes.empty() ? nullptr : owner->bytes.data();
    out->len = owner->bytes.size();
    out->owner = owner.release();
    return DA_DBGENG_OK;
}

int32_t set_output(DA_DbgEngTextView* out, const std::string& text) {
    return set_output(out, text.data(), text.size());
}

#ifdef _WIN32

int32_t query_session_interfaces(DbgEngSession& session) {
    HRESULT hr = session.client->QueryInterface(__uuidof(IDebugControl), reinterpret_cast<void**>(session.control.put()));
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::QueryInterface(IDebugControl)", hr);
    }
    hr = session.client->QueryInterface(__uuidof(IDebugSymbols), reinterpret_cast<void**>(session.symbols.put()));
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::QueryInterface(IDebugSymbols)", hr);
    }
    hr = session.client->QueryInterface(__uuidof(IDebugDataSpaces), reinterpret_cast<void**>(session.data_spaces.put()));
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::QueryInterface(IDebugDataSpaces)", hr);
    }
    return DA_DBGENG_OK;
}

int32_t create_session(std::unique_ptr<DbgEngSession>& session) {
    int32_t status = ensure_dbgeng_runtime_loaded(nullptr);
    if (status != DA_DBGENG_OK) {
        return status;
    }
    session = std::make_unique<DbgEngSession>();
    HRESULT hr = g_debug_create(__uuidof(IDebugClient), reinterpret_cast<void**>(session->client.put()));
    if (FAILED(hr)) {
        return fail_hr("DebugCreate", hr);
    }
    return query_session_interfaces(*session);
}

int32_t wait_for_initial_event(DbgEngSession& session, const char* operation) {
    HRESULT hr = session.control->WaitForEvent(0, 30'000);
    if (hr != S_OK) {
        return fail_hr(operation, hr);
    }
    return DA_DBGENG_OK;
}

int32_t wait_for_launch_initial_event(DbgEngSession& session) {
    BreakingEventCallbacks callbacks;
    IDebugEventCallbacks* previous = nullptr;
    HRESULT hr = session.client->GetEventCallbacks(&previous);
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::GetEventCallbacks(launch process)", hr);
    }

    hr = session.client->SetEventCallbacks(&callbacks);
    if (FAILED(hr)) {
        if (previous != nullptr) {
            previous->Release();
        }
        return fail_hr("IDebugClient::SetEventCallbacks(launch process)", hr);
    }

    hr = session.control->WaitForEvent(0, 30'000);

    const HRESULT restore_hr = session.client->SetEventCallbacks(previous);
    if (previous != nullptr) {
        previous->Release();
    }
    if (FAILED(restore_hr)) {
        return fail_hr("IDebugClient::SetEventCallbacks(restore)", restore_hr);
    }

    if (hr == S_OK) {
        return DA_DBGENG_OK;
    }

    ULONG execution_status = 0;
    const HRESULT status_hr = session.control->GetExecutionStatus(&execution_status);
    if (hr == E_UNEXPECTED && SUCCEEDED(status_hr) && execution_status == DEBUG_STATUS_NO_DEBUGGEE) {
        return DA_DBGENG_OK;
    }

    return fail_hr("IDebugControl::WaitForEvent(launch process)", hr);
}

int32_t open_debug_file(DbgEngSession& session, const char* path_utf8) {
    const std::wstring path_wide = utf8_to_wide(path_utf8);
    ComPtr<IDebugClient4> client4;
    HRESULT hr = session.client->QueryInterface(__uuidof(IDebugClient4), reinterpret_cast<void**>(client4.put()));
    if (SUCCEEDED(hr) && client4) {
        hr = client4->OpenDumpFileWide(path_wide.c_str(), 0);
        if (FAILED(hr)) {
            return fail_hr("IDebugClient4::OpenDumpFileWide(open file)", hr);
        }
        return DA_DBGENG_OK;
    }

    hr = session.client->OpenDumpFile(path_utf8);
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::OpenDumpFile(open file)", hr);
    }
    return DA_DBGENG_OK;
}

int32_t launch_debug_process(DbgEngSession& session, const char* command_line_utf8) {
    std::wstring command_line_wide = utf8_to_wide(command_line_utf8);
    ComPtr<IDebugClient4> client4;
    HRESULT hr = session.client->QueryInterface(__uuidof(IDebugClient4), reinterpret_cast<void**>(client4.put()));
    if (SUCCEEDED(hr) && client4) {
        hr = client4->CreateProcessWide(0, command_line_wide.data(), DEBUG_ONLY_THIS_PROCESS);
        if (FAILED(hr)) {
            return fail_hr("IDebugClient4::CreateProcessWide(launch process)", hr);
        }
        return DA_DBGENG_OK;
    }

    std::string command_line_ansi(command_line_utf8);
    hr = session.client->CreateProcess(0, command_line_ansi.data(), DEBUG_ONLY_THIS_PROCESS);
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::CreateProcess(launch process)", hr);
    }
    return DA_DBGENG_OK;
}

DbgEngSession* session_from_handle(DA_DbgEngSessionHandle* handle) {
    return reinterpret_cast<DbgEngSession*>(handle);
}

int32_t execute_command(DbgEngSession& session, const char* command_utf8, DA_DbgEngTextView* out) {
    // Temporarily replace the output callback to capture this command only.
    // Always restore the previous callback before returning so later commands
    // or other DbgEng clients do not write into the wrong buffer.
    CapturingOutputCallbacks callbacks;
    IDebugOutputCallbacks* previous = nullptr;
    HRESULT hr = session.client->GetOutputCallbacks(&previous);
    if (FAILED(hr)) {
        return fail_hr("IDebugClient::GetOutputCallbacks", hr);
    }

    hr = session.client->SetOutputCallbacks(&callbacks);
    if (FAILED(hr)) {
        if (previous != nullptr) {
            previous->Release();
        }
        return fail_hr("IDebugClient::SetOutputCallbacks", hr);
    }

    hr = session.control->Execute(DEBUG_OUTCTL_THIS_CLIENT, command_utf8, DEBUG_EXECUTE_DEFAULT);

    const HRESULT restore_hr = session.client->SetOutputCallbacks(previous);
    if (previous != nullptr) {
        previous->Release();
    }
    if (FAILED(restore_hr)) {
        return fail_hr("IDebugClient::SetOutputCallbacks(restore)", restore_hr);
    }
    if (FAILED(hr)) {
        // Do not include command_utf8 here: native errors can flow into
        // persistent service diagnostics, while commands may contain secrets.
        return fail_hr("IDebugControl::Execute", hr);
    }

    return set_output(out, callbacks.output());
}

#endif

} // namespace

DA_DBGENG_EXPORT int32_t da_dbgeng_abi_version(DA_DbgEngVersion* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out version pointer is null");
        }

        out->struct_size = sizeof(DA_DbgEngVersion);
        out->flags = 0;
        out->abi_major = 0;
        out->abi_minor = 3;
        out->abi_patch = 0;
        g_last_error.clear();
        return DA_DBGENG_OK;
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_load_runtime(const char* dbgeng_dir_utf8) {
    return guard([&]() -> int32_t {
#ifndef _WIN32
        (void)dbgeng_dir_utf8;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        int32_t status = ensure_dbgeng_runtime_loaded(dbgeng_dir_utf8);
        if (status == DA_DBGENG_OK) {
            g_last_error.clear();
        }
        return status;
#endif
    });
}

DA_DBGENG_EXPORT void da_dbgeng_release_view(void* owner) {
    try {
        delete static_cast<ViewOwner*>(owner);
    } catch (...) {
    }
}

DA_DBGENG_EXPORT int32_t da_dbgeng_last_error(
    char* buffer,
    size_t buffer_len,
    size_t* required_len) {
    return guard([&]() -> int32_t {
        const size_t required = g_last_error.size() + 1;
        if (required_len != nullptr) {
            *required_len = required;
        }

        if (buffer == nullptr || buffer_len < required) {
            return static_cast<int32_t>(DA_DBGENG_ERR_BUFFER_TOO_SMALL);
        }

        std::memcpy(buffer, g_last_error.c_str(), required);
        return DA_DBGENG_OK;
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_open_file(
    const char* path_utf8,
    DA_DbgEngSessionHandle** out_handle) {
    return guard([&]() -> int32_t {
        if (out_handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out session handle pointer is null");
        }
        *out_handle = nullptr;

        if (path_utf8 == nullptr || path_utf8[0] == '\0') {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "debug file path is empty");
        }

#ifndef _WIN32
        (void)path_utf8;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        std::unique_ptr<DbgEngSession> session;
        int32_t status = create_session(session);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        status = open_debug_file(*session, path_utf8);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        status = wait_for_initial_event(*session, "IDebugControl::WaitForEvent(open file)");
        if (status != DA_DBGENG_OK) {
            return status;
        }
        *out_handle = reinterpret_cast<DA_DbgEngSessionHandle*>(session.release());
        g_last_error.clear();
        return DA_DBGENG_OK;
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_attach_process(
    uint32_t pid,
    DA_DbgEngSessionHandle** out_handle) {
    return guard([&]() -> int32_t {
        if (out_handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out session handle pointer is null");
        }
        *out_handle = nullptr;

        if (pid == 0) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "attach pid must be greater than zero");
        }

#ifndef _WIN32
        (void)pid;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        std::unique_ptr<DbgEngSession> session;
        int32_t status = create_session(session);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        // Use non-invasive/no-suspend attach: read target state without taking
        // over the debug lifecycle. Destruction detaches passively so closing a
        // DbgAtlas session does not terminate the target process.
        HRESULT hr = session->client->AttachProcess(
            0,
            pid,
            DEBUG_ATTACH_NONINVASIVE | DEBUG_ATTACH_NONINVASIVE_NO_SUSPEND);
        if (FAILED(hr)) {
            return fail_hr("IDebugClient::AttachProcess", hr);
        }
        session->detach_processes_on_close = true;
        (void)session->control->WaitForEvent(0, 1'000);
        *out_handle = reinterpret_cast<DA_DbgEngSessionHandle*>(session.release());
        g_last_error.clear();
        return DA_DBGENG_OK;
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_launch_process(
    const char* command_line_utf8,
    DA_DbgEngSessionHandle** out_handle) {
    return guard([&]() -> int32_t {
        if (out_handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out session handle pointer is null");
        }
        *out_handle = nullptr;

        if (command_line_utf8 == nullptr || command_line_utf8[0] == '\0') {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "launch command line is empty");
        }

#ifndef _WIN32
        (void)command_line_utf8;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        std::unique_ptr<DbgEngSession> session;
        int32_t status = create_session(session);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        status = launch_debug_process(*session, command_line_utf8);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        session->detach_processes_on_close = true;
        status = wait_for_launch_initial_event(*session);
        if (status != DA_DBGENG_OK) {
            return status;
        }
        *out_handle = reinterpret_cast<DA_DbgEngSessionHandle*>(session.release());
        g_last_error.clear();
        return DA_DBGENG_OK;
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_execute(
    DA_DbgEngSessionHandle* handle,
    const char* command_utf8,
    DA_DbgEngTextView* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out text view pointer is null");
        }
        clear_text_view(out);

        if (handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        if (command_utf8 == nullptr || command_utf8[0] == '\0') {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "debug command is empty");
        }

#ifndef _WIN32
        (void)handle;
        (void)command_utf8;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        int32_t status = execute_command(*session_from_handle(handle), command_utf8, out);
        if (status == DA_DBGENG_OK) {
            g_last_error.clear();
        }
        return status;
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_add_symbols(
    DA_DbgEngSessionHandle* handle,
    const char* symbol_path_utf8,
    int32_t reload,
    DA_DbgEngTextView* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out text view pointer is null");
        }
        clear_text_view(out);

        if (handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        if (symbol_path_utf8 == nullptr || symbol_path_utf8[0] == '\0') {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "symbol path is empty");
        }

#ifndef _WIN32
        (void)handle;
        (void)symbol_path_utf8;
        (void)reload;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        auto& session = *session_from_handle(handle);
        HRESULT hr = session.symbols->AppendSymbolPath(symbol_path_utf8);
        if (FAILED(hr)) {
            return fail_hr("IDebugSymbols::AppendSymbolPath", hr);
        }

        std::string output = "symbol path appended\n";
        if (reload != 0) {
            DA_DbgEngTextView reload_output{};
            clear_text_view(&reload_output);
            int32_t status = execute_command(session, ".reload", &reload_output);
            if (status != DA_DBGENG_OK) {
                da_dbgeng_release_view(reload_output.owner);
                return status;
            }
            if (reload_output.data != nullptr && reload_output.len > 0) {
                output.append(reload_output.data, reload_output.len);
            }
            da_dbgeng_release_view(reload_output.owner);
        }

        g_last_error.clear();
        return set_output(out, output);
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_read_virtual(
    DA_DbgEngSessionHandle* handle,
    uint64_t address,
    uint32_t length,
    DA_DbgEngTextView* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out byte view pointer is null");
        }
        clear_text_view(out);

        if (handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        if (length == 0) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "read length must be greater than zero");
        }

#ifndef _WIN32
        (void)handle;
        (void)address;
        (void)length;
        return fail(DA_DBGENG_ERR_INTERNAL, "DbgEng is only available on Windows");
#else
        auto owner = std::make_unique<BufferOwner>();
        owner->bytes.resize(length);
        ULONG bytes_read = 0;
        HRESULT hr = session_from_handle(handle)->data_spaces->ReadVirtual(
            address,
            owner->bytes.data(),
            length,
            &bytes_read);
        if (FAILED(hr)) {
            char operation[128] = {};
            std::snprintf(
                operation,
                sizeof(operation),
                "IDebugDataSpaces::ReadVirtual(address=0x%llX,length=%u)",
                static_cast<unsigned long long>(address),
                length);
            return fail_hr(operation, hr);
        }
        owner->bytes.resize(std::min<size_t>(owner->bytes.size(), bytes_read));
        out->data = owner->bytes.empty() ? nullptr : owner->bytes.data();
        out->len = owner->bytes.size();
        out->owner = owner.release();
        g_last_error.clear();
        return DA_DBGENG_OK;
#endif
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_close(DA_DbgEngSessionHandle* handle) {
    return guard([&]() -> int32_t {
        if (handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "session handle is null");
        }
#ifdef _WIN32
        delete reinterpret_cast<DbgEngSession*>(handle);
#else
        (void)handle;
#endif
        g_last_error.clear();
        return DA_DBGENG_OK;
    });
}
