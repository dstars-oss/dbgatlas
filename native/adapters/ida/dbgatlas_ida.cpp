#define NOMINMAX
#include "dbgatlas_ida.h"

#include <cstring>

#ifdef _WIN32
#include <windows.h>

#include <ida.hpp>
#include <auto.hpp>
#include <bytes.hpp>
#include <frame.hpp>
#include <funcs.hpp>
#include <hexrays.hpp>
#include <idalib.hpp>
#include <lines.hpp>
#include <name.hpp>
#include <nalt.hpp>
#include <xref.hpp>

#include <algorithm>
#include <array>
#include <cctype>
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

struct IdaSessionHandleImpl {
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

void validate_ida_install_dir(const std::wstring& install_dir) {
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

std::string qstring_to_string(const qstring& value) {
    return value.c_str() == nullptr ? std::string() : std::string(value.c_str());
}

std::string json_escape(const std::string& text) {
    std::ostringstream out;
    for (unsigned char ch : text) {
        switch (ch) {
        case '\\': out << "\\\\"; break;
        case '"': out << "\\\""; break;
        case '\n': out << "\\n"; break;
        case '\r': out << "\\r"; break;
        case '\t': out << "\\t"; break;
        default:
            if (ch < 0x20) {
                out << "\\u";
                const char* hex = "0123456789abcdef";
                out << '0' << '0' << hex[(ch >> 4) & 0xf] << hex[ch & 0xf];
            } else {
                out << static_cast<char>(ch);
            }
            break;
        }
    }
    return out.str();
}

std::string json_string(const std::string& text) {
    return "\"" + json_escape(text) + "\"";
}

std::string json_u64(uint64_t value) {
    return std::to_string(value);
}

std::string json_nullable_u64(ea_t value) {
    return value == BADADDR ? "null" : json_u64(static_cast<uint64_t>(value));
}

std::string trim_copy(std::string value) {
    auto is_space = [](unsigned char ch) { return std::isspace(ch) != 0; };
    value.erase(value.begin(), std::find_if(value.begin(), value.end(), [&](unsigned char ch) { return !is_space(ch); }));
    value.erase(std::find_if(value.rbegin(), value.rend(), [&](unsigned char ch) { return !is_space(ch); }).base(), value.end());
    return value;
}

std::string find_json_value(const std::string& json, const std::string& key) {
    const std::string needle = "\"" + key + "\"";
    size_t pos = json.find(needle);
    if (pos == std::string::npos) {
        return "";
    }
    pos = json.find(':', pos + needle.size());
    if (pos == std::string::npos) {
        return "";
    }
    ++pos;
    while (pos < json.size() && std::isspace(static_cast<unsigned char>(json[pos])) != 0) {
        ++pos;
    }
    if (pos >= json.size()) {
        return "";
    }
    if (json[pos] == '"') {
        size_t end = pos + 1;
        bool escape = false;
        for (; end < json.size(); ++end) {
            if (escape) {
                escape = false;
            } else if (json[end] == '\\') {
                escape = true;
            } else if (json[end] == '"') {
                break;
            }
        }
        return end < json.size() ? json.substr(pos, end - pos + 1) : "";
    }
    if (json[pos] == '[') {
        size_t end = pos + 1;
        int depth = 1;
        bool in_string = false;
        bool escape = false;
        for (; end < json.size(); ++end) {
            char ch = json[end];
            if (in_string) {
                if (escape) {
                    escape = false;
                } else if (ch == '\\') {
                    escape = true;
                } else if (ch == '"') {
                    in_string = false;
                }
            } else if (ch == '"') {
                in_string = true;
            } else if (ch == '[') {
                ++depth;
            } else if (ch == ']') {
                --depth;
                if (depth == 0) {
                    break;
                }
            }
        }
        return end < json.size() ? json.substr(pos, end - pos + 1) : "";
    }
    size_t end = pos;
    while (end < json.size() && json[end] != ',' && json[end] != '}') {
        ++end;
    }
    return trim_copy(json.substr(pos, end - pos));
}

std::string unquote_json_string(std::string value) {
    value = trim_copy(std::move(value));
    if (value.size() < 2 || value.front() != '"' || value.back() != '"') {
        return value;
    }
    std::string out;
    for (size_t i = 1; i + 1 < value.size(); ++i) {
        char ch = value[i];
        if (ch == '\\' && i + 1 < value.size()) {
            char next = value[++i];
            switch (next) {
            case 'n': out.push_back('\n'); break;
            case 'r': out.push_back('\r'); break;
            case 't': out.push_back('\t'); break;
            case '\\': out.push_back('\\'); break;
            case '"': out.push_back('"'); break;
            default: out.push_back(next); break;
            }
        } else {
            out.push_back(ch);
        }
    }
    return out;
}

bool parse_u64_text(const std::string& text, uint64_t* out) {
    std::string value = trim_copy(text);
    if (value.empty()) {
        return false;
    }
    int base = 10;
    if (value.size() > 2 && value[0] == '0' && (value[1] == 'x' || value[1] == 'X')) {
        base = 16;
        value = value.substr(2);
    } else if (value.size() > 2 && value[0] == '0' && (value[1] == 'b' || value[1] == 'B')) {
        base = 2;
        value = value.substr(2);
    }
    uint64_t result = 0;
    for (char ch : value) {
        int digit = -1;
        if (ch >= '0' && ch <= '9') digit = ch - '0';
        else if (ch >= 'a' && ch <= 'f') digit = ch - 'a' + 10;
        else if (ch >= 'A' && ch <= 'F') digit = ch - 'A' + 10;
        if (digit < 0 || digit >= base) {
            return false;
        }
        if (result > (std::numeric_limits<uint64_t>::max() - static_cast<uint64_t>(digit)) / static_cast<uint64_t>(base)) {
            return false;
        }
        result = result * static_cast<uint64_t>(base) + static_cast<uint64_t>(digit);
    }
    *out = result;
    return true;
}

uint64_t json_u64_arg(const std::string& args, const std::string& key, uint64_t fallback) {
    std::string value = find_json_value(args, key);
    if (value.empty()) {
        return fallback;
    }
    value = unquote_json_string(value);
    uint64_t parsed = 0;
    return parse_u64_text(value, &parsed) ? parsed : fallback;
}

uint64_t required_json_u64_arg(const std::string& args, const std::string& key) {
    std::string value = find_json_value(args, key);
    if (value.empty()) {
        throw std::invalid_argument(key + " is required");
    }
    value = unquote_json_string(value);
    uint64_t parsed = 0;
    if (!parse_u64_text(value, &parsed)) {
        throw std::invalid_argument(key + " must be an unsigned integer or address string");
    }
    return parsed;
}

std::string json_string_arg(const std::string& args, const std::string& key) {
    return unquote_json_string(find_json_value(args, key));
}

std::vector<std::string> json_list_arg(const std::string& args, const std::string& key) {
    std::string value = trim_copy(find_json_value(args, key));
    std::vector<std::string> result;
    if (value.empty() || value == "null") {
        return result;
    }
    if (value.front() == '[') {
        size_t pos = 1;
        while (pos < value.size() && value[pos] != ']') {
            while (pos < value.size() && (std::isspace(static_cast<unsigned char>(value[pos])) != 0 || value[pos] == ',')) {
                ++pos;
            }
            if (pos >= value.size() || value[pos] == ']') {
                break;
            }
            if (value[pos] == '"') {
                size_t end = pos + 1;
                bool escape = false;
                for (; end < value.size(); ++end) {
                    if (escape) escape = false;
                    else if (value[end] == '\\') escape = true;
                    else if (value[end] == '"') break;
                }
                result.push_back(unquote_json_string(value.substr(pos, end - pos + 1)));
                pos = end + 1;
            } else {
                size_t end = pos;
                while (end < value.size() && value[end] != ',' && value[end] != ']') {
                    ++end;
                }
                result.push_back(trim_copy(value.substr(pos, end - pos)));
                pos = end;
            }
        }
        return result;
    }
    std::string scalar = unquote_json_string(value);
    size_t start = 0;
    while (start <= scalar.size()) {
        size_t comma = scalar.find(',', start);
        std::string part = trim_copy(scalar.substr(start, comma == std::string::npos ? std::string::npos : comma - start));
        if (!part.empty()) {
            result.push_back(part);
        }
        if (comma == std::string::npos) {
            break;
        }
        start = comma + 1;
    }
    return result;
}

std::string function_json(func_t* function) {
    if (function == nullptr) {
        return "null";
    }
    qstring name;
    get_func_name(&name, function->start_ea);
    std::ostringstream out;
    out << "{\"address\":" << json_u64(static_cast<uint64_t>(function->start_ea))
        << ",\"start_ea\":" << json_u64(static_cast<uint64_t>(function->start_ea))
        << ",\"end_ea\":" << json_u64(static_cast<uint64_t>(function->end_ea))
        << ",\"size\":" << json_u64(static_cast<uint64_t>(function->end_ea - function->start_ea))
        << ",\"name\":" << json_string(qstring_to_string(name)) << "}";
    return out.str();
}

std::string core_int_convert(const std::string& args) {
    std::vector<std::string> inputs = json_list_arg(args, "inputs");
    if (inputs.empty()) {
        inputs = json_list_arg(args, "");
    }
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    for (const auto& input : inputs) {
        uint64_t value = 0;
        bool ok = false;
        if (input.rfind("ascii:", 0) == 0) {
            ok = true;
            std::string ascii = input.substr(6);
            for (size_t i = 0; i < ascii.size() && i < 8; ++i) {
                value |= static_cast<uint64_t>(static_cast<unsigned char>(ascii[i])) << (i * 8);
            }
        } else if (input.rfind("bytes:", 0) == 0 || input.rfind("bytes_le:", 0) == 0) {
            ok = true;
            std::string bytes = input.substr(input.find(':') + 1);
            size_t index = 0;
            size_t pos = 0;
            while (pos < bytes.size() && index < 8) {
                while (pos < bytes.size() && (bytes[pos] == ' ' || bytes[pos] == ',' || bytes[pos] == '-')) ++pos;
                size_t end = pos;
                while (end < bytes.size() && bytes[end] != ' ' && bytes[end] != ',' && bytes[end] != '-') ++end;
                uint64_t byte = 0;
                if (parse_u64_text(bytes.substr(pos, end - pos), &byte) && byte <= 0xff) {
                    value |= byte << (index * 8);
                } else {
                    ok = false;
                    break;
                }
                ++index;
                pos = end;
            }
        } else {
            ok = parse_u64_text(input, &value);
        }
        if (!first) out << ",";
        first = false;
        out << "{\"input\":" << json_string(input);
        if (ok) {
            std::ostringstream hex;
            hex << "0x" << std::hex << value;
            std::string binary = "0b";
            bool seen_one = false;
            for (int bit = 63; bit >= 0; --bit) {
                bool set = (value & (uint64_t{1} << bit)) != 0;
                if (set || seen_one || bit == 0) {
                    binary.push_back(set ? '1' : '0');
                    seen_one = true;
                }
            }
            std::string ascii;
            std::array<uint8_t, 8> bytes{static_cast<uint8_t>(value), static_cast<uint8_t>(value >> 8), static_cast<uint8_t>(value >> 16), static_cast<uint8_t>(value >> 24), static_cast<uint8_t>(value >> 32), static_cast<uint8_t>(value >> 40), static_cast<uint8_t>(value >> 48), static_cast<uint8_t>(value >> 56)};
            for (uint8_t byte : bytes) {
                if (byte == 0) break;
                ascii.push_back((byte >= 0x20 && byte < 0x7f) ? static_cast<char>(byte) : '.');
            }
            out << ",\"decimal\":" << json_string(std::to_string(value))
                << ",\"hex\":" << json_string(hex.str())
                << ",\"binary\":" << json_string(binary)
                << ",\"bytes_le\":[";
            for (size_t i = 0; i < bytes.size(); ++i) {
                if (i != 0) out << ",";
                out << static_cast<unsigned int>(bytes[i]);
            }
            out << "]"
                << ",\"ascii\":" << json_string(ascii);
        } else {
            out << ",\"error\":\"not a supported integer, hex, binary, bytes, or ASCII representation\"";
        }
        out << "}";
    }
    out << "],\"count\":" << inputs.size() << "}";
    return out.str();
}

std::string core_lookup_funcs(const std::string& args) {
    std::vector<std::string> queries = json_list_arg(args, "queries");
    uint64_t runtime_module_base = json_u64_arg(args, "runtime_module_base", 0);
    uint64_t ida_image_base = json_u64_arg(args, "ida_image_base", 0);
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    for (const auto& query : queries) {
        uint64_t address = 0;
        bool is_address = parse_u64_text(query, &address);
        ea_t ea = BADADDR;
        if (is_address) {
            if (address >= runtime_module_base && ida_image_base <= std::numeric_limits<uint64_t>::max() - (address - runtime_module_base)) {
                ea = static_cast<ea_t>(ida_image_base + (address - runtime_module_base));
            }
        } else {
            ea = get_name_ea(BADADDR, query.c_str());
        }
        func_t* function = ea == BADADDR ? nullptr : get_func(ea);
        if (!first) out << ",";
        first = false;
        out << "{\"query\":" << json_string(query)
            << ",\"input_type\":" << json_string(is_address ? "address" : "name")
            << ",\"found\":" << (function == nullptr ? "false" : "true")
            << ",\"ida_ea\":" << json_nullable_u64(ea)
            << ",\"function\":" << function_json(function) << "}";
    }
    out << "],\"count\":" << queries.size() << "}";
    return out.str();
}

std::string core_list_funcs(const std::string& args) {
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = std::min<uint64_t>(json_u64_arg(args, "count", 50), 1000);
    std::string filter = json_string_arg(args, "filter");
    std::ostringstream out;
    out << "{\"offset\":" << offset << ",\"items\":[";
    size_t total = 0;
    size_t emitted = 0;
    bool first = true;
    size_t qty = get_func_qty();
    for (size_t i = 0; i < qty; ++i) {
        func_t* function = getn_func(i);
        std::string item = function_json(function);
        if (!filter.empty()) {
            std::string haystack = item;
            std::transform(haystack.begin(), haystack.end(), haystack.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
            std::string needle = filter;
            std::transform(needle.begin(), needle.end(), needle.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
            if (haystack.find(needle) == std::string::npos) continue;
        }
        if (total++ < offset) continue;
        if (emitted >= count) continue;
        if (!first) out << ",";
        first = false;
        out << item;
        ++emitted;
    }
    out << "],\"count\":" << emitted << ",\"total\":" << total << "}";
    return out.str();
}

std::string core_list_globals(const std::string& args) {
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = std::min<uint64_t>(json_u64_arg(args, "count", 50), 1000);
    std::string filter = json_string_arg(args, "filter");
    std::ostringstream out;
    out << "{\"offset\":" << offset << ",\"items\":[";
    size_t total = 0;
    size_t emitted = 0;
    bool first = true;
    size_t qty = get_nlist_size();
    for (size_t i = 0; i < qty; ++i) {
        ea_t ea = get_nlist_ea(i);
        if (ea == BADADDR || get_func(ea) != nullptr) {
            continue;
        }
        const char* name = get_nlist_name(i);
        std::string name_text = name == nullptr ? std::string() : std::string(name);
        if (!filter.empty()) {
            std::string haystack = name_text;
            std::transform(haystack.begin(), haystack.end(), haystack.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
            std::string needle = filter;
            std::transform(needle.begin(), needle.end(), needle.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
            if (haystack.find(needle) == std::string::npos) continue;
        }
        if (total++ < offset) continue;
        if (emitted >= count) continue;
        if (!first) out << ",";
        first = false;
        out << "{\"address\":" << json_u64(static_cast<uint64_t>(ea))
            << ",\"name\":" << json_string(name_text) << "}";
        ++emitted;
    }
    out << "],\"count\":" << emitted << ",\"total\":" << total << "}";
    return out.str();
}

struct ImportCollectContext {
    std::ostringstream* out;
    size_t* total;
    size_t* emitted;
    uint64_t offset;
    uint64_t count;
    std::string module;
    std::string filter;
    bool* first;
};

int idaapi collect_import_cb(ea_t ea, const char* name, uval_t ordinal, void* param) {
    auto* ctx = static_cast<ImportCollectContext*>(param);
    if (!ctx->filter.empty()) {
        std::string haystack = ctx->module;
        if (name != nullptr) {
            haystack.append(" ");
            haystack.append(name);
        }
        std::transform(haystack.begin(), haystack.end(), haystack.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
        if (haystack.find(ctx->filter) == std::string::npos) {
            return 1;
        }
    }
    size_t index = (*ctx->total)++;
    if (index < ctx->offset || *ctx->emitted >= ctx->count) {
        return 1;
    }
    if (!*ctx->first) *ctx->out << ",";
    *ctx->first = false;
    *ctx->out << "{\"module\":" << json_string(ctx->module)
              << ",\"name\":" << (name == nullptr ? "null" : json_string(name))
              << ",\"ordinal\":" << (ordinal == 0 ? std::string("null") : std::to_string(static_cast<uint64_t>(ordinal)))
              << ",\"iat_ea\":" << json_u64(static_cast<uint64_t>(ea)) << "}";
    ++*ctx->emitted;
    return 1;
}

std::string core_imports(const std::string& args) {
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = std::min<uint64_t>(json_u64_arg(args, "count", 50), 1000);
    std::string filter = json_string_arg(args, "filter");
    std::transform(filter.begin(), filter.end(), filter.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    std::ostringstream out;
    out << "{\"offset\":" << offset << ",\"items\":[";
    size_t total = 0;
    size_t emitted = 0;
    bool first = true;
    uint modules = get_import_module_qty();
    for (uint i = 0; i < modules; ++i) {
        qstring module_name;
        get_import_module_name(&module_name, static_cast<int>(i));
        ImportCollectContext ctx{&out, &total, &emitted, offset, count, qstring_to_string(module_name), filter, &first};
        enum_import_names(static_cast<int>(i), collect_import_cb, &ctx);
    }
    out << "],\"count\":" << emitted << ",\"total\":" << total << "}";
    return out.str();
}

std::string core_decompile(const std::string& args) {
    uint64_t addr = required_json_u64_arg(args, "addr");
    func_t* function = get_func(static_cast<ea_t>(addr));
    if (function == nullptr) {
        return "{\"found\":false,\"pseudocode\":null,\"error\":\"function not found\"}";
    }

    if (!init_hexrays_plugin()) {
        throw std::runtime_error("Hex-Rays decompiler is not available");
    }

    hexrays_failure_t failure;
    cfuncptr_t cfunc = decompile_func(function, &failure, DECOMP_WARNINGS);
    if (cfunc == nullptr) {
        std::string message = failure.code == MERR_LICENSE
            ? "Decompiler license is not available"
            : "Hex-Rays decompilation failed";
        qstring details = failure.desc();
        if (!details.empty()) {
            message += ": ";
            message += qstring_to_string(details);
        }
        if (failure.errea != BADADDR) {
            std::ostringstream address;
            address << " (address: 0x" << std::hex << static_cast<uint64_t>(failure.errea) << ")";
            message += address.str();
        }
        throw std::runtime_error(message);
    }

    std::ostringstream text;
    const strvec_t& lines = cfunc->get_pseudocode();
    for (size_t index = 0; index < lines.size(); ++index) {
        qstring line;
        tag_remove(&line, lines[index].line);
        text << qstring_to_string(line);
        if (index + 1 < lines.size()) {
            text << "\n";
        }
    }
    return "{\"found\":true,\"function\":" + function_json(function) + ",\"language\":\"c\",\"pseudocode\":" + json_string(text.str()) + "}";
}

std::string core_disasm(const std::string& args) {
    uint64_t addr = required_json_u64_arg(args, "addr");
    func_t* function = get_func(static_cast<ea_t>(addr));
    if (function == nullptr) {
        return "{\"found\":false,\"instructions\":[],\"warnings\":[\"function not found\"]}";
    }
    std::ostringstream out;
    out << "{\"found\":true,\"function\":" << function_json(function)
        << ",\"arguments\":[],\"stack_frame\":{\"size\":" << json_u64(static_cast<uint64_t>(get_frame_size(function)))
        << "},\"instructions\":[";
    bool first = true;
    size_t emitted = 0;
    for (ea_t ea = function->start_ea; ea < function->end_ea && emitted < 256;) {
        qstring line;
        if (generate_disasm_line(&line, ea, GENDSM_REMOVE_TAGS)) {
            if (!first) out << ",";
            first = false;
            out << "{\"ea\":" << json_u64(static_cast<uint64_t>(ea))
                << ",\"text\":" << json_string(qstring_to_string(line)) << "}";
            ++emitted;
        }
        asize_t size = get_item_size(ea);
        ea += size == 0 ? 1 : size;
    }
    out << "]}";
    return out.str();
}

std::string core_xrefs_to(const std::string& args) {
    std::vector<std::string> addrs = json_list_arg(args, "addrs");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first_item = true;
    for (const auto& text : addrs) {
        uint64_t value = 0;
        if (!parse_u64_text(text, &value)) {
            throw std::invalid_argument("xrefs_to addrs must contain only addresses");
        }
        ea_t to = static_cast<ea_t>(value);
        if (!first_item) out << ",";
        first_item = false;
        out << "{\"to\":" << json_u64(value) << ",\"xrefs\":[";
        bool first = true;
        for (ea_t from = get_first_cref_to(to); from != BADADDR; from = get_next_cref_to(to, from)) {
            if (!first) out << ",";
            first = false;
            func_t* function = get_func(from);
            out << "{\"from\":" << json_u64(static_cast<uint64_t>(from))
                << ",\"type\":\"code\",\"function\":" << function_json(function) << "}";
        }
        out << "]}";
    }
    out << "],\"count\":" << addrs.size() << "}";
    return out.str();
}

std::string core_xrefs_to_field(const std::string& args) {
    std::vector<std::string> queries = json_list_arg(args, "queries");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first_item = true;
    for (const auto& query : queries) {
        if (!first_item) out << ",";
        first_item = false;
        std::string field = query;
        size_t dot = field.rfind('.');
        if (dot != std::string::npos && dot + 1 < field.size()) {
            field = field.substr(dot + 1);
        }
        std::string query_lower = query;
        std::string field_lower = field;
        std::transform(query_lower.begin(), query_lower.end(), query_lower.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
        std::transform(field_lower.begin(), field_lower.end(), field_lower.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
        out << "{\"query\":" << json_string(query)
            << ",\"match\":\"disasm_text\""
            << ",\"xrefs\":[";
        bool first = true;
        size_t qty = get_func_qty();
        for (size_t i = 0; i < qty; ++i) {
            func_t* function = getn_func(i);
            if (function == nullptr) {
                continue;
            }
            for (ea_t ea = function->start_ea; ea < function->end_ea;) {
                qstring line;
                if (generate_disasm_line(&line, ea, GENDSM_REMOVE_TAGS)) {
                    std::string text = qstring_to_string(line);
                    std::string lower = text;
                    std::transform(lower.begin(), lower.end(), lower.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
                    if ((!query_lower.empty() && lower.find(query_lower) != std::string::npos)
                        || (!field_lower.empty() && lower.find(field_lower) != std::string::npos)) {
                        if (!first) out << ",";
                        first = false;
                        out << "{\"from\":" << json_u64(static_cast<uint64_t>(ea))
                            << ",\"type\":\"struct_field_text\",\"text\":" << json_string(text)
                            << ",\"function\":" << function_json(function) << "}";
                    }
                }
                asize_t size = get_item_size(ea);
                ea += size == 0 ? 1 : size;
            }
        }
        out << "]}";
    }
    out << "],\"count\":" << queries.size() << "}";
    return out.str();
}

std::string core_callees(const std::string& args) {
    std::vector<std::string> addrs = json_list_arg(args, "addrs");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first_item = true;
    for (const auto& text : addrs) {
        uint64_t value = 0;
        if (!parse_u64_text(text, &value)) {
            throw std::invalid_argument("callees addrs must contain only addresses");
        }
        func_t* function = get_func(static_cast<ea_t>(value));
        if (!first_item) out << ",";
        first_item = false;
        out << "{\"function\":" << function_json(function) << ",\"callees\":[";
        bool first = true;
        if (function != nullptr) {
            for (ea_t ea = function->start_ea; ea < function->end_ea;) {
                for (ea_t to = get_first_cref_from(ea); to != BADADDR; to = get_next_cref_from(ea, to)) {
                    func_t* callee = get_func(to);
                    if (callee != nullptr && callee->start_ea != function->start_ea) {
                        if (!first) out << ",";
                        first = false;
                        out << function_json(callee);
                    }
                }
                asize_t size = get_item_size(ea);
                ea += size == 0 ? 1 : size;
            }
        }
        out << "]}";
    }
    out << "],\"count\":" << addrs.size() << "}";
    return out.str();
}

std::string execute_core_function(const std::string& function, const std::string& args) {
    if (function == "lookup_funcs") return core_lookup_funcs(args);
    if (function == "int_convert") return core_int_convert(args);
    if (function == "list_funcs") return core_list_funcs(args);
    if (function == "list_globals") return core_list_globals(args);
    if (function == "imports") return core_imports(args);
    if (function == "decompile") return core_decompile(args);
    if (function == "disasm") return core_disasm(args);
    if (function == "xrefs_to") return core_xrefs_to(args);
    if (function == "xrefs_to_field") return core_xrefs_to_field(args);
    if (function == "callees") return core_callees(args);
    throw std::invalid_argument("unsupported IDA Core Function `" + function + "`");
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
        if (handle->database_open) {
            close_database(false);
            handle->database_open = false;
        }
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
        validate_ida_install_dir(install_dir);
        int init_result = init_library(0, nullptr);
        if (init_result != 0) {
            return fail(DA_IDA_ERR_IDA, "init_library failed with result " + std::to_string(init_result));
        }
        int open_result = open_database(database_path_utf8_copy.c_str(), true, nullptr);
        if (open_result != 0) {
            return fail(DA_IDA_ERR_IDA, "open_database failed with result " + std::to_string(open_result));
        }
        if (!auto_wait()) {
            close_database(false);
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

        func_t* function = get_func(static_cast<ea_t>(ida_ea));
        if (function == nullptr) {
            return DA_IDA_OK;
        }

        qstring name;
        ssize_t name_len = get_func_name(&name, static_cast<ea_t>(ida_ea));
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

DA_IDA_EXPORT int32_t da_ida_core_function(
    DA_IdaSessionHandle* handle,
    const char* function_utf8,
    const char* arguments_json_utf8,
    DA_IdaCoreResult* out) {
    if (handle == nullptr || function_utf8 == nullptr || arguments_json_utf8 == nullptr
        || out == nullptr || out->struct_size < sizeof(DA_IdaCoreResult)) {
        return fail(DA_IDA_ERR_INVALID_ARGUMENT, "core function arguments are invalid");
    }
    try {
        auto* impl = reinterpret_cast<IdaSessionHandleImpl*>(handle);
        ensure_owner_thread(impl);
        std::string function = utf8_string(function_utf8, "function");
        std::string arguments_json = utf8_string(arguments_json_utf8, "arguments_json");
        std::string result_json = execute_core_function(function, arguments_json);
        out->flags = 0;
        out->result_json = make_text_view(result_json);
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

DA_IDA_EXPORT int32_t da_ida_core_function(
    DA_IdaSessionHandle* handle,
    const char* function_utf8,
    const char* arguments_json_utf8,
    DA_IdaCoreResult* out) {
    (void)handle;
    (void)function_utf8;
    (void)arguments_json_utf8;
    (void)out;
    return DA_IDA_ERR_IDA;
}

DA_IDA_EXPORT int32_t da_ida_session_close(DA_IdaSessionHandle* handle) {
    (void)handle;
    return DA_IDA_OK;
}

#endif
