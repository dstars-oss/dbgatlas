#define NOMINMAX
#include "dbgatlas_ida.h"

#include <cstring>

#ifdef _WIN32
#include <windows.h>
#include <delayimp.h>

#include <ida.hpp>
#include <auto.hpp>
#include <funcs.hpp>
#include <idalib.hpp>

#include <limits>
#include <memory>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>

namespace {

thread_local std::string g_last_error;
std::mutex g_session_mutex;
bool g_active_session = false;

using init_library_fn = int(idaapi*)(int, char**);
using open_database_fn = int(idaapi*)(const char*, bool, const char*);
using close_database_fn = void(idaapi*)(bool);
using get_library_version_fn = bool(idaapi*)(int&, int&, int&);
using auto_wait_fn = bool(idaapi*)();
using get_func_fn = func_t*(idaapi*)(ea_t);
using get_func_name_fn = ssize_t(idaapi*)(qstring*, ea_t);

struct IdaApi {
    HMODULE ida = nullptr;
    HMODULE idalib = nullptr;
    DLL_DIRECTORY_COOKIE dll_directory = nullptr;
    init_library_fn init_library = nullptr;
    open_database_fn open_database = nullptr;
    close_database_fn close_database = nullptr;
    get_library_version_fn get_library_version = nullptr;
    auto_wait_fn auto_wait = nullptr;
    get_func_fn get_func = nullptr;
    get_func_name_fn get_func_name = nullptr;
};

struct IdaSessionHandleImpl {
    IdaApi api;
    std::thread::id owner_thread;
    bool database_open = false;
};

struct TextOwner {
    explicit TextOwner(std::string value) : text(std::move(value)) {}
    std::string text;
};

void set_last_error(std::string message) {
    g_last_error = std::move(message);
}

int32_t fail(int32_t status, std::string message) {
    set_last_error(std::move(message));
    return status;
}

template <typename T>
void bind_symbol(HMODULE module, const char* name, T& out) {
    FARPROC proc = GetProcAddress(module, name);
    if (proc == nullptr) {
        throw std::runtime_error(std::string("missing IDA export `") + name + "`");
    }
    out = reinterpret_cast<T>(proc);
}

std::wstring utf8_to_wide(const char* value, const char* field) {
    if (value == nullptr || value[0] == '\0') {
        throw std::invalid_argument(std::string(field) + " must not be empty");
    }
    int required = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value, -1, nullptr, 0);
    if (required <= 0) {
        throw std::invalid_argument(std::string(field) + " is not valid UTF-8");
    }
    std::wstring wide(static_cast<size_t>(required), L'\0');
    int written = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value, -1, wide.data(), required);
    if (written <= 0) {
        throw std::invalid_argument(std::string(field) + " is not valid UTF-8");
    }
    if (!wide.empty() && wide.back() == L'\0') {
        wide.pop_back();
    }
    return wide;
}

std::string utf8_string(const char* value, const char* field) {
    if (value == nullptr || value[0] == '\0') {
        throw std::invalid_argument(std::string(field) + " must not be empty");
    }
    (void)utf8_to_wide(value, field);
    return std::string(value);
}

bool is_existing_directory(const std::wstring& path) {
    DWORD attrs = GetFileAttributesW(path.c_str());
    return attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0;
}

bool is_existing_file(const std::wstring& path) {
    DWORD attrs = GetFileAttributesW(path.c_str());
    return attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY) == 0;
}

std::wstring join_path(const std::wstring& base, const wchar_t* child) {
    std::wstring result = base;
    if (!result.empty() && result.back() != L'\\' && result.back() != L'/') {
        result.push_back(L'\\');
    }
    result.append(child);
    return result;
}

void release_api(IdaApi& api) {
    if (api.idalib != nullptr) {
        FreeLibrary(api.idalib);
        api.idalib = nullptr;
    }
    if (api.ida != nullptr) {
        FreeLibrary(api.ida);
        api.ida = nullptr;
    }
    if (api.dll_directory != nullptr) {
        RemoveDllDirectory(api.dll_directory);
        api.dll_directory = nullptr;
    }
}

