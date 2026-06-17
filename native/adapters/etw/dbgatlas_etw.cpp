#include "dbgatlas_etw.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#include <combaseapi.h>
#include <evntrace.h>
#include <initguid.h>
#include <oleauto.h>
#include <relogger.h>
#include <tdh.h>
#endif

#ifdef _WIN32
DEFINE_GUID(IID_ITraceEventCallback, 0x3ed25501, 0x593f, 0x43e9, 0x8f, 0x38, 0x3a, 0xb4, 0x6f, 0x5a, 0x4a, 0x52);
DEFINE_GUID(IID_ITraceRelogger, 0xf754ad43, 0x3bcc, 0x4286, 0x80, 0x09, 0x9c, 0x5d, 0xa2, 0x14, 0xe8, 0x4e);
DEFINE_GUID(DA_KernelProcessProviderGuid, 0x22fb2cd6, 0x0e7b, 0x422b, 0xa0, 0xc7, 0x2f, 0xad, 0x1f, 0xd0, 0xe7, 0x16);
DEFINE_GUID(DA_KernelFileProviderGuid, 0xedd08927, 0x9cc4, 0x4e65, 0xb9, 0x70, 0xc2, 0x56, 0x0f, 0xb5, 0xc2, 0x89);
DEFINE_GUID(DA_KernelRegistryProviderGuid, 0x70eb4f03, 0xc1de, 0x4f73, 0xa0, 0x51, 0x33, 0xd1, 0x3d, 0x54, 0x13, 0xbd);
DEFINE_GUID(DA_KernelNetworkProviderGuid, 0x7dd42a49, 0x5329, 0x4832, 0x8d, 0xfd, 0x43, 0xd9, 0x79, 0x15, 0x3a, 0x88);
DEFINE_GUID(DA_ClassicProcessGuid, 0x3d6fa8d0, 0xfe05, 0x11d0, 0x9d, 0xda, 0x00, 0xc0, 0x4f, 0xd7, 0xba, 0x7c);
DEFINE_GUID(DA_ClassicThreadGuid, 0x3d6fa8d1, 0xfe05, 0x11d0, 0x9d, 0xda, 0x00, 0xc0, 0x4f, 0xd7, 0xba, 0x7c);
DEFINE_GUID(DA_ClassicFileIoGuid, 0x90cbdc39, 0x4a3e, 0x11d1, 0x84, 0xf4, 0x00, 0x00, 0xf8, 0x04, 0x64, 0xe3);
DEFINE_GUID(DA_ClassicTcpIpGuid, 0x9a280ac0, 0xc8e0, 0x11d1, 0x84, 0xe2, 0x00, 0xc0, 0x4f, 0xb9, 0x98, 0xa2);
DEFINE_GUID(DA_ClassicUdpIpGuid, 0xbf3a50c5, 0xa9c9, 0x4988, 0xa0, 0x05, 0x2d, 0xf0, 0xb7, 0xc8, 0x0f, 0x80);
DEFINE_GUID(DA_ClassicImageLoadGuid, 0x2cb15d1d, 0x5fc1, 0x11d2, 0xab, 0xe1, 0x00, 0xa0, 0xc9, 0x11, 0xf5, 0x18);
DEFINE_GUID(DA_ClassicRegistryGuid, 0xae53722e, 0xc863, 0x11d2, 0x86, 0x59, 0x00, 0xc0, 0x4f, 0xa3, 0x21, 0xa1);
DEFINE_GUID(DA_StackWalkGuid, 0xdef2fe46, 0x7bd6, 0x4b80, 0xbd, 0x94, 0xf5, 0x7f, 0xe2, 0x0d, 0x0c, 0xe3);
#endif

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstddef>
#include <cstdio>
#include <cstring>
#include <deque>
#include <exception>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <initializer_list>
#include <memory>
#include <new>
#include <optional>
#include <sstream>
#include <string>
#include <thread>
#include <unordered_map>
#include <unordered_set>
#include <vector>

