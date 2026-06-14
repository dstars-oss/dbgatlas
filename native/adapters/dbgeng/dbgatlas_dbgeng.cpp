#include "dbgatlas_dbgeng.h"

#include <cstring>
#include <exception>
#include <memory>
#include <new>
#include <string>

namespace {

thread_local std::string g_last_error;

struct ViewOwner {
    virtual ~ViewOwner() = default;
};

struct TextOwner final : ViewOwner {
    std::string text;
};

struct DbgEngSession final {
    std::string dump_path;
};

int32_t fail(DA_DbgEngStatus status, std::string message) noexcept {
    g_last_error = std::move(message);
    return static_cast<int32_t>(status);
}

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

} // namespace

DA_DBGENG_EXPORT int32_t da_dbgeng_abi_version(DA_DbgEngVersion* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "out version pointer is null");
        }

        out->struct_size = sizeof(DA_DbgEngVersion);
        out->flags = 0;
        out->abi_major = 0;
        out->abi_minor = 1;
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

        auto session = std::make_unique<DbgEngSession>();
        session->dump_path = path_utf8;
        *out_handle = reinterpret_cast<DA_DbgEngSessionHandle*>(session.release());
        g_last_error.clear();
        return DA_DBGENG_OK;
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

        const auto* session = reinterpret_cast<const DbgEngSession*>(handle);
        auto owner = std::make_unique<TextOwner>();
        owner->text = "DbgEng session skeleton: real DbgEng execution is not wired yet\n";
        owner->text += "dump: ";
        owner->text += session->dump_path;
        owner->text += "\ncommand: ";
        owner->text += command_utf8;
        owner->text += "\n";

        out->data = owner->text.data();
        out->len = owner->text.size();
        out->owner = owner.release();
        g_last_error.clear();
        return DA_DBGENG_OK;
    });
}

DA_DBGENG_EXPORT int32_t da_dbgeng_session_close(DA_DbgEngSessionHandle* handle) {
    return guard([&]() -> int32_t {
        if (handle == nullptr) {
            return fail(DA_DBGENG_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        delete reinterpret_cast<DbgEngSession*>(handle);
        g_last_error.clear();
        return DA_DBGENG_OK;
    });
}
