#include "dbgatlas_native.h"

#include <algorithm>
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

int32_t fail(DA_Status status, std::string message) noexcept {
    g_last_error = std::move(message);
    return static_cast<int32_t>(status);
}

int32_t fail_unknown() noexcept {
    return fail(DA_ERR_INTERNAL, "unknown native exception");
}

template <typename Fn>
int32_t guard(Fn&& fn) noexcept {
    try {
        return fn();
    } catch (const std::bad_alloc&) {
        return fail(DA_ERR_INTERNAL, "native allocation failed");
    } catch (const std::exception& error) {
        return fail(DA_ERR_INTERNAL, error.what());
    } catch (...) {
        return fail_unknown();
    }
}

void clear_text_view(DA_TextView* out) noexcept {
    out->struct_size = sizeof(DA_TextView);
    out->flags = 0;
    out->data = nullptr;
    out->len = 0;
    out->owner = nullptr;
}

} // namespace

DA_EXPORT int32_t da_abi_version(DA_Version* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ERR_INVALID_ARGUMENT, "out version pointer is null");
        }

        out->struct_size = sizeof(DA_Version);
        out->flags = 0;
        out->abi_major = 0;
        out->abi_minor = 1;
        out->abi_patch = 0;
        g_last_error.clear();
        return DA_OK;
    });
}

DA_EXPORT int32_t da_native_hello(const char* input_utf8, DA_TextView* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ERR_INVALID_ARGUMENT, "out text view pointer is null");
        }
        clear_text_view(out);

        const char* input = input_utf8 == nullptr ? "" : input_utf8;
        auto owner = std::make_unique<TextOwner>();
        owner->text = "DbgAtlas native hello: ";
        owner->text += input;

        out->data = owner->text.data();
        out->len = owner->text.size();
        out->owner = owner.release();
        g_last_error.clear();
        return DA_OK;
    });
}

DA_EXPORT void da_release_view(void* owner) {
    try {
        delete static_cast<ViewOwner*>(owner);
    } catch (...) {
    }
}

DA_EXPORT int32_t da_last_error(char* buffer, size_t buffer_len, size_t* required_len) {
    return guard([&]() -> int32_t {
        const size_t required = g_last_error.size() + 1;
        if (required_len != nullptr) {
            *required_len = required;
        }

        if (buffer == nullptr || buffer_len < required) {
            return static_cast<int32_t>(DA_ERR_BUFFER_TOO_SMALL);
        }

        std::memcpy(buffer, g_last_error.c_str(), required);
        return DA_OK;
    });
}
