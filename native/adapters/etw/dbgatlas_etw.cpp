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
#endif

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstdio>
#include <cstring>
#include <exception>
#include <filesystem>
#include <fstream>
#include <initializer_list>
#include <memory>
#include <new>
#include <optional>
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

const char* classify_event(const EventNames& names, uint32_t preset_flags) {
    const std::string text = to_lower_ascii(names.provider + " " + names.task + " " + names.opcode);
    const char* category = nullptr;
    if (contains_any(text, {"process"})) {
        category = "process";
    } else if (contains_any(text, {"thread"})) {
        category = "thread";
    } else if (contains_any(text, {"image", "imageload"})) {
        category = "image";
    } else if (contains_any(text, {"registry", "reg"})) {
        category = "registry";
    } else if (contains_any(text, {"tcp", "udp", "network"})) {
        category = "network";
    } else if (contains_any(text, {"file", "disk"})) {
        category = "file";
    }
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

struct ExtractionContext final {
    std::filesystem::path events_dir;
    uint32_t preset_flags = 0;
    bool has_root_pid = false;
    uint32_t root_pid = 0;
    uint32_t events_written = 0;
    uint32_t skipped_events = 0;
    std::unordered_set<std::string> files_written;
    std::unordered_set<uint32_t> process_tree_pids;
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
    const char* category = classify_event(metadata.names, context->preset_flags);
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
        const char* category = classify_event(metadata.names, context_.preset_flags);
        if (category == nullptr ||
            !process_tree_allows_event(context_, metadata, category, record->EventHeader.ProcessId)) {
            context_.skipped_events += 1;
            return S_OK;
        }

        hr = relogger->Inject(event);
        if (SUCCEEDED(hr)) {
            context_.events_written += 1;
            context_.files_written.insert(category);
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

ULONG kernel_enable_flags(uint32_t preset_flags) {
    ULONG flags = 0;
    if ((preset_flags & DA_ETW_PRESET_PROCESS) != 0) {
        flags |= EVENT_TRACE_FLAG_PROCESS;
    }
    if ((preset_flags & DA_ETW_PRESET_THREAD) != 0) {
        flags |= EVENT_TRACE_FLAG_THREAD;
    }
    if ((preset_flags & DA_ETW_PRESET_IMAGE) != 0) {
        flags |= EVENT_TRACE_FLAG_IMAGE_LOAD;
    }
    if ((preset_flags & DA_ETW_PRESET_FILE) != 0) {
        flags |= EVENT_TRACE_FLAG_DISK_IO;
        flags |= EVENT_TRACE_FLAG_DISK_FILE_IO;
        flags |= EVENT_TRACE_FLAG_FILE_IO;
        flags |= EVENT_TRACE_FLAG_FILE_IO_INIT;
    }
    if ((preset_flags & DA_ETW_PRESET_REGISTRY) != 0) {
        flags |= EVENT_TRACE_FLAG_REGISTRY;
    }
    if ((preset_flags & DA_ETW_PRESET_NETWORK) != 0) {
        flags |= EVENT_TRACE_FLAG_NETWORK_TCPIP;
    }
    return flags;
}

TraceProperties make_trace_properties(
    const char* session_name_utf8,
    const char* trace_path_utf8,
    uint32_t preset_flags) {
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
    trace.properties->EnableFlags = kernel_enable_flags(preset_flags);
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
        out->abi_minor = 1;
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
            DA_ETW_CAP_REALTIME_CONSUME | DA_ETW_CAP_FILE_TRACE | DA_ETW_CAP_PROCESS_TREE_FILTER;
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
        auto trace = make_trace_properties(session_name_utf8, trace_path_utf8, preset_flags);
        TRACEHANDLE handle = 0;
        ULONG status = StartTraceA(&handle, session_name_utf8, trace.properties);
        if (status == ERROR_ALREADY_EXISTS) {
            ControlTraceA(0, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            status = StartTraceA(&handle, session_name_utf8, trace.properties);
        }
        if (status != ERROR_SUCCESS) {
            return fail_win32("StartTraceA", status);
        }

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
        auto trace = make_trace_properties(session_name_utf8, trace_path_utf8, preset_flags);
        TRACEHANDLE handle = 0;
        ULONG status = StartTraceA(&handle, session_name_utf8, trace.properties);
        if (status == ERROR_ALREADY_EXISTS) {
            ControlTraceA(0, session_name_utf8, trace.properties, EVENT_TRACE_CONTROL_STOP);
            status = StartTraceA(&handle, session_name_utf8, trace.properties);
        }
        if (status != ERROR_SUCCESS) {
            return fail_win32("StartTraceA", status);
        }

        auto session = std::make_unique<DA_EtwSessionHandle>();
        session->handle = handle;
        session->session_name = session_name_utf8;
        session->trace_path = trace_path_utf8;
        *out_handle = session.release();
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
        auto trace = make_trace_properties(handle->session_name.c_str(), handle->trace_path.c_str(), 0);
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
        out->struct_size = sizeof(DA_EtwEventExtractionResult);
        out->events_written = 0;
        out->files_written = 0;
        out->skipped_events = 0;
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
        if (status != ERROR_SUCCESS) {
            return fail_win32("ProcessTrace", status);
        }

        out->events_written = context.events_written;
        out->files_written = static_cast<uint32_t>(context.files_written.size());
        out->skipped_events = context.skipped_events;
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
        out->struct_size = sizeof(DA_EtwEventExtractionResult);
        out->events_written = 0;
        out->files_written = 0;
        out->skipped_events = 0;
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
        out->events_written = filter_context.events_written;
        out->files_written = static_cast<uint32_t>(filter_context.files_written.size());
        out->skipped_events = filter_context.skipped_events;
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