namespace {

thread_local std::string g_last_error;

int32_t fail(DA_EtwStatus status, std::string message) noexcept {
    g_last_error = std::move(message);
    return static_cast<int32_t>(status);
}

int32_t fail_unknown() noexcept {
    return fail(DA_ETW_ERR_INTERNAL, "unknown native exception");
}

template <typename Fn>
int32_t guard(Fn&& fn) noexcept {
    try {
        return fn();
    } catch (const std::bad_alloc&) {
        return fail(DA_ETW_ERR_INTERNAL, "native allocation failed");
    } catch (const std::exception& error) {
        return fail(DA_ETW_ERR_INTERNAL, error.what());
    } catch (...) {
        return fail_unknown();
    }
}

#ifdef _WIN32
std::string win32_message(ULONG error) {
    wchar_t buffer[256] = {};
    const auto written = FormatMessageW(
        FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
        nullptr,
        error,
        MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
        buffer,
        static_cast<DWORD>(sizeof(buffer)),
        nullptr);
    std::string message = "Win32 error ";
    message += std::to_string(error);
    if (written > 0) {
        const int utf8_len = WideCharToMultiByte(
            CP_UTF8,
            0,
            buffer,
            static_cast<int>(written),
            nullptr,
            0,
            nullptr,
            nullptr);
        std::string text;
        if (utf8_len > 0) {
            text.resize(static_cast<size_t>(utf8_len));
            WideCharToMultiByte(
                CP_UTF8,
                0,
                buffer,
                static_cast<int>(written),
                text.data(),
                utf8_len,
                nullptr,
                nullptr);
        }
        message += ": ";
        message += text;
        while (!message.empty() && (message.back() == '\r' || message.back() == '\n')) {
            message.pop_back();
        }
    }
    return message;
}

int32_t fail_win32(const char* operation, ULONG error) noexcept {
    std::string message = operation;
    message += " failed: ";
    message += win32_message(error);
    return fail(DA_ETW_ERR_INTERNAL, std::move(message));
}

int32_t fail_hresult(const char* operation, HRESULT hr) noexcept {
    std::string message = operation;
    message += " failed: HRESULT 0x";
    char buffer[16] = {};
    std::snprintf(buffer, sizeof(buffer), "%08lX", static_cast<unsigned long>(hr));
    message += buffer;
    return fail(DA_ETW_ERR_INTERNAL, std::move(message));
}

std::string wide_to_utf8(const wchar_t* value) {
    if (value == nullptr || value[0] == L'\0') {
        return {};
    }
    const int utf8_len = WideCharToMultiByte(CP_UTF8, 0, value, -1, nullptr, 0, nullptr, nullptr);
    if (utf8_len <= 1) {
        return {};
    }
    std::string text(static_cast<size_t>(utf8_len - 1), '\0');
    WideCharToMultiByte(CP_UTF8, 0, value, -1, text.data(), utf8_len, nullptr, nullptr);
    return text;
}

std::wstring utf8_to_wide(const char* value) {
    if (value == nullptr || value[0] == '\0') {
        return {};
    }
    const int wide_len = MultiByteToWideChar(CP_UTF8, 0, value, -1, nullptr, 0);
    if (wide_len <= 1) {
        return {};
    }
    std::wstring text(static_cast<size_t>(wide_len - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, value, -1, text.data(), wide_len);
    return text;
}

std::string json_escape(const std::string& value) {
    std::string escaped;
    escaped.reserve(value.size() + 8);
    for (const unsigned char ch : value) {
        switch (ch) {
        case '\\':
            escaped += "\\\\";
            break;
        case '"':
            escaped += "\\\"";
            break;
        case '\b':
            escaped += "\\b";
            break;
        case '\f':
            escaped += "\\f";
            break;
        case '\n':
            escaped += "\\n";
            break;
        case '\r':
            escaped += "\\r";
            break;
        case '\t':
            escaped += "\\t";
            break;
        default:
            if (ch < 0x20) {
                char buffer[7] = {};
                std::snprintf(buffer, sizeof(buffer), "\\u%04x", ch);
                escaped += buffer;
            } else {
                escaped.push_back(static_cast<char>(ch));
            }
            break;
        }
    }
    return escaped;
}

std::string guid_to_string(const GUID& guid) {
    char buffer[39] = {};
    std::snprintf(
        buffer,
        sizeof(buffer),
        "{%08lX-%04X-%04X-%02X%02X-%02X%02X%02X%02X%02X%02X}",
        static_cast<unsigned long>(guid.Data1),
        guid.Data2,
        guid.Data3,
        guid.Data4[0],
        guid.Data4[1],
        guid.Data4[2],
        guid.Data4[3],
        guid.Data4[4],
        guid.Data4[5],
        guid.Data4[6],
        guid.Data4[7]);
    return buffer;
}

std::string to_lower_ascii(std::string value) {
    std::transform(value.begin(), value.end(), value.begin(), [](unsigned char ch) {
        return static_cast<char>(std::tolower(ch));
    });
    return value;
}

bool contains_any(const std::string& haystack, std::initializer_list<const char*> needles) {
    for (const char* needle : needles) {
        if (haystack.find(needle) != std::string::npos) {
            return true;
        }
    }
    return false;
}

bool preset_enabled(uint32_t preset_flags, const char* category) {
    if (std::strcmp(category, "process") == 0) {
        return (preset_flags & DA_ETW_PRESET_PROCESS) != 0;
    }
    if (std::strcmp(category, "thread") == 0) {
        return (preset_flags & DA_ETW_PRESET_THREAD) != 0;
    }
    if (std::strcmp(category, "image") == 0) {
        return (preset_flags & DA_ETW_PRESET_IMAGE) != 0;
    }
    if (std::strcmp(category, "file") == 0) {
        return (preset_flags & DA_ETW_PRESET_FILE) != 0;
    }
    if (std::strcmp(category, "registry") == 0) {
        return (preset_flags & DA_ETW_PRESET_REGISTRY) != 0;
    }
    if (std::strcmp(category, "network") == 0) {
        return (preset_flags & DA_ETW_PRESET_NETWORK) != 0;
    }
    return false;
}

struct EventNames final {
    std::string provider;
    std::string task;
    std::string opcode;
};

struct EventMetadata final {
    EventNames names;
    std::optional<uint32_t> process_pid;
    std::optional<uint32_t> parent_pid;
    std::unordered_map<std::string, uint64_t> numeric_fields;
    std::unordered_map<std::string, std::string> string_fields;
};

struct ModuleInfo final {
    uint64_t base = 0;
    uint64_t size = 0;
    std::string path;
    std::string name;
};

struct StackTraceRuntimeStatus final {
    bool requested = true;
    bool provider_stack_enabled = false;
    uint32_t provider_stack_warning_count = 0;
    bool kernel_stack_enabled = false;
    uint32_t kernel_stack_warning_count = 0;

    bool enabled() const {
        return provider_stack_enabled || kernel_stack_enabled;
    }
};

std::string trace_info_string(PTRACE_EVENT_INFO info, ULONG offset) {
    if (info == nullptr || offset == 0) {
        return {};
    }
    const auto* bytes = reinterpret_cast<const unsigned char*>(info);
    const auto* text = reinterpret_cast<const wchar_t*>(bytes + offset);
    return wide_to_utf8(text);
}

std::optional<uint64_t> read_numeric_property(PEVENT_RECORD record, const wchar_t* property_name) {
    if (property_name == nullptr || property_name[0] == L'\0') {
        return std::nullopt;
    }

    PROPERTY_DATA_DESCRIPTOR descriptor = {};
    descriptor.PropertyName = reinterpret_cast<ULONGLONG>(property_name);
    ULONG property_size = 0;
    ULONG status = TdhGetPropertySize(record, 0, nullptr, 1, &descriptor, &property_size);
    if (status != ERROR_SUCCESS || property_size == 0 || property_size > sizeof(uint64_t)) {
        return std::nullopt;
    }

    uint64_t value = 0;
    status = TdhGetProperty(
        record,
        0,
        nullptr,
        1,
        &descriptor,
        property_size,
        reinterpret_cast<PBYTE>(&value));
    if (status != ERROR_SUCCESS) {
        return std::nullopt;
    }
    return value;
}

std::optional<std::string> read_string_property(
    PEVENT_RECORD record,
    const wchar_t* property_name,
    USHORT in_type) {
    if (property_name == nullptr || property_name[0] == L'\0') {
        return std::nullopt;
    }
    if (in_type != TDH_INTYPE_UNICODESTRING && in_type != TDH_INTYPE_ANSISTRING) {
        return std::nullopt;
    }

    PROPERTY_DATA_DESCRIPTOR descriptor = {};
    descriptor.PropertyName = reinterpret_cast<ULONGLONG>(property_name);
    ULONG property_size = 0;
    ULONG status = TdhGetPropertySize(record, 0, nullptr, 1, &descriptor, &property_size);
    if (status != ERROR_SUCCESS || property_size == 0 || property_size > 64 * 1024) {
        return std::nullopt;
    }

    std::vector<unsigned char> buffer(property_size + (in_type == TDH_INTYPE_UNICODESTRING ? sizeof(wchar_t) : 1));
    status = TdhGetProperty(
        record,
        0,
        nullptr,
        1,
        &descriptor,
        property_size,
        buffer.data());
    if (status != ERROR_SUCCESS) {
        return std::nullopt;
    }

    if (in_type == TDH_INTYPE_UNICODESTRING) {
        const auto* text = reinterpret_cast<const wchar_t*>(buffer.data());
        return wide_to_utf8(text);
    }

    const auto* text = reinterpret_cast<const char*>(buffer.data());
    return std::string(text);
}

bool is_process_pid_field(const std::string& name) {
    const std::string lower = to_lower_ascii(name);
    return !contains_any(lower, {"parent"}) &&
           (lower == "pid" || lower == "processid" || lower == "process_id" ||
            lower == "process id" || lower == "process");
}

bool is_parent_pid_field(const std::string& name) {
    const std::string lower = to_lower_ascii(name);
    return contains_any(lower, {"parent"}) &&
           contains_any(lower, {"pid", "processid", "process id", "process"});
}

std::optional<std::string> first_matching_string(
    const EventMetadata& metadata,
    std::initializer_list<const char*> needles) {
    for (const auto& [name, value] : metadata.string_fields) {
        const std::string lower = to_lower_ascii(name);
        if (contains_any(lower, needles) && !value.empty()) {
            return value;
        }
    }
    return std::nullopt;
}

std::optional<uint64_t> first_matching_number(
    const EventMetadata& metadata,
    std::initializer_list<const char*> needles) {
    for (const auto& [name, value] : metadata.numeric_fields) {
        const std::string lower = to_lower_ascii(name);
        if (contains_any(lower, needles)) {
            return value;
        }
    }
    return std::nullopt;
}

std::string hex_u64(uint64_t value, int min_width = 0) {
    std::ostringstream out;
    out << "0x" << std::hex << std::nouppercase;
    if (min_width > 0) {
        out << std::setw(min_width) << std::setfill('0');
    }
    out << value;
    return out.str();
}

std::string format_address(uint64_t address) {
    return hex_u64(address, address > 0xffffffffULL ? 16 : 8);
}

std::string file_name_from_path(const std::string& path) {
    const size_t slash = path.find_last_of("\\/");
    if (slash == std::string::npos) {
        return path;
    }
    return path.substr(slash + 1);
}

EventMetadata event_metadata(PEVENT_RECORD record) {
    EventMetadata metadata;
    ULONG buffer_size = 0;
    ULONG status = TdhGetEventInformation(record, 0, nullptr, nullptr, &buffer_size);
    if (status != ERROR_INSUFFICIENT_BUFFER || buffer_size == 0) {
        return metadata;
    }
    std::vector<unsigned char> buffer(buffer_size);
    auto* info = reinterpret_cast<PTRACE_EVENT_INFO>(buffer.data());
    status = TdhGetEventInformation(record, 0, nullptr, info, &buffer_size);
    if (status != ERROR_SUCCESS) {
        return metadata;
    }
    metadata.names = EventNames{
        trace_info_string(info, info->ProviderNameOffset),
        trace_info_string(info, info->TaskNameOffset),
        trace_info_string(info, info->OpcodeNameOffset),
    };

    const ULONG top_level_count = std::min(info->TopLevelPropertyCount, info->PropertyCount);
    for (ULONG index = 0; index < top_level_count; ++index) {
        const auto& property = info->EventPropertyInfoArray[index];
        if ((property.Flags & PropertyStruct) != 0) {
            continue;
        }
        const auto* bytes = reinterpret_cast<const unsigned char*>(info);
        const auto* property_name_w =
            reinterpret_cast<const wchar_t*>(bytes + property.NameOffset);
        const std::string property_name = wide_to_utf8(property_name_w);
        if (auto text = read_string_property(record, property_name_w, property.nonStructType.InType);
            text.has_value()) {
            metadata.string_fields.emplace(property_name, *text);
        }
        auto value = read_numeric_property(record, property_name_w);
        if (!value.has_value()) {
            continue;
        }
        metadata.numeric_fields.emplace(property_name, *value);
        if (!metadata.process_pid.has_value() && is_process_pid_field(property_name)) {
            metadata.process_pid = static_cast<uint32_t>(*value);
        }
        if (!metadata.parent_pid.has_value() && is_parent_pid_field(property_name)) {
            metadata.parent_pid = static_cast<uint32_t>(*value);
        }
    }
    return metadata;
}

const char* event_category(const EventNames& names) {
    const std::string task_opcode = to_lower_ascii(names.task + " " + names.opcode);
    const std::string text = to_lower_ascii(names.provider + " " + names.task + " " + names.opcode);
    const char* category = nullptr;
    if (contains_any(task_opcode, {"thread"})) {
        category = "thread";
    } else if (contains_any(task_opcode, {"image", "imageload"})) {
        category = "image";
    } else if (contains_any(text, {"process"})) {
        category = "process";
    } else if (contains_any(text, {"registry", "reg"})) {
        category = "registry";
    } else if (contains_any(text, {"tcp", "udp", "network"})) {
        category = "network";
    } else if (contains_any(text, {"file", "disk"})) {
        category = "file";
    }
    return category;
}

const char* classify_event(const EventNames& names, uint32_t preset_flags) {
    const char* category = event_category(names);
    if (category == nullptr || !preset_enabled(preset_flags, category)) {
        return nullptr;
    }
    return category;
}

long long unix_millis_from_etw_timestamp(LARGE_INTEGER timestamp) {
    constexpr long long windows_epoch_delta = 116444736000000000LL;
    if (timestamp.QuadPart <= windows_epoch_delta) {
        return 0;
    }
    return (timestamp.QuadPart - windows_epoch_delta) / 10000;
}

long long unix_millis_from_etw_timestamp_value(long long timestamp) {
    LARGE_INTEGER value = {};
    value.QuadPart = timestamp;
    return unix_millis_from_etw_timestamp(value);
}

constexpr uint32_t DA_FILE_IO_NAME = 0;
constexpr uint32_t DA_FILE_IO_FILE_CREATE_NAME = 32;
constexpr uint32_t DA_FILE_IO_FILE_DELETE_NAME = 35;
constexpr uint32_t DA_FILE_IO_RUNDOWN = 36;
constexpr uint32_t DA_FILE_IO_CREATE = 64;
constexpr uint32_t DA_FILE_IO_CLEANUP = 65;
constexpr uint32_t DA_FILE_IO_CLOSE = 66;
constexpr uint32_t DA_FILE_IO_READ = 67;
constexpr uint32_t DA_FILE_IO_WRITE = 68;
constexpr uint32_t DA_FILE_IO_SET_INFO = 69;
constexpr uint32_t DA_FILE_IO_DELETE = 70;
constexpr uint32_t DA_FILE_IO_RENAME = 71;
constexpr uint32_t DA_FILE_IO_DIR_ENUM = 72;
constexpr uint32_t DA_FILE_IO_FLUSH = 73;
constexpr uint32_t DA_FILE_IO_QUERY_INFO = 74;
constexpr uint32_t DA_FILE_IO_FS_CONTROL = 75;
constexpr uint32_t DA_FILE_IO_OP_END = 76;
constexpr uint32_t DA_FILE_IO_DIR_NOTIFY = 77;
constexpr uint32_t DA_STACK_WALK_OPCODE = 32;
constexpr size_t DA_MAX_PENDING_STACK_TIMESTAMPS = 4096;
constexpr size_t DA_MAX_PENDING_STACKS_PER_TIMESTAMP = 32;

struct ExtractionQuality final {
    uint32_t stack_frames_total = 0;
    uint32_t stack_frames_resolved = 0;
    uint32_t stack_frames_unresolved = 0;
    uint32_t file_path_resolved = 0;
    uint32_t file_path_unresolved = 0;
    uint32_t matched_op_end = 0;
    uint32_t unmatched_op_end = 0;
    uint32_t incomplete_io = 0;
    uint32_t reused_irp = 0;
    uint32_t dropped_stack_walk = 0;
};

struct DecodedStackWalkEvent final {
    long long event_timestamp = 0;
    uint32_t stack_process = 0;
    uint32_t stack_thread = 0;
    std::vector<uint64_t> addresses;
};

struct FileEventRecord final {
    EventMetadata metadata;
    GUID provider = {};
    uint16_t event_id = 0;
    uint8_t version = 0;
    uint8_t opcode = 0;
    uint64_t keywords = 0;
    uint16_t user_data_length = 0;
    long long event_timestamp = 0;
    uint32_t pid = 0;
    uint32_t tid = 0;
    std::string event_type;
    std::optional<std::string> path;
    std::optional<std::string> path_source;
    std::optional<uint64_t> file_object;
    std::optional<uint64_t> file_key;
    std::optional<uint64_t> irp_ptr;
    std::optional<uint64_t> offset;
    std::optional<uint64_t> io_size;
    std::optional<uint32_t> io_flags;
    std::optional<uint32_t> create_options;
    std::optional<uint32_t> file_attributes;
    std::optional<uint32_t> share_access;
    std::optional<uint32_t> info_class;
    std::optional<uint64_t> extra_info;
    std::optional<uint32_t> nt_status;
    std::vector<uint64_t> stack_addresses;
    std::optional<uint32_t> completion_pid;
    std::optional<uint32_t> completion_tid;
    std::optional<uint32_t> completion_sequence;
};

struct ExtractionContext final {
    std::filesystem::path events_dir;
    uint32_t preset_flags = 0;
    bool has_root_pid = false;
    uint32_t root_pid = 0;
    uint32_t events_written = 0;
    uint32_t skipped_events = 0;
    std::unordered_set<std::string> files_written;
    std::unordered_set<uint32_t> process_tree_pids;
    std::unordered_map<uint32_t, std::vector<ModuleInfo>> modules_by_pid;
    ExtractionQuality quality;
    uint32_t file_io_raw_sequence = 0;
    std::unordered_map<uint64_t, FileEventRecord> pending_file_irps;
    std::unordered_set<uint64_t> ignored_file_irps;
    std::unordered_map<uint64_t, std::string> file_paths_by_object;
    std::unordered_map<uint64_t, std::string> file_paths_by_key;
    std::unordered_map<long long, std::vector<DecodedStackWalkEvent>> pending_stacks_by_timestamp;
    std::deque<long long> pending_stack_timestamp_order;
};

void initialize_process_tree(ExtractionContext& context) {
    if (context.has_root_pid) {
        context.process_tree_pids.insert(context.root_pid);
    }
}

bool process_tree_allows_event(ExtractionContext& context, const EventMetadata& metadata, const char* category, uint32_t event_pid) {
    if (!context.has_root_pid) {
        return true;
    }

    if (std::strcmp(category, "process") == 0) {
        const uint32_t process_pid = metadata.process_pid.value_or(event_pid);
        const bool parent_in_tree =
            metadata.parent_pid.has_value() &&
            context.process_tree_pids.find(*metadata.parent_pid) != context.process_tree_pids.end();
        const bool process_in_tree = context.process_tree_pids.find(process_pid) != context.process_tree_pids.end();
        if (process_in_tree || parent_in_tree) {
            context.process_tree_pids.insert(process_pid);
            return true;
        }
        return false;
    }

    return context.process_tree_pids.find(event_pid) != context.process_tree_pids.end();
}

void update_module_map(ExtractionContext& context, const EventMetadata& metadata, const std::string& event_type, uint32_t pid) {
    auto base = first_matching_number(metadata, {"imagebase", "baseaddress", "base"});
    if (!base.has_value()) {
        return;
    }

    auto& modules = context.modules_by_pid[pid];
    const uint64_t image_base = *base;
    const std::string lower_event_type = to_lower_ascii(event_type);
    if (contains_any(lower_event_type, {"unload", "stop", "end"})) {
        modules.erase(
            std::remove_if(
                modules.begin(),
                modules.end(),
                [image_base](const ModuleInfo& module) { return module.base == image_base; }),
            modules.end());
        return;
    }

    const auto image_path = first_matching_string(metadata, {"image", "filename", "file name"});
    ModuleInfo module;
    module.base = image_base;
    module.size = first_matching_number(metadata, {"imagesize", "size"}).value_or(0);
    module.path = image_path.value_or(std::string{});
    module.name = file_name_from_path(module.path);
    if (module.name.empty()) {
        module.name = format_address(module.base);
    }

    modules.erase(
        std::remove_if(
            modules.begin(),
            modules.end(),
            [image_base](const ModuleInfo& existing) { return existing.base == image_base; }),
        modules.end());
    modules.push_back(std::move(module));
}

const ModuleInfo* find_module(const ExtractionContext& context, uint32_t pid, uint64_t address) {
    auto find_in_modules = [address](const std::vector<ModuleInfo>& modules) -> const ModuleInfo* {
        for (const auto& module : modules) {
            if (address < module.base) {
                continue;
            }
            if (module.size != 0 && address - module.base < module.size) {
                return &module;
            }
        }
        return nullptr;
    };

    if (auto it = context.modules_by_pid.find(pid); it != context.modules_by_pid.end()) {
        if (const auto* module = find_in_modules(it->second); module != nullptr) {
            return module;
        }
    }
    if (auto it = context.modules_by_pid.find(0); it != context.modules_by_pid.end()) {
        return find_in_modules(it->second);
    }
    return nullptr;
}

std::optional<uint64_t> exact_number_field(const EventMetadata& metadata, const std::string& name) {
    if (auto it = metadata.numeric_fields.find(name); it != metadata.numeric_fields.end()) {
        return it->second;
    }
    return std::nullopt;
}

std::optional<uint32_t> number_to_u32(std::optional<uint64_t> value) {
    if (!value.has_value()) {
        return std::nullopt;
    }
    return static_cast<uint32_t>(*value);
}

std::vector<uint64_t> stack_addresses(PEVENT_RECORD record) {
    std::vector<uint64_t> addresses;
    if (record == nullptr || record->ExtendedData == nullptr || record->ExtendedDataCount == 0) {
        return addresses;
    }

    for (USHORT index = 0; index < record->ExtendedDataCount; ++index) {
        const auto& item = record->ExtendedData[index];
        if (item.DataPtr == 0 || item.DataSize <= sizeof(ULONG64)) {
            continue;
        }
        if (item.ExtType == EVENT_HEADER_EXT_TYPE_STACK_TRACE64) {
            const auto* stack = reinterpret_cast<const EVENT_EXTENDED_ITEM_STACK_TRACE64*>(
                static_cast<uintptr_t>(item.DataPtr));
            const size_t count = (item.DataSize - sizeof(ULONG64)) / sizeof(ULONG64);
            for (size_t frame = 0; frame < count; ++frame) {
                if (stack->Address[frame] != 0) {
                    addresses.push_back(stack->Address[frame]);
                }
            }
            continue;
        }
        if (item.ExtType == EVENT_HEADER_EXT_TYPE_STACK_TRACE32) {
            const auto* stack = reinterpret_cast<const EVENT_EXTENDED_ITEM_STACK_TRACE32*>(
                static_cast<uintptr_t>(item.DataPtr));
            const size_t count = (item.DataSize - sizeof(ULONG64)) / sizeof(ULONG);
            for (size_t frame = 0; frame < count; ++frame) {
                if (stack->Address[frame] != 0) {
                    addresses.push_back(stack->Address[frame]);
                }
            }
        }
    }
    return addresses;
}

std::optional<DecodedStackWalkEvent> decode_stack_walk_raw_data(PEVENT_RECORD record) {
    if (record == nullptr || record->UserData == nullptr || record->UserDataLength < 16) {
        return std::nullopt;
    }
    const auto* data = static_cast<const unsigned char*>(record->UserData);
    DecodedStackWalkEvent stack;
    std::memcpy(&stack.event_timestamp, data, sizeof(int64_t));
    std::memcpy(&stack.stack_process, data + 8, sizeof(uint32_t));
    std::memcpy(&stack.stack_thread, data + 12, sizeof(uint32_t));
    const size_t remaining = record->UserDataLength - 16;
    const auto* frames = data + 16;
    const size_t pointer_size = remaining % sizeof(uint64_t) == 0 ? sizeof(uint64_t) : sizeof(uint32_t);
    if (pointer_size == sizeof(uint64_t)) {
        for (size_t offset = 0; offset + sizeof(uint64_t) <= remaining; offset += sizeof(uint64_t)) {
            uint64_t address = 0;
            std::memcpy(&address, frames + offset, sizeof(uint64_t));
            if (address != 0) {
                stack.addresses.push_back(address);
            }
        }
    } else {
        for (size_t offset = 0; offset + sizeof(uint32_t) <= remaining; offset += sizeof(uint32_t)) {
            uint32_t address = 0;
            std::memcpy(&address, frames + offset, sizeof(uint32_t));
            if (address != 0) {
                stack.addresses.push_back(address);
            }
        }
    }
    return stack;
}

std::optional<DecodedStackWalkEvent> decode_stack_walk_event(PEVENT_RECORD record, const EventMetadata& metadata) {
    if (record == nullptr || !IsEqualGUID(record->EventHeader.ProviderId, DA_StackWalkGuid) ||
        record->EventHeader.EventDescriptor.Opcode != DA_STACK_WALK_OPCODE) {
        return std::nullopt;
    }

    DecodedStackWalkEvent stack;
    const auto event_timestamp = first_matching_number(metadata, {"eventtimestamp", "event time stamp"});
    const auto stack_process = first_matching_number(metadata, {"stackprocess"});
    const auto stack_thread = first_matching_number(metadata, {"stackthread"});
    if (event_timestamp.has_value() && stack_process.has_value() && stack_thread.has_value()) {
        stack.event_timestamp = static_cast<long long>(*event_timestamp);
        stack.stack_process = static_cast<uint32_t>(*stack_process);
        stack.stack_thread = static_cast<uint32_t>(*stack_thread);
        for (uint32_t index = 1; index <= 192; ++index) {
            const std::string field = "Stack" + std::to_string(index);
            auto address = exact_number_field(metadata, field);
            if (!address.has_value()) {
                break;
            }
            if (*address != 0) {
                stack.addresses.push_back(*address);
            }
        }
        return stack;
    }
    return decode_stack_walk_raw_data(record);
}

void append_stack_addresses(std::vector<uint64_t>& target, const std::vector<uint64_t>& source) {
    for (const uint64_t address : source) {
        if (address != 0 && std::find(target.begin(), target.end(), address) == target.end()) {
            target.push_back(address);
        }
    }
}

std::vector<uint64_t> take_matching_pending_stack(ExtractionContext& context, long long timestamp, uint32_t pid, uint32_t tid) {
    auto it = context.pending_stacks_by_timestamp.find(timestamp);
    if (it == context.pending_stacks_by_timestamp.end()) {
        return {};
    }
    auto& pending = it->second;
    auto exact = std::find_if(pending.begin(), pending.end(), [pid, tid](const DecodedStackWalkEvent& stack) {
        return stack.stack_process == pid && stack.stack_thread == tid;
    });
    auto selected = exact != pending.end()
                        ? exact
                        : std::find_if(pending.begin(), pending.end(), [pid](const DecodedStackWalkEvent& stack) {
                              return stack.stack_process == pid;
                          });
    if (selected == pending.end()) {
        return {};
    }
    auto addresses = selected->addresses;
    pending.erase(selected);
    if (pending.empty()) {
        context.pending_stacks_by_timestamp.erase(it);
    }
    return addresses;
}

void cache_pending_stack(ExtractionContext& context, DecodedStackWalkEvent stack) {
    if (context.has_root_pid && context.process_tree_pids.find(stack.stack_process) == context.process_tree_pids.end()) {
        context.quality.dropped_stack_walk += 1;
        return;
    }
    if (context.pending_stacks_by_timestamp.find(stack.event_timestamp) == context.pending_stacks_by_timestamp.end()) {
        if (context.pending_stack_timestamp_order.size() >= DA_MAX_PENDING_STACK_TIMESTAMPS) {
            const long long oldest = context.pending_stack_timestamp_order.front();
            context.pending_stack_timestamp_order.pop_front();
            if (auto removed = context.pending_stacks_by_timestamp.find(oldest);
                removed != context.pending_stacks_by_timestamp.end()) {
                context.quality.dropped_stack_walk += static_cast<uint32_t>(removed->second.size());
                context.pending_stacks_by_timestamp.erase(removed);
            }
        }
        context.pending_stack_timestamp_order.push_back(stack.event_timestamp);
    }
    auto& pending = context.pending_stacks_by_timestamp[stack.event_timestamp];
    if (pending.size() >= DA_MAX_PENDING_STACKS_PER_TIMESTAMP) {
        context.quality.dropped_stack_walk += 1;
        return;
    }
    pending.push_back(std::move(stack));
}

struct StackFrameInfo final {
    std::string text;
    bool resolved = false;
};

std::vector<StackFrameInfo> stack_frames_from_addresses(
    ExtractionContext& context,
    const std::vector<uint64_t>& addresses,
    uint32_t pid) {
    std::vector<StackFrameInfo> frames;
    for (const uint64_t address : addresses) {
        if (const auto* module = find_module(context, pid, address); module != nullptr && address >= module->base) {
            frames.push_back(StackFrameInfo{module->name + "+" + hex_u64(address - module->base), true});
            context.quality.stack_frames_resolved += 1;
        } else {
            frames.push_back(StackFrameInfo{format_address(address), false});
            context.quality.stack_frames_unresolved += 1;
        }
        context.quality.stack_frames_total += 1;
    }
    std::reverse(frames.begin(), frames.end());
    return frames;
}

std::vector<StackFrameInfo> stack_frames(ExtractionContext& context, PEVENT_RECORD record, uint32_t pid) {
    std::vector<uint64_t> addresses = stack_addresses(record);
    append_stack_addresses(
        addresses,
        take_matching_pending_stack(context, record->EventHeader.TimeStamp.QuadPart, pid, record->EventHeader.ThreadId));
    return stack_frames_from_addresses(context, addresses, pid);
}

std::vector<std::string> stack_frame_text(const std::vector<StackFrameInfo>& frame_infos) {
    std::vector<std::string> frames;
    frames.reserve(frame_infos.size());
    for (const auto& frame : frame_infos) {
        frames.push_back(frame.text);
    }
    return frames;
}

std::string json_string_or_null(const std::optional<std::string>& value) {
    if (!value.has_value()) {
        return "null";
    }
    return "\"" + json_escape(*value) + "\"";
}

std::string json_number_or_null(const std::optional<uint64_t>& value) {
    if (!value.has_value()) {
        return "null";
    }
    return std::to_string(*value);
}

std::string json_u32_or_null(const std::optional<uint32_t>& value) {
    if (!value.has_value()) {
        return "null";
    }
    return std::to_string(*value);
}

std::optional<const char*> known_file_io_event_type(uint32_t opcode) {
    switch (opcode) {
    case DA_FILE_IO_NAME:
        return "file_name";
    case DA_FILE_IO_FILE_CREATE_NAME:
        return "file_create_name";
    case DA_FILE_IO_FILE_DELETE_NAME:
        return "file_delete_name";
    case DA_FILE_IO_RUNDOWN:
        return "file_rundown";
    case DA_FILE_IO_CREATE:
        return "create";
    case DA_FILE_IO_CLEANUP:
        return "cleanup";
    case DA_FILE_IO_CLOSE:
        return "close";
    case DA_FILE_IO_READ:
        return "read";
    case DA_FILE_IO_WRITE:
        return "write";
    case DA_FILE_IO_SET_INFO:
        return "set_info";
    case DA_FILE_IO_DELETE:
        return "delete";
    case DA_FILE_IO_RENAME:
        return "rename";
    case DA_FILE_IO_DIR_ENUM:
        return "dir_enum";
    case DA_FILE_IO_FLUSH:
        return "flush";
    case DA_FILE_IO_QUERY_INFO:
        return "query_info";
    case DA_FILE_IO_FS_CONTROL:
        return "fs_control";
    case DA_FILE_IO_OP_END:
        return "op_end";
    case DA_FILE_IO_DIR_NOTIFY:
        return "dir_notify";
    default:
        return std::nullopt;
    }
}

bool is_known_file_io_event(PEVENT_RECORD record, const char* raw_category) {
    if (record == nullptr || raw_category == nullptr || std::strcmp(raw_category, "file") != 0) {
        return false;
    }
    const uint32_t opcode = record->EventHeader.EventDescriptor.Opcode;
    return known_file_io_event_type(opcode).has_value() &&
           (IsEqualGUID(record->EventHeader.ProviderId, DA_ClassicFileIoGuid) ||
            IsEqualGUID(record->EventHeader.ProviderId, DA_KernelFileProviderGuid));
}

void write_stack_json(std::ofstream& out, const std::vector<StackFrameInfo>& frames) {
    if (frames.empty()) {
        return;
    }
    out << ",\"stack\":{\"frames\":[";
    for (size_t index = 0; index < frames.size(); ++index) {
        if (index != 0) {
            out << ",";
        }
        out << "\"" << json_escape(frames[index].text) << "\"";
    }
    out << "]}";
}

std::optional<std::string> direct_file_path(const EventMetadata& metadata, uint32_t opcode) {
    if (opcode == DA_FILE_IO_CREATE) {
        return first_matching_string(metadata, {"openpath", "file name", "filename", "path"});
    }
    if (opcode == DA_FILE_IO_NAME || opcode == DA_FILE_IO_FILE_CREATE_NAME ||
        opcode == DA_FILE_IO_FILE_DELETE_NAME || opcode == DA_FILE_IO_RUNDOWN ||
        opcode == DA_FILE_IO_DIR_ENUM || opcode == DA_FILE_IO_DIR_NOTIFY) {
        return first_matching_string(metadata, {"filename", "file name", "openpath", "path"});
    }
    return first_matching_string(metadata, {"filename", "file name", "path"});
}

std::optional<std::string> direct_file_path_source(uint32_t opcode, const std::optional<std::string>& path) {
    if (!path.has_value()) {
        return std::nullopt;
    }
    if (opcode == DA_FILE_IO_CREATE) {
        return "open_path";
    }
    return "file_name";
}

void cache_file_path(ExtractionContext& context, const FileEventRecord& event) {
    if (!event.path.has_value() || event.path->empty()) {
        return;
    }
    if (event.file_object.has_value()) {
        context.file_paths_by_object[*event.file_object] = *event.path;
    }
    if (event.file_key.has_value()) {
        context.file_paths_by_key[*event.file_key] = *event.path;
    }
}

void remove_file_path(ExtractionContext& context, const FileEventRecord& event) {
    if (event.file_object.has_value()) {
        context.file_paths_by_object.erase(*event.file_object);
    }
    if (event.file_key.has_value()) {
        context.file_paths_by_key.erase(*event.file_key);
    }
}

void resolve_file_path(ExtractionContext& context, FileEventRecord& event) {
    if (event.path.has_value() && !event.path->empty()) {
        context.quality.file_path_resolved += 1;
        return;
    }
    if (event.file_object.has_value()) {
        if (auto found = context.file_paths_by_object.find(*event.file_object);
            found != context.file_paths_by_object.end()) {
            event.path = found->second;
            event.path_source = "file_object_cache";
            context.quality.file_path_resolved += 1;
            return;
        }
    }
    if (event.file_key.has_value()) {
        if (auto found = context.file_paths_by_key.find(*event.file_key);
            found != context.file_paths_by_key.end()) {
            event.path = found->second;
            event.path_source = "file_key_cache";
            context.quality.file_path_resolved += 1;
            return;
        }
    }
    context.quality.file_path_unresolved += 1;
}

FileEventRecord decoded_file_event(ExtractionContext& context, PEVENT_RECORD record, const EventMetadata& metadata) {
    const uint32_t opcode = record->EventHeader.EventDescriptor.Opcode;
    FileEventRecord event;
    event.metadata = metadata;
    event.provider = record->EventHeader.ProviderId;
    event.event_id = record->EventHeader.EventDescriptor.Id;
    event.version = record->EventHeader.EventDescriptor.Version;
    event.opcode = record->EventHeader.EventDescriptor.Opcode;
    event.keywords = record->EventHeader.EventDescriptor.Keyword;
    event.user_data_length = record->UserDataLength;
    event.event_timestamp = record->EventHeader.TimeStamp.QuadPart;
    event.pid = metadata.process_pid.value_or(record->EventHeader.ProcessId);
    event.tid = record->EventHeader.ThreadId;
    event.event_type = known_file_io_event_type(opcode).value_or("file_event");
    event.path = direct_file_path(metadata, opcode);
    event.path_source = direct_file_path_source(opcode, event.path);
    event.file_object = first_matching_number(metadata, {"fileobject", "fileobj"});
    event.file_key = first_matching_number(metadata, {"filekey", "fileobjectkey"});
    event.irp_ptr = first_matching_number(metadata, {"irpptr", "irp", "irppointer"});
    event.offset = first_matching_number(metadata, {"offset", "byteoffset"});
    event.io_size = first_matching_number(metadata, {"iosize", "size", "transfersize"});
    event.io_flags = number_to_u32(first_matching_number(metadata, {"ioflags", "flags"}));
    event.create_options = number_to_u32(first_matching_number(metadata, {"createoptions"}));
    event.file_attributes = number_to_u32(first_matching_number(metadata, {"fileattributes"}));
    event.share_access = number_to_u32(first_matching_number(metadata, {"shareaccess"}));
    event.info_class = number_to_u32(first_matching_number(metadata, {"infoclass"}));
    event.extra_info = first_matching_number(metadata, {"extrainfo"});
    event.nt_status = number_to_u32(first_matching_number(metadata, {"ntstatus", "status"}));
    if (opcode != DA_FILE_IO_OP_END) {
        event.stack_addresses = stack_addresses(record);
        append_stack_addresses(
            event.stack_addresses,
            take_matching_pending_stack(context, event.event_timestamp, event.pid, event.tid));
    }
    return event;
}

bool file_event_allowed(ExtractionContext& context, const FileEventRecord& event) {
    if (!context.has_root_pid) {
        return true;
    }
    return context.process_tree_pids.find(event.pid) != context.process_tree_pids.end();
}

void write_file_event(ExtractionContext& context, FileEventRecord event) {
    const std::filesystem::path path = context.events_dir / "file.jsonl";
    std::ofstream out(path, std::ios::binary | std::ios::app);
    if (!out) {
        context.skipped_events += 1;
        return;
    }
    const auto frames = stack_frames_from_addresses(context, event.stack_addresses, event.pid);
    out << "{\"schema_version\":1"
        << ",\"timestamp\":{\"unix_millis\":" << unix_millis_from_etw_timestamp_value(event.event_timestamp) << "}"
        << ",\"category\":\"file\""
        << ",\"event_type\":\"" << json_escape(event.event_type) << "\""
        << ",\"pid\":" << event.pid
        << ",\"tid\":" << event.tid
        << ",\"process\":{\"pid\":" << event.pid << ",\"parent_pid\":";
    if (event.metadata.parent_pid.has_value()) {
        out << *event.metadata.parent_pid;
    } else {
        out << "null";
    }
    out << ",\"image_path\":"
        << json_string_or_null(first_matching_string(event.metadata, {"image", "filename", "processname"}))
        << ",\"command_line\":"
        << json_string_or_null(first_matching_string(event.metadata, {"commandline", "command line"}))
        << "}"
        << ",\"file\":{"
        << "\"path\":" << json_string_or_null(event.path)
        << ",\"path_source\":" << json_string_or_null(event.path_source)
        << ",\"operation\":\"" << json_escape(event.event_type) << "\""
        << ",\"status\":" << json_u32_or_null(event.nt_status)
        << ",\"byte_count\":" << json_number_or_null(event.io_size)
        << ",\"file_object\":" << json_number_or_null(event.file_object)
        << ",\"file_key\":" << json_number_or_null(event.file_key)
        << ",\"irp_ptr\":" << json_number_or_null(event.irp_ptr)
        << ",\"offset\":" << json_number_or_null(event.offset)
        << ",\"io_flags\":" << json_u32_or_null(event.io_flags)
        << ",\"create_options\":" << json_u32_or_null(event.create_options)
        << ",\"file_attributes\":" << json_u32_or_null(event.file_attributes)
        << ",\"share_access\":" << json_u32_or_null(event.share_access)
        << ",\"info_class\":" << json_u32_or_null(event.info_class)
        << ",\"extra_info\":" << json_number_or_null(event.extra_info)
        << ",\"completion_pid\":" << json_u32_or_null(event.completion_pid)
        << ",\"completion_tid\":" << json_u32_or_null(event.completion_tid)
        << ",\"completion_sequence\":" << json_u32_or_null(event.completion_sequence)
        << "}";
    write_stack_json(out, frames);
    out
        << ",\"operation_id\":null"
        << ",\"artifact_id\":null"
        << ",\"etw\":{"
        << "\"provider\":\"" << guid_to_string(event.provider) << "\""
        << ",\"provider_name\":\"" << json_escape(event.metadata.names.provider) << "\""
        << ",\"task\":\"" << json_escape(event.metadata.names.task) << "\""
        << ",\"event_id\":" << event.event_id
        << ",\"version\":" << static_cast<unsigned int>(event.version)
        << ",\"opcode\":" << static_cast<unsigned int>(event.opcode)
        << ",\"opcode_name\":\"" << json_escape(event.metadata.names.opcode) << "\""
        << ",\"keywords\":" << event.keywords
        << ",\"raw\":{\"user_data_length\":" << event.user_data_length << ",\"numeric_fields\":{";
    bool first = true;
    for (const auto& [name, value] : event.metadata.numeric_fields) {
        if (!first) {
            out << ",";
        }
        first = false;
        out << "\"" << json_escape(name) << "\":" << value;
    }
    out << "},\"string_fields\":{";
    first = true;
    for (const auto& [name, value] : event.metadata.string_fields) {
        if (!first) {
            out << ",";
        }
        first = false;
        out << "\"" << json_escape(name) << "\":\"" << json_escape(value) << "\"";
    }
    out << "}}}}\n";
    context.files_written.insert("file");
    context.events_written += 1;
}

bool attach_stack_to_pending_file(ExtractionContext& context, const DecodedStackWalkEvent& stack) {
    FileEventRecord* fallback = nullptr;
    for (auto& [_, event] : context.pending_file_irps) {
        if (event.event_timestamp != stack.event_timestamp || event.pid != stack.stack_process) {
            continue;
        }
        if (event.tid == stack.stack_thread) {
            append_stack_addresses(event.stack_addresses, stack.addresses);
            return true;
        }
        fallback = &event;
    }
    if (fallback != nullptr) {
        append_stack_addresses(fallback->stack_addresses, stack.addresses);
        return true;
    }
    return false;
}

void process_stack_walk_event(ExtractionContext& context, DecodedStackWalkEvent stack) {
    if (!attach_stack_to_pending_file(context, stack)) {
        cache_pending_stack(context, std::move(stack));
    }
}

void process_file_io_event(ExtractionContext& context, PEVENT_RECORD record, const EventMetadata& metadata) {
    if (!preset_enabled(context.preset_flags, "file")) {
        context.skipped_events += 1;
        return;
    }
    FileEventRecord event = decoded_file_event(context, record, metadata);
    const uint32_t opcode = record->EventHeader.EventDescriptor.Opcode;
    context.file_io_raw_sequence += 1;

    if (opcode == DA_FILE_IO_OP_END) {
        const auto irp = event.irp_ptr;
        if (!irp.has_value()) {
            context.quality.unmatched_op_end += 1;
            return;
        }
        auto pending = context.pending_file_irps.find(*irp);
        if (pending == context.pending_file_irps.end()) {
            if (context.ignored_file_irps.erase(*irp) == 0) {
                context.quality.unmatched_op_end += 1;
            }
            return;
        }
        FileEventRecord merged = std::move(pending->second);
        context.pending_file_irps.erase(pending);
        if (event.extra_info.has_value()) {
            merged.extra_info = event.extra_info;
        }
        if (event.nt_status.has_value()) {
            merged.nt_status = event.nt_status;
        }
        merged.completion_pid = event.pid;
        merged.completion_tid = event.tid;
        merged.completion_sequence = context.file_io_raw_sequence;
        context.quality.matched_op_end += 1;
        write_file_event(context, std::move(merged));
        return;
    }

    if (!file_event_allowed(context, event)) {
        context.skipped_events += 1;
        return;
    }
    cache_file_path(context, event);
    resolve_file_path(context, event);

    if (opcode == DA_FILE_IO_CLOSE) {
        if (event.irp_ptr.has_value()) {
            if (context.pending_file_irps.erase(*event.irp_ptr) > 0) {
                context.quality.reused_irp += 1;
            }
            context.ignored_file_irps.insert(*event.irp_ptr);
        }
        write_file_event(context, event);
        remove_file_path(context, event);
        return;
    }

    const bool invalidates_file_object = opcode == DA_FILE_IO_CLEANUP;
    const std::optional<uint64_t> pending_irp = event.irp_ptr;
    const std::optional<uint64_t> cleanup_file_object = event.file_object;
    const std::optional<uint64_t> cleanup_file_key = event.file_key;
    if (event.irp_ptr.has_value()) {
        if (auto existing = context.pending_file_irps.find(*event.irp_ptr);
            existing != context.pending_file_irps.end()) {
            context.quality.reused_irp += 1;
            write_file_event(context, std::move(existing->second));
            context.pending_file_irps.erase(existing);
        }
        context.pending_file_irps.emplace(*event.irp_ptr, std::move(event));
    } else {
        write_file_event(context, std::move(event));
    }
    if (invalidates_file_object) {
        if (pending_irp.has_value()) {
            if (auto pending = context.pending_file_irps.find(*pending_irp);
                pending != context.pending_file_irps.end()) {
                remove_file_path(context, pending->second);
            }
        }
        if (cleanup_file_object.has_value()) {
            context.file_paths_by_object.erase(*cleanup_file_object);
        }
        if (cleanup_file_key.has_value()) {
            context.file_paths_by_key.erase(*cleanup_file_key);
        }
    }
}

void flush_pending_file_events(ExtractionContext& context) {
    context.quality.incomplete_io += static_cast<uint32_t>(context.pending_file_irps.size());
    std::vector<FileEventRecord> pending;
    pending.reserve(context.pending_file_irps.size());
    for (auto& [_, event] : context.pending_file_irps) {
        pending.push_back(std::move(event));
    }
    context.pending_file_irps.clear();
    for (auto& event : pending) {
        write_file_event(context, std::move(event));
    }
}

void write_category_fields(
    std::ofstream& out,
    const EventMetadata& metadata,
    const char* category,
    const std::string& event_type) {
    if (std::strcmp(category, "process") == 0) {
        out << ",\"process_event\":{"
            << "\"image_path\":" << json_string_or_null(first_matching_string(metadata, {"image", "filename", "processname"}))
            << ",\"command_line\":" << json_string_or_null(first_matching_string(metadata, {"commandline", "command line"}))
            << ",\"exit_code\":" << json_number_or_null(first_matching_number(metadata, {"exit"}))
            << "}";
        return;
    }
    if (std::strcmp(category, "thread") == 0) {
        out << ",\"thread\":{"
            << "\"start_address\":" << json_number_or_null(first_matching_number(metadata, {"startaddress", "win32startaddr"}))
            << ",\"exit_status\":" << json_number_or_null(first_matching_number(metadata, {"exit", "status"}))
            << "}";
        return;
    }
    if (std::strcmp(category, "image") == 0) {
        out << ",\"image\":{"
            << "\"image_path\":" << json_string_or_null(first_matching_string(metadata, {"image", "filename", "file name"}))
            << ",\"base_address\":" << json_number_or_null(first_matching_number(metadata, {"imagebase", "baseaddress", "base"}))
            << ",\"size\":" << json_number_or_null(first_matching_number(metadata, {"imagesize", "size"}))
            << ",\"checksum\":" << json_number_or_null(first_matching_number(metadata, {"checksum"}))
            << "}";
        return;
    }
    if (std::strcmp(category, "file") == 0) {
        out << ",\"file\":{"
            << "\"path\":" << json_string_or_null(first_matching_string(metadata, {"filename", "file name", "path", "name"}))
            << ",\"operation\":\"" << json_escape(event_type) << "\""
            << ",\"status\":" << json_number_or_null(first_matching_number(metadata, {"status"}))
            << ",\"byte_count\":" << json_number_or_null(first_matching_number(metadata, {"size", "length", "bytes"}))
            << "}";
        return;
    }
    if (std::strcmp(category, "registry") == 0) {
        out << ",\"registry\":{"
            << "\"key_path\":" << json_string_or_null(first_matching_string(metadata, {"key", "path", "object"}))
            << ",\"value_name\":" << json_string_or_null(first_matching_string(metadata, {"value"}))
            << ",\"value_type\":" << json_number_or_null(first_matching_number(metadata, {"type"}))
            << ",\"status\":" << json_number_or_null(first_matching_number(metadata, {"status"}))
            << "}";
        return;
    }
    if (std::strcmp(category, "network") == 0) {
        out << ",\"network\":{"
            << "\"protocol\":" << json_string_or_null(first_matching_string(metadata, {"protocol"}))
            << ",\"local_endpoint\":" << json_string_or_null(first_matching_string(metadata, {"local"}))
            << ",\"remote_endpoint\":" << json_string_or_null(first_matching_string(metadata, {"remote", "dest", "destination"}))
            << ",\"status\":" << json_number_or_null(first_matching_number(metadata, {"status"}))
            << ",\"byte_count\":" << json_number_or_null(first_matching_number(metadata, {"size", "length", "bytes"}))
            << "}";
    }
}

void write_extracted_event(ExtractionContext& context, PEVENT_RECORD record, const EventMetadata& metadata, const char* category) {
    const uint32_t event_pid = record->EventHeader.ProcessId;
    if (!process_tree_allows_event(context, metadata, category, event_pid)) {
        context.skipped_events += 1;
        return;
    }

    const std::filesystem::path path = context.events_dir / (std::string(category) + ".jsonl");
    std::ofstream out(path, std::ios::binary | std::ios::app);
    if (!out) {
        context.skipped_events += 1;
        return;
    }

    const auto& descriptor = record->EventHeader.EventDescriptor;
    const std::string event_type = !metadata.names.opcode.empty()
                                       ? metadata.names.opcode
                                       : (!metadata.names.task.empty() ? metadata.names.task : "event");
    const uint32_t process_pid = metadata.process_pid.value_or(event_pid);
    if (std::strcmp(category, "image") == 0) {
        update_module_map(context, metadata, event_type, process_pid);
    }
    const auto frames = stack_frames(context, record, process_pid);
    out << "{\"schema_version\":1"
        << ",\"timestamp\":{\"unix_millis\":" << unix_millis_from_etw_timestamp(record->EventHeader.TimeStamp) << "}"
        << ",\"category\":\"" << category << "\""
        << ",\"event_type\":\"" << json_escape(event_type) << "\""
        << ",\"pid\":" << process_pid
        << ",\"tid\":" << record->EventHeader.ThreadId
        << ",\"process\":{\"pid\":" << process_pid << ",\"parent_pid\":";
    if (metadata.parent_pid.has_value()) {
        out << *metadata.parent_pid;
    } else {
        out << "null";
    }
    out << ",\"image_path\":"
        << json_string_or_null(first_matching_string(metadata, {"image", "filename", "processname"}))
        << ",\"command_line\":"
        << json_string_or_null(first_matching_string(metadata, {"commandline", "command line"}))
        << "}";
    write_category_fields(out, metadata, category, event_type);
    write_stack_json(out, frames);
    out
        << ",\"operation_id\":null"
        << ",\"artifact_id\":null"
        << ",\"etw\":{"
        << "\"provider\":\"" << guid_to_string(record->EventHeader.ProviderId) << "\""
        << ",\"provider_name\":\"" << json_escape(metadata.names.provider) << "\""
        << ",\"task\":\"" << json_escape(metadata.names.task) << "\""
        << ",\"event_id\":" << descriptor.Id
        << ",\"version\":" << static_cast<unsigned int>(descriptor.Version)
        << ",\"opcode\":" << static_cast<unsigned int>(descriptor.Opcode)
        << ",\"opcode_name\":\"" << json_escape(metadata.names.opcode) << "\""
        << ",\"keywords\":" << descriptor.Keyword
        << ",\"raw\":{\"user_data_length\":" << record->UserDataLength << ",\"numeric_fields\":{";
    bool first = true;
    for (const auto& [name, value] : metadata.numeric_fields) {
        if (!first) {
            out << ",";
        }
        first = false;
        out << "\"" << json_escape(name) << "\":" << value;
    }
    out << "},\"string_fields\":{";
    first = true;
    for (const auto& [name, value] : metadata.string_fields) {
        if (!first) {
            out << ",";
        }
        first = false;
        out << "\"" << json_escape(name) << "\":\"" << json_escape(value) << "\"";
    }
    out << "}}"
        << "}}\n";
    context.files_written.insert(category);
    context.events_written += 1;
}

void WINAPI event_record_callback(PEVENT_RECORD record) {
    auto* context = static_cast<ExtractionContext*>(record->UserContext);
    if (context == nullptr) {
        return;
    }
    const EventMetadata metadata = event_metadata(record);
    if (auto stack = decode_stack_walk_event(record, metadata); stack.has_value()) {
        process_stack_walk_event(*context, std::move(*stack));
        return;
    }
    const char* raw_category = event_category(metadata.names);
    if (raw_category == nullptr) {
        context->skipped_events += 1;
        return;
    }
    const std::string event_type = !metadata.names.opcode.empty()
                                       ? metadata.names.opcode
                                       : (!metadata.names.task.empty() ? metadata.names.task : "event");
    if (std::strcmp(raw_category, "image") == 0 &&
        process_tree_allows_event(*context, metadata, raw_category, record->EventHeader.ProcessId)) {
        const uint32_t image_pid = metadata.process_pid.value_or(record->EventHeader.ProcessId);
        update_module_map(*context, metadata, event_type, image_pid);
    }
    if (is_known_file_io_event(record, raw_category)) {
        process_file_io_event(*context, record, metadata);
        return;
    }
    const char* category = preset_enabled(context->preset_flags, raw_category) ? raw_category : nullptr;
    if (category == nullptr) {
        context->skipped_events += 1;
        return;
    }
    write_extracted_event(*context, record, metadata, category);
}

ULONG process_trace_logfile(EVENT_TRACE_LOGFILEA& logfile) {
    TRACEHANDLE trace = OpenTraceA(&logfile);
    if (trace == INVALID_PROCESSTRACE_HANDLE) {
        return GetLastError();
    }

    const ULONG status = ProcessTrace(&trace, 1, nullptr, nullptr);
    const ULONG close_status = CloseTrace(trace);
    if (status != ERROR_SUCCESS) {
        return status;
    }
    return close_status;
}

bool result_has_field(uint32_t caller_size, size_t offset, size_t field_size) {
    return caller_size >= offset + field_size;
}

uint32_t initialize_extraction_result(DA_EtwEventExtractionResult* out) {
    const uint32_t caller_size = out->struct_size == 0
                                     ? static_cast<uint32_t>(offsetof(DA_EtwEventExtractionResult, stack_frames_total))
                                     : out->struct_size;
    const size_t clear_size = std::min<size_t>(caller_size, sizeof(DA_EtwEventExtractionResult));
    std::memset(out, 0, clear_size);
    out->struct_size = sizeof(DA_EtwEventExtractionResult);
    return caller_size;
}

void copy_extraction_result(DA_EtwEventExtractionResult* out, uint32_t caller_size, const ExtractionContext& context) {
    out->struct_size = sizeof(DA_EtwEventExtractionResult);
#define DA_SET_ETW_RESULT_FIELD(field, value)                                      \
    do {                                                                          \
        if (result_has_field(caller_size, offsetof(DA_EtwEventExtractionResult, field), sizeof(out->field))) { \
            out->field = (value);                                                 \
        }                                                                         \
    } while (0)
    DA_SET_ETW_RESULT_FIELD(events_written, context.events_written);
    DA_SET_ETW_RESULT_FIELD(files_written, static_cast<uint32_t>(context.files_written.size()));
    DA_SET_ETW_RESULT_FIELD(skipped_events, context.skipped_events);
    DA_SET_ETW_RESULT_FIELD(stack_frames_total, context.quality.stack_frames_total);
    DA_SET_ETW_RESULT_FIELD(stack_frames_resolved, context.quality.stack_frames_resolved);
    DA_SET_ETW_RESULT_FIELD(stack_frames_unresolved, context.quality.stack_frames_unresolved);
    DA_SET_ETW_RESULT_FIELD(file_path_resolved, context.quality.file_path_resolved);
    DA_SET_ETW_RESULT_FIELD(file_path_unresolved, context.quality.file_path_unresolved);
    DA_SET_ETW_RESULT_FIELD(matched_op_end, context.quality.matched_op_end);
    DA_SET_ETW_RESULT_FIELD(unmatched_op_end, context.quality.unmatched_op_end);
    DA_SET_ETW_RESULT_FIELD(incomplete_io, context.quality.incomplete_io);
    DA_SET_ETW_RESULT_FIELD(reused_irp, context.quality.reused_irp);
    DA_SET_ETW_RESULT_FIELD(dropped_stack_walk, context.quality.dropped_stack_walk);
#undef DA_SET_ETW_RESULT_FIELD
}

struct RealTimeConsumer final {
    ExtractionContext context;
    std::string session_name;
    std::thread worker;
    std::atomic<ULONG> status = ERROR_SUCCESS;
};

void run_realtime_consumer(RealTimeConsumer* consumer) noexcept {
    EVENT_TRACE_LOGFILEA logfile = {};
    logfile.LoggerName = consumer->session_name.data();
    logfile.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.EventRecordCallback = event_record_callback;
    logfile.Context = &consumer->context;
    const ULONG status = process_trace_logfile(logfile);
    flush_pending_file_events(consumer->context);
    consumer->status.store(status, std::memory_order_relaxed);
}

struct ScopedBStr final {
    BSTR value = nullptr;

    explicit ScopedBStr(const std::wstring& text) : value(SysAllocString(text.c_str())) {}

    ~ScopedBStr() {
        if (value != nullptr) {
            SysFreeString(value);
        }
    }
};

bool keep_stack_walk_for_filter(ExtractionContext& context, PEVENT_RECORD record, const EventMetadata& metadata) {
    auto stack = decode_stack_walk_event(record, metadata);
    if (!stack.has_value()) {
        return false;
    }
    if (!context.has_root_pid) {
        return true;
    }
    return context.process_tree_pids.find(stack->stack_process) != context.process_tree_pids.end();
}

bool keep_file_completion_for_filter(ExtractionContext& context, PEVENT_RECORD record, const char* category) {
    return preset_enabled(context.preset_flags, "file") && is_known_file_io_event(record, category) &&
           record->EventHeader.EventDescriptor.Opcode == DA_FILE_IO_OP_END;
}

class FilteringReloggerCallback final : public ITraceEventCallback {
public:
    explicit FilteringReloggerCallback(ExtractionContext context) : context_(std::move(context)) {
        initialize_process_tree(context_);
    }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_ITraceEventCallback)) {
            *object = static_cast<ITraceEventCallback*>(this);
            AddRef();
            return S_OK;
        }
        *object = nullptr;
        return E_NOINTERFACE;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return reference_count_.fetch_add(1, std::memory_order_relaxed) + 1;
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG count = reference_count_.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (count == 0) {
            delete this;
        }
        return count;
    }

    HRESULT STDMETHODCALLTYPE OnBeginProcessTrace(ITraceEvent*, ITraceRelogger*) override {
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE OnFinalizeProcessTrace(ITraceRelogger*) override {
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE OnEvent(ITraceEvent* event, ITraceRelogger* relogger) override {
        if (event == nullptr || relogger == nullptr) {
            context_.skipped_events += 1;
            return S_OK;
        }

        PEVENT_RECORD record = nullptr;
        HRESULT hr = event->GetEventRecord(&record);
        if (FAILED(hr) || record == nullptr) {
            context_.skipped_events += 1;
            return S_OK;
        }

        const EventMetadata metadata = event_metadata(record);
        const char* category = event_category(metadata.names);
        const bool keep_for_output = category != nullptr && preset_enabled(context_.preset_flags, category);
        const bool keep_for_module_map = category != nullptr && std::strcmp(category, "image") == 0;
        const bool keep_for_stack_walk = keep_stack_walk_for_filter(context_, record, metadata);
        const bool keep_for_file_completion = keep_file_completion_for_filter(context_, record, category);
        const bool needs_process_tree_filter = !keep_for_stack_walk && !keep_for_file_completion;
        if ((!keep_for_output && !keep_for_module_map && !keep_for_stack_walk && !keep_for_file_completion) ||
            (needs_process_tree_filter &&
             !process_tree_allows_event(context_, metadata, category, record->EventHeader.ProcessId))) {
            context_.skipped_events += 1;
            return S_OK;
        }

        hr = relogger->Inject(event);
        if (SUCCEEDED(hr)) {
            context_.events_written += 1;
            if (keep_for_output) {
                context_.files_written.insert(category);
            }
        } else {
            context_.skipped_events += 1;
        }
        return S_OK;
    }

    const ExtractionContext& context() const {
        return context_;
    }

private:
    std::atomic<ULONG> reference_count_ = 1;
    ExtractionContext context_;
};

struct TraceProperties final {
    std::unique_ptr<unsigned char[]> storage;
    EVENT_TRACE_PROPERTIES* properties = nullptr;
};

struct ProviderSpec final {
    GUID id;
    ULONGLONG match_any_keyword = 0;
};

std::vector<ProviderSpec> provider_specs(uint32_t preset_flags) {
    std::vector<ProviderSpec> providers;
    ULONGLONG process_keywords = 0;
    const uint32_t collection_flags = preset_flags == 0 ? 0 : preset_flags | DA_ETW_PRESET_IMAGE;
    if ((collection_flags & DA_ETW_PRESET_PROCESS) != 0) {
        process_keywords |= 0x10;
    }
    if ((collection_flags & DA_ETW_PRESET_THREAD) != 0) {
        process_keywords |= 0x20;
    }
    if ((collection_flags & DA_ETW_PRESET_IMAGE) != 0) {
        process_keywords |= 0x40;
    }
    if (process_keywords != 0) {
        providers.push_back(ProviderSpec{DA_KernelProcessProviderGuid, process_keywords});
    }
    if ((preset_flags & DA_ETW_PRESET_FILE) != 0) {
        providers.push_back(ProviderSpec{DA_KernelFileProviderGuid, 0x1ff0});
    }
    if ((preset_flags & DA_ETW_PRESET_REGISTRY) != 0) {
        providers.push_back(ProviderSpec{DA_KernelRegistryProviderGuid, 0xffff});
    }
    if ((preset_flags & DA_ETW_PRESET_NETWORK) != 0) {
        providers.push_back(ProviderSpec{DA_KernelNetworkProviderGuid, 0x30});
    }
    return providers;
}

std::vector<CLASSIC_EVENT_ID> stack_trace_events(uint32_t preset_flags) {
    std::vector<CLASSIC_EVENT_ID> events;
    const uint32_t collection_flags = preset_flags == 0 ? 0 : preset_flags | DA_ETW_PRESET_IMAGE;
    auto add = [&events](const GUID& guid, UCHAR type) {
        CLASSIC_EVENT_ID event = {};
        event.EventGuid = guid;
        event.Type = type;
        events.push_back(event);
    };

    if ((collection_flags & DA_ETW_PRESET_PROCESS) != 0) {
        add(DA_ClassicProcessGuid, EVENT_TRACE_TYPE_START);
        add(DA_ClassicProcessGuid, EVENT_TRACE_TYPE_END);
    }
    if ((collection_flags & DA_ETW_PRESET_THREAD) != 0) {
        add(DA_ClassicThreadGuid, EVENT_TRACE_TYPE_START);
        add(DA_ClassicThreadGuid, EVENT_TRACE_TYPE_END);
    }
    if ((collection_flags & DA_ETW_PRESET_IMAGE) != 0) {
        add(DA_ClassicImageLoadGuid, EVENT_TRACE_TYPE_LOAD);
    }
    if ((collection_flags & DA_ETW_PRESET_FILE) != 0) {
        add(DA_ClassicFileIoGuid, EVENT_TRACE_TYPE_IO_READ);
        add(DA_ClassicFileIoGuid, EVENT_TRACE_TYPE_IO_WRITE);
        add(DA_ClassicFileIoGuid, EVENT_TRACE_TYPE_IO_READ_INIT);
        add(DA_ClassicFileIoGuid, EVENT_TRACE_TYPE_IO_WRITE_INIT);
    }
    if ((collection_flags & DA_ETW_PRESET_REGISTRY) != 0) {
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGCREATE);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGOPEN);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGDELETE);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGQUERY);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGSETVALUE);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGDELETEVALUE);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGQUERYVALUE);
        add(DA_ClassicRegistryGuid, EVENT_TRACE_TYPE_REGCLOSE);
    }
    if ((collection_flags & DA_ETW_PRESET_NETWORK) != 0) {
        add(DA_ClassicTcpIpGuid, EVENT_TRACE_TYPE_SEND);
        add(DA_ClassicTcpIpGuid, EVENT_TRACE_TYPE_RECEIVE);
        add(DA_ClassicTcpIpGuid, EVENT_TRACE_TYPE_CONNECT);
        add(DA_ClassicTcpIpGuid, EVENT_TRACE_TYPE_DISCONNECT);
        add(DA_ClassicTcpIpGuid, EVENT_TRACE_TYPE_ACCEPT);
        add(DA_ClassicUdpIpGuid, EVENT_TRACE_TYPE_SEND);
        add(DA_ClassicUdpIpGuid, EVENT_TRACE_TYPE_RECEIVE);
    }
    return events;
}