void configure_ida_dll_search_path(IdaApi& api, const std::wstring& install_dir) {
    constexpr DWORD dll_search_flags = LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_USER_DIRS;
    if (!SetDefaultDllDirectories(dll_search_flags)) {
        std::wstringstream stream;
        stream << L"failed to configure IDA DLL search path (SetDefaultDllDirectories GetLastError="
               << GetLastError() << L")";
        std::wstring wide = stream.str();
        throw std::runtime_error(std::string(wide.begin(), wide.end()));
    }

    api.dll_directory = AddDllDirectory(install_dir.c_str());
    if (api.dll_directory == nullptr) {
        std::wstringstream stream;
        stream << L"failed to add IDA DLL directory " << install_dir
               << L" (AddDllDirectory GetLastError=" << GetLastError() << L")";
        std::wstring wide = stream.str();
        throw std::runtime_error(std::string(wide.begin(), wide.end()));
    }
}

HMODULE load_ida_library_from(const std::wstring& path) {
    HMODULE module = LoadLibraryExW(path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (module == nullptr) {
        std::wstringstream stream;
        stream << L"failed to load " << path << L" (GetLastError=" << GetLastError() << L")";
        std::wstring wide = stream.str();
        throw std::runtime_error(std::string(wide.begin(), wide.end()));
    }
    return module;
}

IdaApi load_api(const std::wstring& install_dir) {
    if (!is_existing_directory(install_dir)) {
        throw std::invalid_argument("tools.ida.install_dir does not exist or is not a directory");
    }
    std::wstring ida_path = join_path(install_dir, L"ida.dll");
    std::wstring idalib_path = join_path(install_dir, L"idalib.dll");
    if (!is_existing_file(ida_path)) {
        throw std::invalid_argument("tools.ida.install_dir is missing ida.dll");
    }
    if (!is_existing_file(idalib_path)) {
        throw std::invalid_argument("tools.ida.install_dir is missing idalib.dll");
    }

    IdaApi api;
    try {
        configure_ida_dll_search_path(api, install_dir);
        api.ida = load_ida_library_from(ida_path);
        api.idalib = load_ida_library_from(idalib_path);
        bind_symbol(api.idalib, "init_library", api.init_library);
        bind_symbol(api.idalib, "open_database", api.open_database);
        bind_symbol(api.idalib, "close_database", api.close_database);
        bind_symbol(api.idalib, "get_library_version", api.get_library_version);
        bind_symbol(api.ida, "auto_wait", api.auto_wait);
        bind_symbol(api.ida, "get_func", api.get_func);
        bind_symbol(api.ida, "get_func_name", api.get_func_name);
        return api;
    } catch (...) {
        release_api(api);
        throw;
    }
}

DA_IdaTextView make_text_view(const std::string& text) {
    auto owner = std::make_unique<TextOwner>(text);
    DA_IdaTextView view{};
    view.struct_size = sizeof(DA_IdaTextView);
    view.data = owner->text.data();
    view.len = owner->text.size();
    view.owner = owner.release();
    return view;
}

void ensure_owner_thread(IdaSessionHandleImpl* handle) {
    if (handle->owner_thread != std::this_thread::get_id()) {
        throw std::runtime_error("IDA session used from a different thread");
    }
}

int32_t close_session(IdaSessionHandleImpl* handle) {
    if (handle == nullptr) {
        return DA_IDA_OK;
    }
    try {
        ensure_owner_thread(handle);
        if (handle->database_open && handle->api.close_database != nullptr) {
            handle->api.close_database(false);
            handle->database_open = false;
        }
        release_api(handle->api);
        delete handle;
        g_active_session = false;
        return DA_IDA_OK;
    } catch (const std::exception& error) {
        delete handle;
        g_active_session = false;
        return fail(DA_IDA_ERR_INTERNAL, error.what());
    }
}

} // namespace

extern "C" FARPROC WINAPI __delayLoadFailureHook2(unsigned dliNotify, PDelayLoadInfo pdli) {
    (void)dliNotify;
    (void)pdli;
    return nullptr;
}

