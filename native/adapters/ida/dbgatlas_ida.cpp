#define NOMINMAX
#include "dbgatlas_ida.h"
#include "dbgatlas_ida_runtime.h"

#include <cstring>

#ifdef _WIN32
#include <windows.h>

#include <algorithm>
#include <array>
#include <cctype>
#include <limits>
#include <memory>
#include <mutex>
#include <regex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace {

thread_local std::string g_last_error;
std::mutex g_session_mutex;
bool g_active_session = false;

struct IdaSessionHandleImpl {
    std::thread::id owner_thread;
    bool database_open = false;
    bool strings_built = false;
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
    const char* text = value.c_str();
    return text == nullptr ? std::string() : std::string(text, value.length());
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
    if (json[pos] == '{') {
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
            } else if (ch == '{') {
                ++depth;
            } else if (ch == '}') {
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

bool optional_json_u64_arg(const std::string& args, const std::string& key, uint64_t* out) {
    std::string value = find_json_value(args, key);
    if (value.empty()) {
        return false;
    }
    value = unquote_json_string(value);
    uint64_t parsed = 0;
    if (!parse_u64_text(value, &parsed)) {
        throw std::invalid_argument(key + " must be an unsigned integer or address string");
    }
    *out = parsed;
    return true;
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

std::vector<std::string> json_value_items_arg(const std::string& args, const std::string& key) {
    std::string value = trim_copy(find_json_value(args, key));
    std::vector<std::string> result;
    if (value.empty() || value == "null") {
        return result;
    }
    if (value.front() != '[') {
        result.push_back(value);
        return result;
    }

    size_t pos = 1;
    while (pos < value.size() && value[pos] != ']') {
        while (pos < value.size() && (std::isspace(static_cast<unsigned char>(value[pos])) != 0 || value[pos] == ',')) {
            ++pos;
        }
        if (pos >= value.size() || value[pos] == ']') {
            break;
        }

        size_t start = pos;
        if (value[pos] == '"' || value[pos] == '{' || value[pos] == '[') {
            char open = value[pos];
            char close = open == '{' ? '}' : (open == '[' ? ']' : '"');
            int depth = open == '"' ? 0 : 1;
            bool in_string = open == '"';
            bool escape = false;
            ++pos;
            for (; pos < value.size(); ++pos) {
                char ch = value[pos];
                if (in_string) {
                    if (escape) {
                        escape = false;
                    } else if (ch == '\\') {
                        escape = true;
                    } else if (ch == '"') {
                        if (open == '"') {
                            ++pos;
                            break;
                        }
                        in_string = false;
                    }
                } else if (ch == '"') {
                    in_string = true;
                } else if ((open == '{' && ch == '{') || (open == '[' && ch == '[')) {
                    ++depth;
                } else if (ch == close) {
                    --depth;
                    if (depth == 0) {
                        ++pos;
                        break;
                    }
                }
            }
        } else {
            while (pos < value.size() && value[pos] != ',' && value[pos] != ']') {
                ++pos;
            }
        }
        std::string item = trim_copy(value.substr(start, pos - start));
        if (!item.empty()) {
            result.push_back(item);
        }
    }
    return result;
}

bool json_bool_arg(const std::string& args, const std::string& key, bool fallback) {
    std::string value = unquote_json_string(find_json_value(args, key));
    std::transform(value.begin(), value.end(), value.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    if (value == "true" || value == "1") {
        return true;
    }
    if (value == "false" || value == "0") {
        return false;
    }
    return fallback;
}

std::string json_object_string_arg(const std::string& object, const std::string& key) {
    return unquote_json_string(find_json_value(object, key));
}

bool optional_json_object_u64_arg(const std::string& object, const std::string& key, uint64_t* out) {
    std::string value = find_json_value(object, key);
    if (value.empty()) {
        return false;
    }
    value = unquote_json_string(value);
    if (!parse_u64_text(value, out)) {
        throw std::invalid_argument(key + " must be an unsigned integer or address string");
    }
    return true;
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

std::string bytes_hex(const std::vector<uint8_t>& bytes) {
    static constexpr char hex[] = "0123456789abcdef";
    std::string out;
    out.reserve(bytes.size() * 2);
    for (uint8_t byte : bytes) {
        out.push_back(hex[(byte >> 4) & 0xf]);
        out.push_back(hex[byte & 0xf]);
    }
    return out;
}

std::string to_lower_copy(std::string value) {
    std::transform(value.begin(), value.end(), value.begin(), [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    return value;
}

void ensure_string_list(IdaSessionHandleImpl* handle) {
    if (!handle->strings_built) {
        build_strlist();
        handle->strings_built = true;
    }
}

bool get_string_list_item_at(ea_t ea, string_info_t* out) {
    size_t qty = get_strlist_qty();
    for (size_t i = 0; i < qty; ++i) {
        string_info_t item;
        if (get_strlist_item(&item, i) && item.ea == ea) {
            *out = item;
            return true;
        }
    }
    return false;
}

std::string string_info_json(const string_info_t& item, const std::string& text) {
    std::ostringstream out;
    out << "{\"address\":" << json_u64(static_cast<uint64_t>(item.ea))
        << ",\"length\":" << json_u64(item.length > 0 ? static_cast<uint64_t>(item.length) : 0)
        << ",\"type\":" << item.type
        << ",\"text\":" << json_string(text) << "}";
    return out.str();
}

bool read_string_text(ea_t ea, size_t length, int32 type, std::string* out) {
    qstring text;
    ssize_t result = get_strlit_contents(&text, ea, length, type, nullptr, STRCONV_ESCAPE);
    if (result < 0) {
        return false;
    }
    *out = qstring_to_string(text);
    return true;
}

std::vector<uint8_t> read_idb_bytes(ea_t ea, uint64_t length) {
    if (length == 0 || length > 4096) {
        throw std::invalid_argument("length must be between 1 and 4096 bytes");
    }
    std::vector<uint8_t> bytes;
    bytes.reserve(static_cast<size_t>(length));
    for (uint64_t i = 0; i < length; ++i) {
        uint8_t byte = 0;
        ssize_t read = get_bytes(&byte, 1, ea + static_cast<ea_t>(i));
        if (read != 1) {
            break;
        }
        bytes.push_back(byte);
    }
    return bytes;
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

std::string core_list_strings(IdaSessionHandleImpl* handle, const std::string& args) {
    ensure_string_list(handle);
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = std::min<uint64_t>(json_u64_arg(args, "count", 50), 1000);
    std::string filter = to_lower_copy(json_string_arg(args, "filter"));
    std::ostringstream out;
    out << "{\"offset\":" << offset << ",\"items\":[";
    size_t total = 0;
    size_t emitted = 0;
    bool first = true;
    size_t qty = get_strlist_qty();
    for (size_t i = 0; i < qty; ++i) {
        string_info_t item;
        if (!get_strlist_item(&item, i)) {
            continue;
        }
        size_t length = item.length > 0 ? static_cast<size_t>(item.length) : size_t(-1);
        if (length != size_t(-1)) {
            length = std::min<size_t>(length, 4096);
        }
        std::string text;
        if (!read_string_text(item.ea, length, item.type, &text)) {
            continue;
        }
        if (!filter.empty() && to_lower_copy(text).find(filter) == std::string::npos) {
            continue;
        }
        if (total++ < offset) {
            continue;
        }
        if (emitted >= count) {
            continue;
        }
        if (!first) out << ",";
        first = false;
        out << string_info_json(item, text);
        ++emitted;
    }
    out << "],\"count\":" << emitted << ",\"total\":" << total << "}";
    return out.str();
}

std::string core_get_string(IdaSessionHandleImpl* handle, const std::string& args) {
    ensure_string_list(handle);
    uint64_t addr = required_json_u64_arg(args, "addr");
    ea_t ea = static_cast<ea_t>(addr);

    string_info_t item;
    bool from_list = get_string_list_item_at(ea, &item);
    uint64_t length_arg = 0;
    bool has_length = optional_json_u64_arg(args, "length", &length_arg);
    if (has_length) {
        if (length_arg == 0 || length_arg > 4096) {
            throw std::invalid_argument("length must be between 1 and 4096 bytes");
        }
    }
    uint64_t type_arg = 0;
    bool has_type = optional_json_u64_arg(args, "type", &type_arg);

    size_t length = size_t(-1);
    int32 type = STRTYPE_C;
    if (from_list) {
        length = item.length > 0 ? static_cast<size_t>(item.length) : size_t(-1);
        type = item.type;
    }
    if (has_length) {
        length = static_cast<size_t>(length_arg);
    } else if (length != size_t(-1)) {
        length = std::min<size_t>(length, 4096);
    }
    if (has_type) {
        type = static_cast<int32>(type_arg);
    } else if (!from_list) {
        type = static_cast<int32>(get_str_type(ea));
    }

    std::string text;
    bool found = read_string_text(ea, length, type, &text);
    std::ostringstream out;
    out << "{\"address\":" << json_u64(addr)
        << ",\"found\":" << (found ? "true" : "false")
        << ",\"length\":" << (length == size_t(-1) ? "null" : json_u64(static_cast<uint64_t>(length)))
        << ",\"type\":" << type;
    if (found) {
        out << ",\"text\":" << json_string(text);
    } else {
        out << ",\"text\":null";
    }
    out << "}";
    return out.str();
}

std::string core_get_bytes(const std::string& args) {
    uint64_t addr = required_json_u64_arg(args, "addr");
    uint64_t length = required_json_u64_arg(args, "length");
    std::vector<uint8_t> bytes = read_idb_bytes(static_cast<ea_t>(addr), length);
    std::ostringstream out;
    out << "{\"address\":" << json_u64(addr)
        << ",\"requested_length\":" << json_u64(length)
        << ",\"read_length\":" << json_u64(static_cast<uint64_t>(bytes.size()))
        << ",\"complete\":" << (bytes.size() == length ? "true" : "false")
        << ",\"bytes_hex\":" << json_string(bytes_hex(bytes)) << "}";
    return out.str();
}

std::string core_get_int(const std::string& args) {
    uint64_t addr = required_json_u64_arg(args, "addr");
    uint64_t size = 8;
    optional_json_u64_arg(args, "size", &size);
    if (size != 1 && size != 2 && size != 4 && size != 8) {
        throw std::invalid_argument("size must be one of 1, 2, 4, or 8 bytes");
    }
    std::string endian = json_string_arg(args, "endian");
    if (endian.empty()) {
        endian = "little";
    }
    if (endian != "little" && endian != "big") {
        throw std::invalid_argument("endian must be `little` or `big`");
    }

    std::vector<uint8_t> bytes = read_idb_bytes(static_cast<ea_t>(addr), size);
    bool complete = bytes.size() == size;
    std::ostringstream out;
    out << "{\"address\":" << json_u64(addr)
        << ",\"size\":" << json_u64(size)
        << ",\"endian\":" << json_string(endian)
        << ",\"complete\":" << (complete ? "true" : "false")
        << ",\"bytes_hex\":" << json_string(bytes_hex(bytes));
    if (complete) {
        uint64_t value = 0;
        if (endian == "little") {
            for (size_t i = 0; i < bytes.size(); ++i) {
                value |= static_cast<uint64_t>(bytes[i]) << (i * 8);
            }
        } else {
            for (uint8_t byte : bytes) {
                value = (value << 8) | byte;
            }
        }
        std::ostringstream hex;
        hex << "0x" << std::hex << value;
        out << ",\"decimal\":" << json_string(std::to_string(value))
            << ",\"hex\":" << json_string(hex.str());
    } else {
        out << ",\"decimal\":null,\"hex\":null";
    }
    out << "}";
    return out.str();
}

std::string core_decompile(const std::string& args) {
    uint64_t addr = required_json_u64_arg(args, "addr");
    func_t* function = get_func(static_cast<ea_t>(addr));
    if (function == nullptr) {
        return "{\"found\":false,\"pseudocode\":null,\"error\":\"function not found\"}";
    }

    if (!dbgatlas_init_hexrays_plugin()) {
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
        asize_t size = dbgatlas_get_item_size(ea);
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
                asize_t size = dbgatlas_get_item_size(ea);
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
                asize_t size = dbgatlas_get_item_size(ea);
                ea += size == 0 ? 1 : size;
            }
        }
        out << "]}";
    }
    out << "],\"count\":" << addrs.size() << "}";
    return out.str();
}

ea_t resolve_addr_or_name(const std::string& object, bool required = true) {
    uint64_t addr = 0;
    if (optional_json_object_u64_arg(object, "addr", &addr)) {
        return static_cast<ea_t>(addr);
    }
    std::string name = json_object_string_arg(object, "name");
    if (!name.empty()) {
        return get_name_ea(BADADDR, name.c_str());
    }
    if (required) {
        throw std::invalid_argument("item requires addr or name");
    }
    return BADADDR;
}

ea_t resolve_query_addr_or_name(const std::string& query) {
    uint64_t addr = 0;
    if (parse_u64_text(query, &addr)) {
        return static_cast<ea_t>(addr);
    }
    return get_name_ea(BADADDR, query.c_str());
}

std::string current_name_or_placeholder(ea_t ea) {
    qstring name = get_name(ea);
    std::string text = qstring_to_string(name);
    if (!text.empty()) {
        return text;
    }
    std::ostringstream fallback;
    fallback << "dbgatlas_" << std::hex << static_cast<uint64_t>(ea);
    return fallback.str();
}

std::string make_decl_for_ea(ea_t ea, const std::string& type_text) {
    std::string trimmed = trim_copy(type_text);
    if (trimmed.empty()) {
        return trimmed;
    }
    if (trimmed.find(';') != std::string::npos) {
        return trimmed;
    }
    std::string name = current_name_or_placeholder(ea);
    if (trimmed.find(name) != std::string::npos) {
        return trimmed + ";";
    }
    return trimmed + " " + name + ";";
}

std::string paged_result_json(const std::vector<std::string>& rows, uint64_t offset, uint64_t count) {
    uint64_t limit = std::min<uint64_t>(count == 0 ? 50 : count, 1000);
    std::ostringstream out;
    out << "{\"offset\":" << json_u64(offset) << ",\"items\":[";
    uint64_t emitted = 0;
    bool first = true;
    for (uint64_t i = offset; i < rows.size() && emitted < limit; ++i) {
        if (!first) out << ",";
        first = false;
        out << rows[static_cast<size_t>(i)];
        ++emitted;
    }
    uint64_t next = offset + emitted;
    out << "],\"count\":" << json_u64(emitted)
        << ",\"total\":" << json_u64(static_cast<uint64_t>(rows.size()))
        << ",\"next_offset\":";
    if (next < rows.size()) {
        out << json_u64(next);
    } else {
        out << "null";
    }
    out << "}";
    return out.str();
}

std::string core_rename(const std::string& args) {
    std::vector<std::string> items = json_value_items_arg(args, "items");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    size_t changed = 0;
    for (const auto& item : items) {
        std::string kind = json_object_string_arg(item, "kind");
        std::string new_name = json_object_string_arg(item, "new_name");
        ea_t ea = BADADDR;
        std::string error;
        bool ok = false;
        try {
            ea = resolve_addr_or_name(item);
            if (kind.empty()) kind = "addr";
            if (kind != "function" && kind != "global") {
                throw std::invalid_argument("kind must be `function` or `global`");
            }
            if (new_name.empty()) {
                throw std::invalid_argument("new_name is required");
            }
            if (kind == "function") {
                func_t* function = get_func(ea);
                if (function == nullptr) {
                    throw std::invalid_argument("addr/name does not resolve to a function");
                }
                ea = function->start_ea;
            }
            ok = set_name(ea, new_name.c_str(), SN_FORCE | SN_NOCHECK);
            if (!ok) {
                error = "set_name failed";
            } else {
                ++changed;
            }
        } catch (const std::exception& ex) {
            error = ex.what();
        }
        if (!first) out << ",";
        first = false;
        out << "{\"kind\":" << json_string(kind)
            << ",\"ea\":" << json_nullable_u64(ea)
            << ",\"new_name\":" << json_string(new_name)
            << ",\"ok\":" << (ok ? "true" : "false");
        if (!error.empty()) out << ",\"error\":" << json_string(error);
        out << "}";
    }
    out << "],\"count\":" << items.size() << ",\"changed_count\":" << changed << "}";
    return out.str();
}

std::string core_set_comments(const std::string& args) {
    std::vector<std::string> items = json_value_items_arg(args, "items");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    size_t changed = 0;
    for (const auto& item : items) {
        ea_t ea = BADADDR;
        std::string text = json_object_string_arg(item, "text");
        bool repeatable = json_bool_arg(item, "repeatable", false);
        bool ok = false;
        std::string error;
        try {
            ea = resolve_addr_or_name(item);
            ok = set_cmt(ea, text.c_str(), repeatable);
            if (!ok) {
                error = "set_cmt failed";
            } else {
                ++changed;
            }
        } catch (const std::exception& ex) {
            error = ex.what();
        }
        if (!first) out << ",";
        first = false;
        out << "{\"ea\":" << json_nullable_u64(ea)
            << ",\"repeatable\":" << (repeatable ? "true" : "false")
            << ",\"ok\":" << (ok ? "true" : "false");
        if (!error.empty()) out << ",\"error\":" << json_string(error);
        out << "}";
    }
    out << "],\"count\":" << items.size() << ",\"changed_count\":" << changed << "}";
    return out.str();
}

std::string core_set_type(const std::string& args) {
    std::vector<std::string> items = json_value_items_arg(args, "items");
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    size_t changed = 0;
    for (const auto& item : items) {
        std::string kind = json_object_string_arg(item, "kind");
        std::string type_text = json_object_string_arg(item, "type");
        ea_t ea = BADADDR;
        bool ok = false;
        std::string error;
        try {
            ea = resolve_addr_or_name(item);
            if (kind.empty()) kind = "addr";
            if (kind != "function" && kind != "global" && kind != "addr") {
                throw std::invalid_argument("kind must be `function`, `global`, or `addr`");
            }
            if (kind == "function") {
                func_t* function = get_func(ea);
                if (function == nullptr) {
                    throw std::invalid_argument("addr/name does not resolve to a function");
                }
                ea = function->start_ea;
            }
            std::string decl = make_decl_for_ea(ea, type_text);
            if (decl.empty()) {
                throw std::invalid_argument("type is required");
            }
            ok = apply_cdecl(get_idati(), ea, decl.c_str(), TINFO_DEFINITE);
            if (!ok) {
                error = "apply_cdecl failed";
            } else {
                ++changed;
                if (func_t* function = get_func(ea)) {
                    mark_cfunc_dirty(function->start_ea, false);
                }
            }
        } catch (const std::exception& ex) {
            error = ex.what();
        }
        if (!first) out << ",";
        first = false;
        out << "{\"kind\":" << json_string(kind)
            << ",\"ea\":" << json_nullable_u64(ea)
            << ",\"ok\":" << (ok ? "true" : "false");
        if (!error.empty()) out << ",\"error\":" << json_string(error);
        out << "}";
    }
    out << "],\"count\":" << items.size() << ",\"changed_count\":" << changed << "}";
    return out.str();
}

std::string core_declare_type(const std::string& args) {
    std::vector<std::string> decls;
    std::string raw_decls = trim_copy(find_json_value(args, "decls"));
    if (!raw_decls.empty() && raw_decls.front() == '[') {
        decls = json_list_arg(args, "decls");
    } else {
        std::string scalar = unquote_json_string(raw_decls);
        if (!scalar.empty()) {
            decls.push_back(scalar);
        }
    }
    std::ostringstream input;
    for (const auto& decl : decls) {
        input << decl;
        if (!decl.empty() && decl.back() != ';') {
            input << ';';
        }
        input << '\n';
    }
    int errors = parse_decls(get_idati(), input.str().c_str(), nullptr, HTI_DCL | HTI_NWR);
    bool ok = errors == 0;
    return std::string("{\"ok\":") + (ok ? "true" : "false")
        + ",\"count\":" + std::to_string(decls.size())
        + ",\"changed_count\":" + (ok ? std::to_string(decls.size()) : "0")
        + ",\"errors\":" + std::to_string(errors) + "}";
}

std::string core_force_recompile(const std::string& args) {
    if (!dbgatlas_init_hexrays_plugin()) {
        throw std::runtime_error("Hex-Rays decompiler is not available");
    }
    std::vector<std::string> addrs = json_list_arg(args, "addrs");
    if (addrs.empty()) {
        clear_cached_cfuncs();
        return "{\"items\":[],\"count\":0,\"changed_count\":0,\"all\":true}";
    }
    std::ostringstream out;
    out << "{\"items\":[";
    bool first = true;
    size_t changed = 0;
    for (const auto& text : addrs) {
        ea_t ea = resolve_query_addr_or_name(text);
        func_t* function = ea == BADADDR ? nullptr : get_func(ea);
        bool ok = false;
        std::string error;
        if (function == nullptr) {
            error = "function not found";
        } else {
            ok = mark_cfunc_dirty(function->start_ea, false);
            if (ok) ++changed;
            else error = "mark_cfunc_dirty failed";
        }
        if (!first) out << ",";
        first = false;
        out << "{\"query\":" << json_string(text)
            << ",\"ea\":" << json_nullable_u64(function == nullptr ? BADADDR : function->start_ea)
            << ",\"ok\":" << (ok ? "true" : "false");
        if (!error.empty()) out << ",\"error\":" << json_string(error);
        out << "}";
    }
    out << "],\"count\":" << addrs.size() << ",\"changed_count\":" << changed << ",\"all\":false}";
    return out.str();
}

std::string core_idb_save(const std::string& args) {
    std::string path = json_string_arg(args, "path");
    bool ok = save_database(path.empty() ? nullptr : path.c_str(), 0);
    std::ostringstream out;
    out << "{\"ok\":" << (ok ? "true" : "false")
        << ",\"path\":" << (path.empty() ? "null" : json_string(path))
        << ",\"changed_count\":" << (ok ? 1 : 0);
    if (!ok) out << ",\"error\":\"save_database failed\"";
    out << "}";
    return out.str();
}

std::string core_py_eval(const std::string& args) {
    std::string code = json_string_arg(args, "code");
    if (code.empty()) {
        throw std::invalid_argument("code is required");
    }

    extlang_object_t python = find_extlang_by_name("Python");
    if (python == nullptr) {
        python = find_extlang_by_ext("py");
    }
    if (python == nullptr || python->eval_snippet == nullptr || python->eval_expr == nullptr) {
        throw std::runtime_error("IDAPython external language is not available");
    }

    std::ostringstream wrapper;
    wrapper
        << "def __dbgatlas_py_eval_run():\n"
        << "    import contextlib, io, json, traceback\n"
        << "    _json_dumps = json.dumps\n"
        << "    _format_exc = traceback.format_exc\n"
        << "    _stdout = io.StringIO()\n"
        << "    _stderr = io.StringIO()\n"
        << "    _error = None\n"
        << "    try:\n"
        << "        with contextlib.redirect_stdout(_stdout), contextlib.redirect_stderr(_stderr):\n"
        << "            exec(" << json_string(code) << ", globals(), globals())\n"
        << "    except BaseException:\n"
        << "        _error = _format_exc()\n"
        << "    return _json_dumps({\n"
        << "        'ok': _error is None,\n"
        << "        'stdout': _stdout.getvalue(),\n"
        << "        'stderr': _stderr.getvalue(),\n"
        << "        'error': _error,\n"
        << "    })\n"
        << "__dbgatlas_py_eval_result = __dbgatlas_py_eval_run()\n";

    qstring errbuf;
    if (!python->eval_snippet(wrapper.str().c_str(), &errbuf)) {
        throw std::runtime_error("IDAPython eval_snippet failed: " + qstring_to_string(errbuf));
    }

    idc_value_t result;
    qstring result_err;
    if (!python->eval_expr(&result, BADADDR, "__dbgatlas_py_eval_result", &result_err)) {
        throw std::runtime_error("IDAPython result retrieval failed: " + qstring_to_string(result_err));
    }
    if (result.vtype != VT_STR) {
        throw std::runtime_error("IDAPython result retrieval returned a non-string value");
    }
    return result.c_str();
}

struct BytePattern {
    std::vector<uint8_t> bytes;
    std::vector<bool> wildcard;
};

BytePattern parse_byte_pattern(const std::string& pattern) {
    BytePattern result;
    size_t pos = 0;
    while (pos < pattern.size()) {
        while (pos < pattern.size() && std::isspace(static_cast<unsigned char>(pattern[pos])) != 0) ++pos;
        if (pos >= pattern.size()) break;
        size_t end = pos;
        while (end < pattern.size() && std::isspace(static_cast<unsigned char>(pattern[end])) == 0) ++end;
        std::string token = pattern.substr(pos, end - pos);
        if (token == "?" || token == "??") {
            result.bytes.push_back(0);
            result.wildcard.push_back(true);
        } else {
            uint64_t value = 0;
            if (token.rfind("0x", 0) != 0 && token.rfind("0X", 0) != 0) {
                token = "0x" + token;
            }
            if (!parse_u64_text(token, &value) || value > 0xff) {
                throw std::invalid_argument("invalid byte pattern token");
            }
            result.bytes.push_back(static_cast<uint8_t>(value));
            result.wildcard.push_back(false);
        }
        pos = end;
    }
    if (result.bytes.empty()) {
        throw std::invalid_argument("pattern must not be empty");
    }
    return result;
}

bool byte_pattern_matches(ea_t ea, const BytePattern& pattern) {
    for (size_t i = 0; i < pattern.bytes.size(); ++i) {
        uint8_t byte = 0;
        if (get_bytes(&byte, 1, ea + static_cast<ea_t>(i)) != 1) {
            return false;
        }
        if (!pattern.wildcard[i] && byte != pattern.bytes[i]) {
            return false;
        }
    }
    return true;
}

std::string core_find_bytes(const std::string& args) {
    std::vector<std::string> patterns = json_list_arg(args, "patterns");
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t limit = std::min<uint64_t>(json_u64_arg(args, "limit", 100), 10000);
    uint64_t stop_after = offset + limit + 1;
    if (stop_after < offset) stop_after = std::numeric_limits<uint64_t>::max();
    std::vector<std::string> rows;
    uint64_t total = 0;
    bool truncated = false;
    for (const auto& pattern_text : patterns) {
        BytePattern pattern = parse_byte_pattern(pattern_text);
        for (segment_t* seg = get_first_seg(); seg != nullptr; seg = get_next_seg(seg->end_ea)) {
            if (seg->end_ea <= seg->start_ea || static_cast<uint64_t>(seg->end_ea - seg->start_ea) < pattern.bytes.size()) {
                continue;
            }
            for (ea_t ea = seg->start_ea; ea + static_cast<ea_t>(pattern.bytes.size()) <= seg->end_ea; ++ea) {
                if (!byte_pattern_matches(ea, pattern)) {
                    continue;
                }
                uint64_t index = total++;
                if (total >= stop_after) {
                    truncated = true;
                    break;
                }
                if (index < offset) {
                    continue;
                }
                if (rows.size() >= limit) {
                    truncated = true;
                    break;
                }
                std::ostringstream row;
                row << "{\"pattern\":" << json_string(pattern_text)
                    << ",\"ea\":" << json_u64(static_cast<uint64_t>(ea)) << "}";
                rows.push_back(row.str());
            }
            if (truncated) break;
        }
        if (truncated) break;
    }
    std::ostringstream out;
    out << "{\"offset\":" << json_u64(offset) << ",\"items\":[";
    for (size_t i = 0; i < rows.size(); ++i) {
        if (i != 0) out << ",";
        out << rows[i];
    }
    uint64_t next = offset + static_cast<uint64_t>(rows.size());
    out << "],\"count\":" << json_u64(static_cast<uint64_t>(rows.size()))
        << ",\"total\":" << json_u64(total)
        << ",\"next_offset\":";
    if (truncated || next < total) {
        out << json_u64(next);
    } else {
        out << "null";
    }
    out << "}";
    return out.str();
}

void maybe_add_search_row(std::vector<std::string>* rows, const std::string& scope, ea_t ea, const std::string& text, const std::string& needle) {
    if (needle.empty() || to_lower_copy(text).find(needle) == std::string::npos) {
        return;
    }
    std::ostringstream row;
    row << "{\"scope\":" << json_string(scope)
        << ",\"ea\":" << json_u64(static_cast<uint64_t>(ea))
        << ",\"text\":" << json_string(text) << "}";
    rows->push_back(row.str());
}

std::string core_search_text(IdaSessionHandleImpl* handle, const std::string& args) {
    std::string query = json_string_arg(args, "query");
    std::string scope = json_string_arg(args, "scope");
    if (scope.empty()) scope = "all";
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = json_u64_arg(args, "limit", json_u64_arg(args, "count", 100));
    std::string needle = to_lower_copy(query);
    std::vector<std::string> rows;

    if (scope == "all" || scope == "strings") {
        ensure_string_list(handle);
        size_t qty = get_strlist_qty();
        for (size_t i = 0; i < qty; ++i) {
            string_info_t item;
            if (!get_strlist_item(&item, i)) continue;
            std::string text;
            if (read_string_text(item.ea, item.length > 0 ? static_cast<size_t>(item.length) : size_t(-1), item.type, &text)) {
                maybe_add_search_row(&rows, "strings", item.ea, text, needle);
            }
        }
    }
    if (scope == "all" || scope == "names") {
        size_t qty = get_nlist_size();
        for (size_t i = 0; i < qty; ++i) {
            const char* name = get_nlist_name(i);
            maybe_add_search_row(&rows, "names", get_nlist_ea(i), name == nullptr ? std::string() : std::string(name), needle);
        }
    }
    if (scope == "all" || scope == "disasm" || scope == "comments") {
        for (size_t i = 0; i < get_func_qty(); ++i) {
            func_t* function = getn_func(i);
            if (function == nullptr) continue;
            for (ea_t ea = function->start_ea; ea < function->end_ea;) {
                if (scope == "all" || scope == "disasm") {
                    qstring line;
                    if (generate_disasm_line(&line, ea, GENDSM_REMOVE_TAGS)) {
                        maybe_add_search_row(&rows, "disasm", ea, qstring_to_string(line), needle);
                    }
                }
                if (scope == "all" || scope == "comments") {
                    qstring cmt;
                    if (get_cmt(&cmt, ea, false) > 0) {
                        maybe_add_search_row(&rows, "comments", ea, qstring_to_string(cmt), needle);
                    }
                    qstring rcmt;
                    if (get_cmt(&rcmt, ea, true) > 0) {
                        maybe_add_search_row(&rows, "comments", ea, qstring_to_string(rcmt), needle);
                    }
                }
                asize_t size = dbgatlas_get_item_size(ea);
                ea += size == 0 ? 1 : size;
            }
        }
    }
    return paged_result_json(rows, offset, count);
}

std::string core_xref_query(const std::string& args) {
    std::string target = json_string_arg(args, "target");
    std::string direction = json_string_arg(args, "direction");
    std::string xref_type = json_string_arg(args, "xref_type");
    if (direction.empty()) direction = "to";
    if (xref_type.empty()) xref_type = "all";
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = json_u64_arg(args, "limit", json_u64_arg(args, "count", 100));
    ea_t ea = resolve_query_addr_or_name(target);
    if (ea == BADADDR) {
        throw std::invalid_argument("target was not found");
    }
    std::vector<std::string> rows;
    auto add_row = [&](ea_t from, ea_t to, const char* kind) {
        func_t* function = get_func(direction == "to" ? from : to);
        std::ostringstream row;
        row << "{\"direction\":" << json_string(direction)
            << ",\"type\":" << json_string(kind)
            << ",\"from\":" << json_u64(static_cast<uint64_t>(from))
            << ",\"to\":" << json_u64(static_cast<uint64_t>(to))
            << ",\"function\":" << function_json(function) << "}";
        rows.push_back(row.str());
    };
    if (direction == "to") {
        if (xref_type == "all" || xref_type == "code") {
            for (ea_t from = get_first_cref_to(ea); from != BADADDR; from = get_next_cref_to(ea, from)) add_row(from, ea, "code");
        }
        if (xref_type == "all" || xref_type == "data") {
            for (ea_t from = get_first_dref_to(ea); from != BADADDR; from = get_next_dref_to(ea, from)) add_row(from, ea, "data");
        }
    } else if (direction == "from") {
        if (xref_type == "all" || xref_type == "code") {
            for (ea_t to = get_first_cref_from(ea); to != BADADDR; to = get_next_cref_from(ea, to)) add_row(ea, to, "code");
        }
        if (xref_type == "all" || xref_type == "data") {
            for (ea_t to = get_first_dref_from(ea); to != BADADDR; to = get_next_dref_from(ea, to)) add_row(ea, to, "data");
        }
    } else {
        throw std::invalid_argument("direction must be `to` or `from`");
    }
    return paged_result_json(rows, offset, count);
}

std::string core_func_query(const std::string& args) {
    std::string filter = to_lower_copy(json_string_arg(args, "filter"));
    std::string name_regex = json_string_arg(args, "name_regex");
    std::string sort_by = json_string_arg(args, "sort_by");
    bool descending = json_bool_arg(args, "descending", false);
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = json_u64_arg(args, "count", 50);
    uint64_t min_size = json_u64_arg(args, "min_size", 0);
    uint64_t max_size = json_u64_arg(args, "max_size", std::numeric_limits<uint64_t>::max());
    bool require_has_type = false;
    bool has_type_filter = !find_json_value(args, "has_type").empty();
    if (has_type_filter) require_has_type = json_bool_arg(args, "has_type", false);
    std::regex regex_filter;
    bool use_regex = false;
    if (!name_regex.empty()) {
        regex_filter = std::regex(name_regex, std::regex::icase);
        use_regex = true;
    }

    struct Row { ea_t ea; uint64_t size; std::string name; bool has_type; std::string json; };
    std::vector<Row> rows;
    for (size_t i = 0; i < get_func_qty(); ++i) {
        func_t* function = getn_func(i);
        if (function == nullptr) continue;
        qstring name_q;
        get_func_name(&name_q, function->start_ea);
        std::string name = qstring_to_string(name_q);
        uint64_t size = static_cast<uint64_t>(function->end_ea - function->start_ea);
        if (!filter.empty() && to_lower_copy(name).find(filter) == std::string::npos) continue;
        if (use_regex && !std::regex_search(name, regex_filter)) continue;
        if (size < min_size || size > max_size) continue;
        tinfo_t tif;
        bool has_type = get_tinfo(&tif, function->start_ea);
        if (has_type_filter && has_type != require_has_type) continue;
        std::ostringstream row;
        row << "{\"address\":" << json_u64(static_cast<uint64_t>(function->start_ea))
            << ",\"name\":" << json_string(name)
            << ",\"size\":" << json_u64(size)
            << ",\"has_type\":" << (has_type ? "true" : "false") << "}";
        rows.push_back(Row{function->start_ea, size, name, has_type, row.str()});
    }
    if (sort_by == "name") {
        std::sort(rows.begin(), rows.end(), [&](const Row& a, const Row& b) { return descending ? a.name > b.name : a.name < b.name; });
    } else if (sort_by == "size") {
        std::sort(rows.begin(), rows.end(), [&](const Row& a, const Row& b) { return descending ? a.size > b.size : a.size < b.size; });
    } else {
        std::sort(rows.begin(), rows.end(), [&](const Row& a, const Row& b) { return descending ? a.ea > b.ea : a.ea < b.ea; });
    }
    std::vector<std::string> json_rows;
    for (const auto& row : rows) json_rows.push_back(row.json);
    return paged_result_json(json_rows, offset, count);
}

std::string core_entity_query(IdaSessionHandleImpl* handle, const std::string& args) {
    std::string kind = json_string_arg(args, "kind");
    if (kind.empty()) kind = "functions";
    std::string filter = to_lower_copy(json_string_arg(args, "filter"));
    uint64_t offset = json_u64_arg(args, "offset", 0);
    uint64_t count = json_u64_arg(args, "count", 50);
    std::vector<std::string> rows;
    auto add_if_match = [&](const std::string& row) {
        if (filter.empty() || to_lower_copy(row).find(filter) != std::string::npos) {
            rows.push_back(row);
        }
    };
    if (kind == "functions") {
        for (size_t i = 0; i < get_func_qty(); ++i) add_if_match(function_json(getn_func(i)));
    } else if (kind == "globals" || kind == "names") {
        size_t qty = get_nlist_size();
        for (size_t i = 0; i < qty; ++i) {
            ea_t ea = get_nlist_ea(i);
            const char* name = get_nlist_name(i);
            if (kind == "globals" && get_func(ea) != nullptr) continue;
            std::ostringstream row;
            row << "{\"kind\":" << json_string(kind == "globals" ? "global" : "name")
                << ",\"address\":" << json_u64(static_cast<uint64_t>(ea))
                << ",\"name\":" << json_string(name == nullptr ? std::string() : std::string(name)) << "}";
            add_if_match(row.str());
        }
    } else if (kind == "imports") {
        return core_imports(args);
    } else if (kind == "strings") {
        ensure_string_list(handle);
        size_t qty = get_strlist_qty();
        for (size_t i = 0; i < qty; ++i) {
            string_info_t item;
            if (!get_strlist_item(&item, i)) continue;
            std::string text;
            if (read_string_text(item.ea, item.length > 0 ? static_cast<size_t>(item.length) : size_t(-1), item.type, &text)) {
                add_if_match(string_info_json(item, text));
            }
        }
    } else {
        throw std::invalid_argument("kind must be functions, globals, imports, strings, or names");
    }
    return paged_result_json(rows, offset, count);
}

std::string execute_core_function(IdaSessionHandleImpl* handle, const std::string& function, const std::string& args) {
    if (function == "lookup_funcs") return core_lookup_funcs(args);
    if (function == "int_convert") return core_int_convert(args);
    if (function == "list_funcs") return core_list_funcs(args);
    if (function == "list_globals") return core_list_globals(args);
    if (function == "imports") return core_imports(args);
    if (function == "list_strings") return core_list_strings(handle, args);
    if (function == "get_string") return core_get_string(handle, args);
    if (function == "get_bytes") return core_get_bytes(args);
    if (function == "get_int") return core_get_int(args);
    if (function == "decompile") return core_decompile(args);
    if (function == "disasm") return core_disasm(args);
    if (function == "xrefs_to") return core_xrefs_to(args);
    if (function == "xrefs_to_field") return core_xrefs_to_field(args);
    if (function == "callees") return core_callees(args);
    if (function == "rename") return core_rename(args);
    if (function == "set_comments") return core_set_comments(args);
    if (function == "set_type") return core_set_type(args);
    if (function == "declare_type") return core_declare_type(args);
    if (function == "force_recompile") return core_force_recompile(args);
    if (function == "idb_save") return core_idb_save(args);
    if (function == "py_eval") return core_py_eval(args);
    if (function == "find_bytes") return core_find_bytes(args);
    if (function == "search_text") return core_search_text(handle, args);
    if (function == "xref_query") return core_xref_query(args);
    if (function == "func_query") return core_func_query(args);
    if (function == "entity_query") return core_entity_query(handle, args);
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
        dbgatlas_ida_runtime_load(install_dir);
        int init_result = init_library(0, nullptr);
        if (init_result != 0) {
            return fail(DA_IDA_ERR_IDA, "init_library failed with result " + std::to_string(init_result));
        }
        dbgatlas_validate_ida_runtime_version();
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
        std::string result_json = execute_core_function(impl, function, arguments_json);
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