void enable_kernel_stack_tracing(TRACEHANDLE handle, uint32_t preset_flags, StackTraceRuntimeStatus& stack_status) {
    const auto events = stack_trace_events(preset_flags);
    if (events.empty()) {
        return;
    }

    const ULONG status = TraceSetInformation(
        handle,
        TraceStackTracingInfo,
        const_cast<CLASSIC_EVENT_ID*>(events.data()),
        static_cast<ULONG>(events.size() * sizeof(CLASSIC_EVENT_ID)));
    if (status == ERROR_SUCCESS) {
        stack_status.kernel_stack_enabled = true;
    } else {
        stack_status.kernel_stack_warning_count += 1;
    }
}

ULONG enable_providers(
    TRACEHANDLE handle,
    const std::vector<ProviderSpec>& providers,
    StackTraceRuntimeStatus& stack_status) {
    for (const auto& provider : providers) {
        ENABLE_TRACE_PARAMETERS params = {};
        params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
        params.EnableProperty = EVENT_ENABLE_PROPERTY_STACK_TRACE;
        ULONG status = EnableTraceEx2(
            handle,
            &provider.id,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE,
            provider.match_any_keyword,
            0,
            0,
            &params);
        if (status == ERROR_SUCCESS) {
            stack_status.provider_stack_enabled = true;
            continue;
        }

        stack_status.provider_stack_warning_count += 1;
        status = EnableTraceEx2(
            handle,
            &provider.id,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE,
            provider.match_any_keyword,
            0,
            0,
            nullptr);
        if (status != ERROR_SUCCESS) {
            return status;
        }
    }
    return ERROR_SUCCESS;
}