DA_IDA_EXPORT int32_t da_ida_abi_version(DA_IdaVersion* out) {
    if (out == nullptr || out->struct_size < sizeof(DA_IdaVersion)) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, "version output buffer is invalid");
    }
    out->flags = 0;
    out->abi_major = 0;
    out->abi_minor = 1;
    out->abi_patch = 0;
    out->ida_major = 0;
    out->ida_minor = 0;
    out->ida_build = 0;
    return DA_IDA_OK;
}

DA_IDA_EXPORT void da_ida_release_view(void* owner) {
    delete static_cast<TextOwner*>(owner);
}

DA_IDA_EXPORT int32_t da_ida_last_error(char* buffer, size_t buffer_len, size_t* required_len) {
    const size_t required = g_last_error.size() + 1;
    if (required_len != nullptr) {
        *required_len = required;
    }
    if (buffer == nullptr || buffer_len == 0) {
        return DA_IDA_ERR_BUFFER_TOO_SMALL;
    }
    if (buffer_len < required) {
        return DA_IDA_ERR_BUFFER_TOO_SMALL;
    }
    memcpy(buffer, g_last_error.c_str(), required);
    return DA_IDA_OK;
}

DA_IDA_EXPORT int32_t da_ida_session_open(
    const char* install_dir_utf8,
    const char* database_path_utf8,
    DA_IdaSessionHandle** out_handle) {
    if (out_handle == nullptr) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, "session output handle is null");
    }
    *out_handle = nullptr;
    std::lock_guard<std::mutex> guard(g_session_mutex);
    if (g_active_session) {
        return fail(DA_IDA_ERR_IDA, "only one IDA session is supported in this MVP");
    }

    try {
        std::wstring install_dir = utf8_to_wide(install_dir_utf8, "install_dir");
        std::wstring database_path = utf8_to_wide(database_path_utf8, "database_path");
        std::string database_path_utf8_copy = utf8_string(database_path_utf8, "database_path");
        if (!is_existing_file(database_path)) {
            return fail(DA_IDA_ERR_INVALID_ARGUMENT, "database_path does not exist or is not a file");
        }

        auto handle = std::make_unique<IdaSessionHandleImpl>();
        handle->owner_thread = std::this_thread::get_id();
        handle->api = load_api(install_dir);
        int init_result = handle->api.init_library(0, nullptr);
        if (init_result != 0) {
            release_api(handle->api);
            return fail(DA_IDA_ERR_IDA, "init_library failed with result " + std::to_string(init_result));
        }
        int open_result = handle->api.open_database(database_path_utf8_copy.c_str(), true, nullptr);
        if (open_result != 0) {
            release_api(handle->api);
            return fail(DA_IDA_ERR_IDA, "open_database failed with result " + std::to_string(open_result));
        }
        if (!handle->api.auto_wait()) {
            handle->api.close_database(false);
            release_api(handle->api);
            return fail(DA_IDA_ERR_IDA, "auto_wait failed");
        }
        handle->database_open = true;
        *out_handle = reinterpret_cast<DA_IdaSessionHandle*>(handle.release());
        g_active_session = true;
        return DA_IDA_OK;
    } catch (const std::invalid_argument& error) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, error.what());
    } catch (const std::exception& error) {
        return fail(DA_IDA_ERR_DYNAMIC_LOAD, error.what());
    }
}

