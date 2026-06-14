#include "dbgatlas_dbgeng.h"

#ifdef _WIN32
#include <Windows.h>
#include <DbgEng.h>
#endif

#include <algorithm>
#include <cstdio>
#include <cstring>
#include <exception>
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

struct DbgEngSession final {
    ComPtr<IDebugClient> client;
    ComPtr<IDebugControl> control;
    ComPtr<IDebugSymbols> symbols;
    ComPtr<IDebugDataSpaces> data_spaces;
    bool attached_process = false;

    ~DbgEngSession() {
        if (client) {
            if (attached_process) {
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
    session = std::make_unique<DbgEngSession>();
    HRESULT hr = DebugCreate(__uuidof(IDebugClient), reinterpret_cast<void**>(session->client.put()));
    if (FAILED(hr)) {
        return fail_hr("DebugCreate", hr);
    }
    return query_session_interfaces(*session);
}

int32_t wait_for_initial_event(DbgEngSession& session, const char* operation) {
    HRESULT hr = session.control->WaitForEvent(0, 30'000);
    if (FAILED(hr)) {
        return fail_hr(operation, hr);
    }
    return DA_DBGENG_OK;
}

DbgEngSession* session_from_handle(DA_DbgEngSessionHandle* handle) {
    return reinterpret_cast<DbgEngSession*>(handle);
}

int32_t execute_command(DbgEngSession& session, const char* command_utf8, DA_DbgEngTextView* out) {
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
        out->abi_minor = 2;
        out->abi_patch = 0;
        g_last_error.clear();
        return DA_DBGENG_OK;
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

DA_DBGENG_EXPORT int32_t da_dbgeng_session_open_dump(
    const char* path_utf8,
    DA_DbgEngSessionHandle** out_handle) {
    return guard([&]() -> int32_t {
        if (out_handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out session handle pointer is null");
        }
        *out_handle = nullptr;

        if (path_utf8 == nullptr || path_utf8[0] == '\0') {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "dump path is empty");
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
        HRESULT hr = session->client->OpenDumpFile(path_utf8);
        if (FAILED(hr)) {
            return fail_hr("IDebugClient::OpenDumpFile", hr);
        }
        status = wait_for_initial_event(*session, "IDebugControl::WaitForEvent(open dump)");
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
        HRESULT hr = session->client->AttachProcess(
            0,
            pid,
            DEBUG_ATTACH_NONINVASIVE | DEBUG_ATTACH_NONINVASIVE_NO_SUSPEND);
        if (FAILED(hr)) {
            return fail_hr("IDebugClient::AttachProcess", hr);
        }
        session->attached_process = true;
        (void)session->control->WaitForEvent(0, 1'000);
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
            return fail_hr("IDebugDataSpaces::ReadVirtual", hr);
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