void disable_providers(TRACEHANDLE handle, const std::vector<ProviderSpec>& providers) {
    for (const auto& provider : providers) {
        EnableTraceEx2(
            handle,
            &provider.id,
            EVENT_CONTROL_CODE_DISABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE,
            0,
            0,
            0,
            nullptr);
    }
}

TraceProperties make_trace_properties(
    const char* session_name_utf8,
    const char* trace_path_utf8) {
    const size_t logger_name_len = std::strlen(session_name_utf8) + 1;
    const size_t log_file_len = std::strlen(trace_path_utf8) + 1;
    const size_t properties_size =
        sizeof(EVENT_TRACE_PROPERTIES) + logger_name_len + log_file_len;
    TraceProperties trace;
    trace.storage = std::make_unique<unsigned char[]>(properties_size);
    std::memset(trace.storage.get(), 0, properties_size);
    trace.properties = reinterpret_cast<EVENT_TRACE_PROPERTIES*>(trace.storage.get());
    trace.properties->Wnode.BufferSize = static_cast<ULONG>(properties_size);
    trace.properties->Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    trace.properties->LogFileMode = EVENT_TRACE_FILE_MODE_SEQUENTIAL | EVENT_TRACE_REAL_TIME_MODE;
    trace.properties->LoggerNameOffset = sizeof(EVENT_TRACE_PROPERTIES);
    trace.properties->LogFileNameOffset =
        static_cast<ULONG>(sizeof(EVENT_TRACE_PROPERTIES) + logger_name_len);
    std::memcpy(trace.storage.get() + trace.properties->LoggerNameOffset, session_name_utf8, logger_name_len);
    std::memcpy(trace.storage.get() + trace.properties->LogFileNameOffset, trace_path_utf8, log_file_len);
    return trace;
}
#endif

} // namespace