DA_IDA_EXPORT int32_t da_ida_lookup_function(
    DA_IdaSessionHandle* handle,
    uint64_t runtime_address,
    uint64_t runtime_module_base,
    uint64_t ida_image_base,
    DA_IdaFunctionLookup* out) {
    if (handle == nullptr || out == nullptr || out->struct_size < sizeof(DA_IdaFunctionLookup)) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, "lookup arguments are invalid");
    }
    try {
        auto* impl = reinterpret_cast<IdaSessionHandleImpl*>(handle);
        ensure_owner_thread(impl);
        if (runtime_address < runtime_module_base) {
            return fail(DA_IDA_ERR_INVALID_ARGUMENT, "runtime_address is below runtime_module_base");
        }
        uint64_t rva = runtime_address - runtime_module_base;
        if (ida_image_base > std::numeric_limits<uint64_t>::max() - rva) {
            return fail(DA_IDA_ERR_INVALID_ARGUMENT, "ida_ea overflow");
        }
        uint64_t ida_ea = ida_image_base + rva;
        out->flags = 0;
        out->runtime_address = runtime_address;
        out->runtime_module_base = runtime_module_base;
        out->rva = rva;
        out->ida_image_base = ida_image_base;
        out->ida_ea = ida_ea;
        out->function_start = 0;
        out->function_end = 0;
        out->found = 0;
        out->function_name = make_text_view("");

        func_t* function = impl->api.get_func(static_cast<ea_t>(ida_ea));
        if (function == nullptr) {
            return DA_IDA_OK;
        }

        qstring name;
        ssize_t name_len = impl->api.get_func_name(&name, static_cast<ea_t>(ida_ea));
        std::string function_name;
        if (name_len > 0 && name.c_str() != nullptr) {
            function_name.assign(name.c_str(), static_cast<size_t>(name_len));
        }
        da_ida_release_view(out->function_name.owner);
        out->function_start = static_cast<uint64_t>(function->start_ea);
        out->function_end = static_cast<uint64_t>(function->end_ea);
        out->found = 1;
        out->function_name = make_text_view(function_name);
        return DA_IDA_OK;
    } catch (const std::invalid_argument& error) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, error.what());
    } catch (const std::exception& error) {
        return fail(DA_IDA_ERR_INTERNAL, error.what());
    }
}

DA_IDA_EXPORT int32_t da_ida_session_close(DA_IdaSessionHandle* handle) {
    std::lock_guard<std::mutex> guard(g_session_mutex);
    return close_session(reinterpret_cast<IdaSessionHandleImpl*>(handle));
}

#else

DA_IDA_EXPORT int32_t da_ida_abi_version(DA_IdaVersion* out) {
    if (out == nullptr || out->struct_size < sizeof(DA_IdaVersion)) {
        return DA_IDA_ERR_INVALID_ARGUMENT;
    }
    out->flags = 0;
    out->abi_major = 0;
    out->abi_minor = 1;
    out->abi_patch = 0;
    out->ida_major = 0;
    out->ida_minor = 0;
    out->ida_build = 0;
    return DA_IDA_OK;
}

DA_IDA_EXPORT void da_ida_release_view(void* owner) {
    (void)owner;
}

DA_IDA_EXPORT int32_t da_ida_last_error(char* buffer, size_t buffer_len, size_t* required_len) {
    const char* message = "IDA adapter is only supported on Windows";
    size_t required = strlen(message) + 1;
    if (required_len != nullptr) {
        *required_len = required;
    }
    if (buffer == nullptr || buffer_len < required) {
        return DA_IDA_ERR_BUFFER_TOO_SMALL;
    }
    memcpy(buffer, message, required);
    return DA_IDA_OK;
}

DA_IDA_EXPORT int32_t da_ida_session_open(
    const char* install_dir_utf8,
    const char* database_path_utf8,
    DA_IdaSessionHandle** out_handle) {
    (void)install_dir_utf8;
    (void)database_path_utf8;
    (void)out_handle;
    return DA_IDA_ERR_IDA;
}

DA_IDA_EXPORT int32_t da_ida_lookup_function(
    DA_IdaSessionHandle* handle,
    uint64_t runtime_address,
    uint64_t runtime_module_base,
    uint64_t ida_image_base,
    DA_IdaFunctionLookup* out) {
    (void)handle;
    (void)runtime_address;
    (void)runtime_module_base;
    (void)ida_image_base;
    (void)out;
    return DA_IDA_ERR_IDA;
}

DA_IDA_EXPORT int32_t da_ida_session_close(DA_IdaSessionHandle* handle) {
    (void)handle;
    return DA_IDA_OK;
}

#endif