struct DA_EtwSessionHandle {
#ifdef _WIN32
    TRACEHANDLE handle = 0;
    std::string session_name;
    std::string trace_path;
    std::vector<ProviderSpec> enabled_providers;
    StackTraceRuntimeStatus stack_trace_status;
    std::unique_ptr<RealTimeConsumer> realtime_consumer;
#endif
};

int32_t da_etw_abi_version(DA_EtwVersion* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out version pointer is null");
        }
        out->struct_size = sizeof(DA_EtwVersion);
        out->flags = 0;
        out->abi_major = 0;
        out->abi_minor = 2;
        out->abi_patch = 0;
        return DA_ETW_OK;
    });
}

int32_t da_etw_adapter_info(DA_EtwAdapterInfo* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out adapter info pointer is null");
        }
        out->struct_size = sizeof(DA_EtwAdapterInfo);
        out->flags = 0;
        out->capability_flags =
            DA_ETW_CAP_REALTIME_CONSUME | DA_ETW_CAP_FILE_TRACE | DA_ETW_CAP_PROCESS_TREE_FILTER |
            DA_ETW_CAP_EVENT_STACK_TRACE;
        return DA_ETW_OK;
    });
}

int32_t da_etw_last_error(char* buffer, size_t buffer_len, size_t* required_len) {
    return guard([&]() -> int32_t {
        const size_t required = g_last_error.size() + 1;
        if (required_len != nullptr) {
            *required_len = required;
        }
        if (buffer == nullptr || buffer_len == 0) {
            return required <= 1 ? DA_ETW_OK : DA_ETW_ERR_BUFFER_TOO_SMALL;
        }
        const size_t copy_len = std::min(buffer_len - 1, g_last_error.size());
        if (copy_len > 0) {
            std::memcpy(buffer, g_last_error.data(), copy_len);
        }
        buffer[copy_len] = '\0';
        return required <= buffer_len ? DA_ETW_OK : DA_ETW_ERR_BUFFER_TOO_SMALL;
    });
}

int32_t da_etw_write_minimal_file_trace(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags) {
    return guard([&]() -> int32_t {
        if (session_name_utf8 == nullptr || session_name_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "session name is empty");
        }
        if (trace_path_utf8 == nullptr || trace_path_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "trace path is empty");
        }

#ifndef _WIN32
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW file trace is only available on Windows");
#else
        auto trace = make_trace_properties(session_name_utf8, trace_path_utf8);
        TRACEHANDLE handle = 0;
        ULONG status = StartTraceA(&handle, session_name_utf8, trace.properties);
        if (status == ERROR_ALREADY_EXISTS) {
            ControlTraceA(0, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            status = StartTraceA(&handle, session_name_utf8, trace.properties);
        }
        if (status != ERROR_SUCCESS) {
            return fail_win32("StartTraceA", status);
        }

        const auto providers = provider_specs(preset_flags);
        StackTraceRuntimeStatus stack_status;
        status = enable_providers(handle, providers, stack_status);
        if (status != ERROR_SUCCESS) {
            ControlTraceA(handle, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            return fail_win32("EnableTraceEx2", status);
        }
        enable_kernel_stack_tracing(handle, preset_flags, stack_status);

        status = ControlTraceA(handle, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
        if (status != ERROR_SUCCESS) {
            return fail_win32("ControlTraceA(STOP)", status);
        }
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_session_start_file_trace(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags,
    DA_EtwSessionHandle** out_handle) {
    return guard([&]() -> int32_t {
        if (out_handle == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out handle pointer is null");
        }
        *out_handle = nullptr;
        if (session_name_utf8 == nullptr || session_name_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "session name is empty");
        }
        if (trace_path_utf8 == nullptr || trace_path_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "trace path is empty");
        }

#ifndef _WIN32
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW file trace is only available on Windows");
#else
        auto trace = make_trace_properties(session_name_utf8, trace_path_utf8);
        TRACEHANDLE handle = 0;
        ULONG status = StartTraceA(&handle, session_name_utf8, trace.properties);
        if (status == ERROR_ALREADY_EXISTS) {
            ControlTraceA(0, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            status = StartTraceA(&handle, session_name_utf8, trace.properties);
        }
        if (status != ERROR_SUCCESS) {
            return fail_win32("StartTraceA", status);
        }

        auto providers = provider_specs(preset_flags);
        StackTraceRuntimeStatus stack_status;
        status = enable_providers(handle, providers, stack_status);
        if (status != ERROR_SUCCESS) {
            ControlTraceA(handle, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            return fail_win32("EnableTraceEx2", status);
        }
        enable_kernel_stack_tracing(handle, preset_flags, stack_status);

        auto session = std::make_unique<DA_EtwSessionHandle>();
        session->handle = handle;
        session->session_name = session_name_utf8;
        session->trace_path = trace_path_utf8;
        session->enabled_providers = std::move(providers);
        session->stack_trace_status = stack_status;
        *out_handle = session.release();
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_session_stack_trace_status(
    DA_EtwSessionHandle* handle,
    DA_EtwStackTraceStatus* out) {
    return guard([&]() -> int32_t {
        if (handle == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        if (out == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out stack trace status pointer is null");
        }
        out->struct_size = sizeof(DA_EtwStackTraceStatus);
        out->flags = 0;
#ifndef _WIN32
        out->requested = 1;
        out->enabled = 0;
        out->provider_stack_enabled = 0;
        out->provider_stack_warning_count = 0;
        out->kernel_stack_enabled = 0;
        out->kernel_stack_warning_count = 0;
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW stack trace status is only available on Windows");
#else
        const auto& status = handle->stack_trace_status;
        out->requested = status.requested ? 1u : 0u;
        out->enabled = status.enabled() ? 1u : 0u;
        out->provider_stack_enabled = status.provider_stack_enabled ? 1u : 0u;
        out->provider_stack_warning_count = status.provider_stack_warning_count;
        out->kernel_stack_enabled = status.kernel_stack_enabled ? 1u : 0u;
        out->kernel_stack_warning_count = status.kernel_stack_warning_count;
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_session_start_realtime_consumer(
    DA_EtwSessionHandle* handle,
    const char* events_dir_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid) {
    return guard([&]() -> int32_t {
        if (handle == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "session handle is null");
        }
        if (events_dir_utf8 == nullptr || events_dir_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "events dir is empty");
        }

#ifndef _WIN32
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW realtime consumer is only available on Windows");
#else
        if (handle->realtime_consumer != nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "realtime consumer is already running");
        }

        auto consumer = std::make_unique<RealTimeConsumer>();
        consumer->session_name = handle->session_name;
        consumer->context.events_dir = std::filesystem::u8path(events_dir_utf8);
        consumer->context.preset_flags = preset_flags;
        consumer->context.has_root_pid = has_root_pid != 0;
        consumer->context.root_pid = root_pid;
        initialize_process_tree(consumer->context);
        std::filesystem::create_directories(consumer->context.events_dir);
        consumer->worker = std::thread(run_realtime_consumer, consumer.get());
        handle->realtime_consumer = std::move(consumer);
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_session_stop(DA_EtwSessionHandle* handle) {
    return guard([&]() -> int32_t {
        if (handle == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "session handle is null");
        }

#ifndef _WIN32
        delete handle;
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW file trace is only available on Windows");
#else
        auto trace = make_trace_properties(handle->session_name.c_str(), handle->trace_path.c_str());
        disable_providers(handle->handle, handle->enabled_providers);
        const ULONG status = ControlTraceA(
            handle->handle,
            handle->session_name.c_str(),
            trace.properties,
            EVENT_TRACE_CONTROL_STOP);
        if (handle->realtime_consumer != nullptr && handle->realtime_consumer->worker.joinable()) {
            handle->realtime_consumer->worker.join();
        }
        const ULONG consumer_status = handle->realtime_consumer != nullptr
                                          ? handle->realtime_consumer->status.load(std::memory_order_relaxed)
                                          : ERROR_SUCCESS;
        delete handle;
        if (status != ERROR_SUCCESS) {
            return fail_win32("ControlTraceA(STOP)", status);
        }
        if (consumer_status != ERROR_SUCCESS && consumer_status != ERROR_CANCELLED) {
            return fail_win32("ProcessTrace(REALTIME)", consumer_status);
        }
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_extract_file_events(
    const char* trace_path_utf8,
    const char* events_dir_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid,
    DA_EtwEventExtractionResult* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out extraction result pointer is null");
        }
        const uint32_t caller_result_size = initialize_extraction_result(out);
        if (trace_path_utf8 == nullptr || trace_path_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "trace path is empty");
        }
        if (events_dir_utf8 == nullptr || events_dir_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "events dir is empty");
        }

#ifndef _WIN32
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW file extraction is only available on Windows");
#else
        ExtractionContext context;
        context.events_dir = std::filesystem::u8path(events_dir_utf8);
        context.preset_flags = preset_flags;
        context.has_root_pid = has_root_pid != 0;
        context.root_pid = root_pid;
        initialize_process_tree(context);
        std::filesystem::create_directories(context.events_dir);

        EVENT_TRACE_LOGFILEA logfile = {};
        logfile.LogFileName = const_cast<char*>(trace_path_utf8);
        logfile.ProcessTraceMode = PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.EventRecordCallback = event_record_callback;
        logfile.Context = &context;

        const ULONG status = process_trace_logfile(logfile);
        flush_pending_file_events(context);
        if (status != ERROR_SUCCESS) {
            return fail_win32("ProcessTrace", status);
        }

        copy_extraction_result(out, caller_result_size, context);
        return DA_ETW_OK;
#endif
    });
}

int32_t da_etw_filter_trace_file(
    const char* input_trace_path_utf8,
    const char* output_trace_path_utf8,
    uint32_t preset_flags,
    uint32_t has_root_pid,
    uint32_t root_pid,
    DA_EtwEventExtractionResult* out) {
    return guard([&]() -> int32_t {
        if (out == nullptr) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "out filter result pointer is null");
        }
        const uint32_t caller_result_size = initialize_extraction_result(out);
        if (input_trace_path_utf8 == nullptr || input_trace_path_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "input trace path is empty");
        }
        if (output_trace_path_utf8 == nullptr || output_trace_path_utf8[0] == '\0') {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "output trace path is empty");
        }

#ifndef _WIN32
        return fail(DA_ETW_ERR_NOT_IMPLEMENTED, "ETW trace filtering is only available on Windows");
#else
        const std::wstring input_path = utf8_to_wide(input_trace_path_utf8);
        const std::wstring output_path = utf8_to_wide(output_trace_path_utf8);
        if (input_path.empty() || output_path.empty()) {
            return fail(DA_ETW_ERR_INVALID_ARGUMENT, "trace paths must be valid UTF-8");
        }

        const HRESULT init_hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        const bool should_uninitialize = SUCCEEDED(init_hr);
        if (FAILED(init_hr) && init_hr != RPC_E_CHANGED_MODE) {
            return fail_hresult("CoInitializeEx", init_hr);
        }

        ITraceRelogger* relogger = nullptr;
        HRESULT hr = CoCreateInstance(
            CLSID_TraceRelogger,
            nullptr,
            CLSCTX_INPROC_SERVER,
            IID_ITraceRelogger,
            reinterpret_cast<void**>(&relogger));
        if (FAILED(hr) || relogger == nullptr) {
            if (should_uninitialize) {
                CoUninitialize();
            }
            return fail_hresult("CoCreateInstance(CLSID_TraceRelogger)", hr);
        }

        ScopedBStr input_bstr(input_path);
        ScopedBStr output_bstr(output_path);
        if (input_bstr.value == nullptr || output_bstr.value == nullptr) {
            relogger->Release();
            if (should_uninitialize) {
                CoUninitialize();
            }
            return fail(DA_ETW_ERR_INTERNAL, "BSTR allocation failed");
        }

        RELOGSTREAM_ID stream_id = 0;
        hr = relogger->AddLogfileTraceStream(input_bstr.value, nullptr, &stream_id);
        if (FAILED(hr)) {
            relogger->Release();
            if (should_uninitialize) {
                CoUninitialize();
            }
            return fail_hresult("ITraceRelogger::AddLogfileTraceStream", hr);
        }

        hr = relogger->SetOutputFilename(output_bstr.value);
        if (FAILED(hr)) {
            relogger->Release();
            if (should_uninitialize) {
                CoUninitialize();
            }
            return fail_hresult("ITraceRelogger::SetOutputFilename", hr);
        }

        ExtractionContext context;
        context.preset_flags = preset_flags;
        context.has_root_pid = has_root_pid != 0;
        context.root_pid = root_pid;
        auto* callback = new FilteringReloggerCallback(std::move(context));
        hr = relogger->RegisterCallback(callback);
        if (FAILED(hr)) {
            callback->Release();
            relogger->Release();
            if (should_uninitialize) {
                CoUninitialize();
            }
            return fail_hresult("ITraceRelogger::RegisterCallback", hr);
        }

        hr = relogger->ProcessTrace();
        const auto& filter_context = callback->context();
        copy_extraction_result(out, caller_result_size, filter_context);
        callback->Release();
        relogger->Release();
        if (should_uninitialize) {
            CoUninitialize();
        }
        if (FAILED(hr)) {
            return fail_hresult("ITraceRelogger::ProcessTrace", hr);
        }
        return DA_ETW_OK;
#endif
    });
}
