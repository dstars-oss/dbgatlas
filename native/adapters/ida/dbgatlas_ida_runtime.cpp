#define NOMINMAX
#include "dbgatlas_ida_runtime.h"

#include <windows.h>

#include <cwchar>
#include <mutex>
#include <new>
#include <sstream>
#include <stdexcept>

namespace {

std::wstring join_path(const std::wstring& base, const wchar_t* child) {
    std::wstring result = base;
    if (!result.empty() && result.back() != L'\\' && result.back() != L'/') {
        result.push_back(L'\\');
    }
    result.append(child);
    return result;
}

std::wstring normalize_install_dir(const std::wstring& path) {
    DWORD required = GetFullPathNameW(path.c_str(), 0, nullptr, nullptr);
    if (required == 0) {
        return path;
    }
    std::wstring normalized(required, L'\0');
    DWORD written = GetFullPathNameW(path.c_str(), required, normalized.data(), nullptr);
    if (written == 0 || written >= required) {
        return path;
    }
    normalized.resize(written);
    while (!normalized.empty() && (normalized.back() == L'\\' || normalized.back() == L'/')) {
        normalized.pop_back();
    }
    return normalized;
}

std::string narrow_symbol_error(const char* dll, const char* name) {
    std::ostringstream out;
    out << dll << " is missing required export `" << name << "`";
    return out.str();
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

using init_library_fn = int(idaapi*)(int, char*[]);
using open_database_fn = int(idaapi*)(const char*, bool, const char*);
using close_database_fn = void(idaapi*)(bool);
using auto_wait_fn = bool(idaapi*)();
using save_database_fn = bool(idaapi*)(const char*, uint32, const snapshot_t*, const snapshot_t*);
using get_library_version_fn = bool(idaapi*)(int&, int&, int&);
using qalloc_fn = void*(idaapi*)(size_t);
using qrealloc_fn = void*(idaapi*)(void*, size_t);
using qfree_fn = void(idaapi*)(void*);
using qalloc_or_throw_fn = void*(idaapi*)(size_t);
using qrealloc_or_throw_fn = void*(idaapi*)(void*, size_t);
using qvector_reserve_fn = void*(idaapi*)(void*, void*, size_t, size_t);
using get_func_fn = func_t*(idaapi*)(ea_t);
using getn_func_fn = func_t*(idaapi*)(size_t);
using get_func_qty_fn = size_t(idaapi*)();
using get_func_name_fn = ssize_t(idaapi*)(qstring*, ea_t);
using get_frame_size_fn = asize_t(idaapi*)(const func_t*);
using get_tinfo_fn = bool(idaapi*)(tinfo_t*, ea_t);
using clear_tinfo_t_fn = void(idaapi*)(tinfo_t*);
using get_name_ea_fn = ea_t(idaapi*)(ea_t, const char*);
using get_ea_name_fn = ssize_t(idaapi*)(qstring*, ea_t, int, getname_info_t*);
using get_nlist_size_fn = size_t(idaapi*)();
using get_nlist_ea_fn = ea_t(idaapi*)(size_t);
using get_nlist_name_fn = const char*(idaapi*)(size_t);
using set_name_fn = bool(idaapi*)(ea_t, const char*, int);
using build_strlist_fn = void(idaapi*)();
using get_strlist_qty_fn = size_t(idaapi*)();
using get_strlist_item_fn = bool(idaapi*)(string_info_t*, size_t);
using get_strlit_contents_fn = ssize_t(idaapi*)(qstring*, ea_t, size_t, int32, size_t*, int);
using get_bytes_fn = ssize_t(idaapi*)(void*, ssize_t, ea_t, int, void*);
using get_item_end_fn = ea_t(idaapi*)(ea_t);
using get_str_type_fn = uint32(idaapi*)(ea_t);
using generate_disasm_line_fn = bool(idaapi*)(qstring*, ea_t, int);
using get_cmt_fn = ssize_t(idaapi*)(qstring*, ea_t, bool);
using set_cmt_fn = bool(idaapi*)(ea_t, const char*, bool);
using tag_remove_fn = ssize_t(idaapi*)(qstring*, const char*, int);
using get_import_module_qty_fn = uint(idaapi*)();
using get_import_module_name_fn = bool(idaapi*)(qstring*, int);
using enum_import_names_fn = int(idaapi*)(int, import_enum_cb_t*, void*);
using get_first_seg_fn = segment_t*(idaapi*)();
using get_next_seg_fn = segment_t*(idaapi*)(ea_t);
using xref_first_fn = ea_t(idaapi*)(ea_t);
using xref_next_fn = ea_t(idaapi*)(ea_t, ea_t);
using get_idati_fn = til_t*(idaapi*)();
using apply_cdecl_fn = bool(idaapi*)(til_t*, ea_t, const char*, int);
using parse_decls_fn = int(idaapi*)(til_t*, const char*, printer_t*, int);
using find_extlang_fn = void*(idaapi*)(const void*, find_extlang_kind_t);
using free_idcv_fn = void(idaapi*)(idc_value_t*);
using get_hexdsp_fn = hexdsp_t*(idaapi*)();

struct IdaApis {
    HMODULE ida = nullptr;
    HMODULE idalib = nullptr;
    init_library_fn init_library = nullptr;
    open_database_fn open_database = nullptr;
    close_database_fn close_database = nullptr;
    auto_wait_fn auto_wait = nullptr;
    save_database_fn save_database = nullptr;
    get_library_version_fn get_library_version = nullptr;
    qalloc_fn qalloc = nullptr;
    qrealloc_fn qrealloc = nullptr;
    qfree_fn qfree = nullptr;
    qalloc_or_throw_fn qalloc_or_throw = nullptr;
    qrealloc_or_throw_fn qrealloc_or_throw = nullptr;
    qvector_reserve_fn qvector_reserve = nullptr;
    get_func_fn get_func = nullptr;
    getn_func_fn getn_func = nullptr;
    get_func_qty_fn get_func_qty = nullptr;
    get_func_name_fn get_func_name = nullptr;
    get_frame_size_fn get_frame_size = nullptr;
    get_tinfo_fn get_tinfo = nullptr;
    clear_tinfo_t_fn clear_tinfo_t = nullptr;
    get_name_ea_fn get_name_ea = nullptr;
    get_ea_name_fn get_ea_name = nullptr;
    get_nlist_size_fn get_nlist_size = nullptr;
    get_nlist_ea_fn get_nlist_ea = nullptr;
    get_nlist_name_fn get_nlist_name = nullptr;
    set_name_fn set_name = nullptr;
    build_strlist_fn build_strlist = nullptr;
    get_strlist_qty_fn get_strlist_qty = nullptr;
    get_strlist_item_fn get_strlist_item = nullptr;
    get_strlit_contents_fn get_strlit_contents = nullptr;
    get_bytes_fn get_bytes = nullptr;
    get_item_end_fn get_item_end = nullptr;
    get_str_type_fn get_str_type = nullptr;
    generate_disasm_line_fn generate_disasm_line = nullptr;
    get_cmt_fn get_cmt = nullptr;
    set_cmt_fn set_cmt = nullptr;
    tag_remove_fn tag_remove = nullptr;
    get_import_module_qty_fn get_import_module_qty = nullptr;
    get_import_module_name_fn get_import_module_name = nullptr;
    enum_import_names_fn enum_import_names = nullptr;
    get_first_seg_fn get_first_seg = nullptr;
    get_next_seg_fn get_next_seg = nullptr;
    xref_first_fn get_first_cref_to = nullptr;
    xref_next_fn get_next_cref_to = nullptr;
    xref_first_fn get_first_dref_to = nullptr;
    xref_next_fn get_next_dref_to = nullptr;
    xref_first_fn get_first_cref_from = nullptr;
    xref_next_fn get_next_cref_from = nullptr;
    xref_first_fn get_first_dref_from = nullptr;
    xref_next_fn get_next_dref_from = nullptr;
    get_idati_fn get_idati = nullptr;
    apply_cdecl_fn apply_cdecl = nullptr;
    parse_decls_fn parse_decls = nullptr;
    find_extlang_fn find_extlang = nullptr;
    free_idcv_fn free_idcv = nullptr;
    get_hexdsp_fn get_hexdsp = nullptr;
    hexdsp_t* hexdsp = nullptr;
};

std::mutex g_runtime_mutex;
IdaApis g_api;
std::wstring g_loaded_install_dir;

template <typename Fn>
Fn bind_proc(HMODULE module, const char* dll, const char* name) {
    FARPROC proc = GetProcAddress(module, name);
    if (proc == nullptr) {
        throw std::runtime_error(narrow_symbol_error(dll, name));
    }
    return reinterpret_cast<Fn>(proc);
}

HMODULE load_dll(const std::wstring& path, const char* name) {
    HMODULE module = LoadLibraryExW(path.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (module == nullptr) {
        std::ostringstream out;
        out << "failed to load " << name << " at " << wide_to_utf8(path)
            << ", GetLastError=" << GetLastError();
        throw std::runtime_error(out.str());
    }
    return module;
}

void ensure_loaded() {
    if (g_api.ida == nullptr || g_api.idalib == nullptr) {
        throw std::runtime_error("IDA runtime has not been loaded");
    }
}

void* bad_alloc_if_null(void* ptr) {
    if (ptr == nullptr) {
        throw std::bad_alloc();
    }
    return ptr;
}

void validate_library_version(const IdaApis& api) {
    int major = 0;
    int minor = 0;
    int build = 0;
    if (!api.get_library_version(major, minor, build)) {
        throw std::runtime_error("get_library_version failed");
    }
    if (major != 9 || minor != 3) {
        std::ostringstream out;
        out << "IDA runtime version " << major << "." << minor << " build " << build
            << " does not match vendored IDA SDK headers 9.3";
        throw std::runtime_error(out.str());
    }
}

} // namespace

void dbgatlas_ida_runtime_load(const std::wstring& install_dir) {
    std::lock_guard<std::mutex> guard(g_runtime_mutex);
    std::wstring normalized_install_dir = normalize_install_dir(install_dir);
    // Build against vendored IDA SDK headers, but bind ida.dll/idalib.dll from
    // the user's installation at runtime. IDA runtime state is process-global,
    // so one process can safely bind only one install_dir.
    if (g_api.ida != nullptr && g_api.idalib != nullptr) {
        if (_wcsicmp(g_loaded_install_dir.c_str(), normalized_install_dir.c_str()) != 0) {
            throw std::runtime_error(
                "IDA runtime is already loaded from " + wide_to_utf8(g_loaded_install_dir) +
                "; requested " + wide_to_utf8(normalized_install_dir));
        }
        return;
    }

    IdaApis api;
    api.ida = load_dll(join_path(normalized_install_dir, L"ida.dll"), "ida.dll");
    api.idalib = load_dll(join_path(normalized_install_dir, L"idalib.dll"), "idalib.dll");

    api.init_library = bind_proc<init_library_fn>(api.idalib, "idalib.dll", "init_library");
    api.open_database = bind_proc<open_database_fn>(api.idalib, "idalib.dll", "open_database");
    api.close_database = bind_proc<close_database_fn>(api.idalib, "idalib.dll", "close_database");

    api.auto_wait = bind_proc<auto_wait_fn>(api.ida, "ida.dll", "auto_wait");
    api.save_database = bind_proc<save_database_fn>(api.ida, "ida.dll", "save_database");
    api.get_library_version = bind_proc<get_library_version_fn>(api.idalib, "idalib.dll", "get_library_version");
    api.qalloc = bind_proc<qalloc_fn>(api.ida, "ida.dll", "qalloc");
    api.qrealloc = bind_proc<qrealloc_fn>(api.ida, "ida.dll", "qrealloc");
    api.qfree = bind_proc<qfree_fn>(api.ida, "ida.dll", "qfree");
    api.qalloc_or_throw = bind_proc<qalloc_or_throw_fn>(api.ida, "ida.dll", "qalloc_or_throw");
    api.qrealloc_or_throw = bind_proc<qrealloc_or_throw_fn>(api.ida, "ida.dll", "qrealloc_or_throw");
    api.qvector_reserve = bind_proc<qvector_reserve_fn>(api.ida, "ida.dll", "qvector_reserve");
    api.get_func = bind_proc<get_func_fn>(api.ida, "ida.dll", "get_func");
    api.getn_func = bind_proc<getn_func_fn>(api.ida, "ida.dll", "getn_func");
    api.get_func_qty = bind_proc<get_func_qty_fn>(api.ida, "ida.dll", "get_func_qty");
    api.get_func_name = bind_proc<get_func_name_fn>(api.ida, "ida.dll", "get_func_name");
    api.get_frame_size = bind_proc<get_frame_size_fn>(api.ida, "ida.dll", "get_frame_size");
    api.get_tinfo = bind_proc<get_tinfo_fn>(api.ida, "ida.dll", "get_tinfo");
    api.clear_tinfo_t = bind_proc<clear_tinfo_t_fn>(api.ida, "ida.dll", "clear_tinfo_t");
    api.get_name_ea = bind_proc<get_name_ea_fn>(api.ida, "ida.dll", "get_name_ea");
    api.get_ea_name = bind_proc<get_ea_name_fn>(api.ida, "ida.dll", "get_ea_name");
    api.get_nlist_size = bind_proc<get_nlist_size_fn>(api.ida, "ida.dll", "get_nlist_size");
    api.get_nlist_ea = bind_proc<get_nlist_ea_fn>(api.ida, "ida.dll", "get_nlist_ea");
    api.get_nlist_name = bind_proc<get_nlist_name_fn>(api.ida, "ida.dll", "get_nlist_name");
    api.set_name = bind_proc<set_name_fn>(api.ida, "ida.dll", "set_name");
    api.build_strlist = bind_proc<build_strlist_fn>(api.ida, "ida.dll", "build_strlist");
    api.get_strlist_qty = bind_proc<get_strlist_qty_fn>(api.ida, "ida.dll", "get_strlist_qty");
    api.get_strlist_item = bind_proc<get_strlist_item_fn>(api.ida, "ida.dll", "get_strlist_item");
    api.get_strlit_contents = bind_proc<get_strlit_contents_fn>(api.ida, "ida.dll", "get_strlit_contents");
    api.get_bytes = bind_proc<get_bytes_fn>(api.ida, "ida.dll", "get_bytes");
    api.get_item_end = bind_proc<get_item_end_fn>(api.ida, "ida.dll", "get_item_end");
    api.get_str_type = bind_proc<get_str_type_fn>(api.ida, "ida.dll", "get_str_type");
    api.generate_disasm_line = bind_proc<generate_disasm_line_fn>(api.ida, "ida.dll", "generate_disasm_line");
    api.get_cmt = bind_proc<get_cmt_fn>(api.ida, "ida.dll", "get_cmt");
    api.set_cmt = bind_proc<set_cmt_fn>(api.ida, "ida.dll", "set_cmt");
    api.tag_remove = bind_proc<tag_remove_fn>(api.ida, "ida.dll", "tag_remove");
    api.get_import_module_qty = bind_proc<get_import_module_qty_fn>(api.ida, "ida.dll", "get_import_module_qty");
    api.get_import_module_name = bind_proc<get_import_module_name_fn>(api.ida, "ida.dll", "get_import_module_name");
    api.enum_import_names = bind_proc<enum_import_names_fn>(api.ida, "ida.dll", "enum_import_names");
    api.get_first_seg = bind_proc<get_first_seg_fn>(api.ida, "ida.dll", "get_first_seg");
    api.get_next_seg = bind_proc<get_next_seg_fn>(api.ida, "ida.dll", "get_next_seg");
    api.get_first_cref_to = bind_proc<xref_first_fn>(api.ida, "ida.dll", "get_first_cref_to");
    api.get_next_cref_to = bind_proc<xref_next_fn>(api.ida, "ida.dll", "get_next_cref_to");
    api.get_first_dref_to = bind_proc<xref_first_fn>(api.ida, "ida.dll", "get_first_dref_to");
    api.get_next_dref_to = bind_proc<xref_next_fn>(api.ida, "ida.dll", "get_next_dref_to");
    api.get_first_cref_from = bind_proc<xref_first_fn>(api.ida, "ida.dll", "get_first_cref_from");
    api.get_next_cref_from = bind_proc<xref_next_fn>(api.ida, "ida.dll", "get_next_cref_from");
    api.get_first_dref_from = bind_proc<xref_first_fn>(api.ida, "ida.dll", "get_first_dref_from");
    api.get_next_dref_from = bind_proc<xref_next_fn>(api.ida, "ida.dll", "get_next_dref_from");
    api.get_idati = bind_proc<get_idati_fn>(api.ida, "ida.dll", "get_idati");
    api.apply_cdecl = bind_proc<apply_cdecl_fn>(api.ida, "ida.dll", "apply_cdecl");
    api.parse_decls = bind_proc<parse_decls_fn>(api.ida, "ida.dll", "parse_decls");
    api.find_extlang = bind_proc<find_extlang_fn>(api.ida, "ida.dll", "find_extlang");
    api.free_idcv = bind_proc<free_idcv_fn>(api.ida, "ida.dll", "free_idcv");
    api.get_hexdsp = bind_proc<get_hexdsp_fn>(api.ida, "ida.dll", "get_hexdsp");

    g_api = api;
    g_loaded_install_dir = normalized_install_dir;
}

void dbgatlas_validate_ida_runtime_version() {
    ensure_loaded();
    validate_library_version(g_api);
}

hexdsp_t* dbgatlas_ida_hexrays_dispatcher() {
    ensure_loaded();
    if (g_api.hexdsp == nullptr && g_api.get_hexdsp != nullptr) {
        g_api.hexdsp = g_api.get_hexdsp();
    }
    if (g_api.hexdsp == nullptr) {
        throw std::runtime_error("Hex-Rays dispatcher is unavailable");
    }
    return g_api.hexdsp;
}

bool dbgatlas_init_hexrays_plugin(int flags) {
    (void)flags;
    ensure_loaded();
    return dbgatlas_ida_hexrays_dispatcher() != nullptr;
}

void dbgatlas_term_hexrays_plugin() {
    g_api.hexdsp = nullptr;
}

asize_t dbgatlas_get_item_size(ea_t ea) {
    ea_t end = get_item_end(ea);
    return end > ea ? end - ea : 0;
}

idaman THREAD_SAFE void* ida_export qalloc(size_t size) {
    ensure_loaded();
    return g_api.qalloc(size);
}

idaman THREAD_SAFE void* ida_export qrealloc(void* alloc, size_t newsize) {
    ensure_loaded();
    return g_api.qrealloc(alloc, newsize);
}

idaman THREAD_SAFE void ida_export qfree(void* alloc) {
    if (alloc != nullptr && g_api.qfree != nullptr) {
        g_api.qfree(alloc);
    }
}

idaman THREAD_SAFE void* ida_export qalloc_or_throw(size_t size) {
    ensure_loaded();
    return bad_alloc_if_null(g_api.qalloc_or_throw(size));
}

idaman THREAD_SAFE void* ida_export qrealloc_or_throw(void* ptr, size_t size) {
    ensure_loaded();
    return bad_alloc_if_null(g_api.qrealloc_or_throw(ptr, size));
}

idaman THREAD_SAFE void* ida_export qvector_reserve(void* vec, void* old, size_t cnt, size_t elsize) {
    ensure_loaded();
    return bad_alloc_if_null(g_api.qvector_reserve(vec, old, cnt, elsize));
}

idaman int ida_export init_library(int argc, char* argv[]) {
    ensure_loaded();
    return g_api.init_library(argc, argv);
}

idaman int ida_export open_database(const char* file, bool run_auto, const char* args) {
    ensure_loaded();
    return g_api.open_database(file, run_auto, args);
}

idaman void ida_export close_database(bool save) {
    ensure_loaded();
    g_api.close_database(save);
}

idaman bool ida_export auto_wait() {
    ensure_loaded();
    return g_api.auto_wait();
}

idaman bool ida_export save_database(
    const char* outfile,
    uint32 flags,
    const snapshot_t* root,
    const snapshot_t* attr) {
    ensure_loaded();
    return g_api.save_database(outfile, flags, root, attr);
}

idaman func_t* ida_export get_func(ea_t ea) {
    ensure_loaded();
    return g_api.get_func(ea);
}

idaman func_t* ida_export getn_func(size_t n) {
    ensure_loaded();
    return g_api.getn_func(n);
}

idaman size_t ida_export get_func_qty() {
    ensure_loaded();
    return g_api.get_func_qty();
}

idaman ssize_t ida_export get_func_name(qstring* out, ea_t ea) {
    ensure_loaded();
    return g_api.get_func_name(out, ea);
}

idaman asize_t ida_export get_frame_size(const func_t* pfn) {
    ensure_loaded();
    return g_api.get_frame_size(pfn);
}

idaman bool ida_export get_tinfo(tinfo_t* tif, ea_t ea) {
    ensure_loaded();
    return g_api.get_tinfo(tif, ea);
}

idaman void ida_export clear_tinfo_t(tinfo_t* tif) {
    ensure_loaded();
    g_api.clear_tinfo_t(tif);
}

idaman ea_t ida_export get_name_ea(ea_t from, const char* name) {
    ensure_loaded();
    return g_api.get_name_ea(from, name);
}

idaman ssize_t ida_export get_ea_name(qstring* out, ea_t ea, int flags, getname_info_t* gtni) {
    ensure_loaded();
    return g_api.get_ea_name(out, ea, flags, gtni);
}

idaman size_t ida_export get_nlist_size() {
    ensure_loaded();
    return g_api.get_nlist_size();
}

idaman ea_t ida_export get_nlist_ea(size_t n) {
    ensure_loaded();
    return g_api.get_nlist_ea(n);
}

idaman const char* ida_export get_nlist_name(size_t n) {
    ensure_loaded();
    return g_api.get_nlist_name(n);
}

idaman bool ida_export set_name(ea_t ea, const char* name, int flags) {
    ensure_loaded();
    return g_api.set_name(ea, name, flags);
}

idaman void ida_export build_strlist() {
    ensure_loaded();
    g_api.build_strlist();
}

idaman size_t ida_export get_strlist_qty() {
    ensure_loaded();
    return g_api.get_strlist_qty();
}

idaman bool ida_export get_strlist_item(string_info_t* out, size_t n) {
    ensure_loaded();
    return g_api.get_strlist_item(out, n);
}

idaman ssize_t ida_export get_strlit_contents(
    qstring* out,
    ea_t ea,
    size_t len,
    int32 type,
    size_t* maxcps,
    int flags) {
    ensure_loaded();
    return g_api.get_strlit_contents(out, ea, len, type, maxcps, flags);
}

idaman ssize_t ida_export get_bytes(void* buf, ssize_t size, ea_t ea, int gmb_flags, void* mask) {
    ensure_loaded();
    return g_api.get_bytes(buf, size, ea, gmb_flags, mask);
}

idaman ea_t ida_export get_item_end(ea_t ea) {
    ensure_loaded();
    return g_api.get_item_end(ea);
}

idaman uint32 ida_export get_str_type(ea_t ea) {
    ensure_loaded();
    return g_api.get_str_type(ea);
}

idaman bool ida_export generate_disasm_line(qstring* out, ea_t ea, int flags) {
    ensure_loaded();
    return g_api.generate_disasm_line(out, ea, flags);
}

idaman ssize_t ida_export get_cmt(qstring* out, ea_t ea, bool repeatable) {
    ensure_loaded();
    return g_api.get_cmt(out, ea, repeatable);
}

idaman bool ida_export set_cmt(ea_t ea, const char* cmt, bool repeatable) {
    ensure_loaded();
    return g_api.set_cmt(ea, cmt, repeatable);
}

idaman THREAD_SAFE ssize_t ida_export tag_remove(qstring* out, const char* line, int init_level) {
    ensure_loaded();
    return g_api.tag_remove(out, line, init_level);
}

idaman uint ida_export get_import_module_qty() {
    ensure_loaded();
    return g_api.get_import_module_qty();
}

idaman bool ida_export get_import_module_name(qstring* out, int mod_index) {
    ensure_loaded();
    return g_api.get_import_module_name(out, mod_index);
}

idaman int ida_export enum_import_names(int mod_index, import_enum_cb_t* cb, void* param) {
    ensure_loaded();
    return g_api.enum_import_names(mod_index, cb, param);
}

idaman segment_t* ida_export get_first_seg() {
    ensure_loaded();
    return g_api.get_first_seg();
}

idaman segment_t* ida_export get_next_seg(ea_t ea) {
    ensure_loaded();
    return g_api.get_next_seg(ea);
}

idaman ea_t ida_export get_first_cref_to(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_cref_to(ea);
}

idaman ea_t ida_export get_next_cref_to(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_cref_to(ea, current);
}

idaman ea_t ida_export get_first_dref_to(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_dref_to(ea);
}

idaman ea_t ida_export get_next_dref_to(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_dref_to(ea, current);
}

idaman ea_t ida_export get_first_cref_from(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_cref_from(ea);
}

idaman ea_t ida_export get_next_cref_from(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_cref_from(ea, current);
}

idaman ea_t ida_export get_first_dref_from(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_dref_from(ea);
}

idaman ea_t ida_export get_next_dref_from(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_dref_from(ea, current);
}

idaman til_t* ida_export get_idati() {
    ensure_loaded();
    return g_api.get_idati();
}

idaman bool ida_export apply_cdecl(til_t* til, ea_t ea, const char* decl, int flags) {
    ensure_loaded();
    return g_api.apply_cdecl(til, ea, decl, flags);
}

idaman int ida_export parse_decls(til_t* til, const char* input, printer_t* printer, int flags) {
    ensure_loaded();
    return g_api.parse_decls(til, input, printer, flags);
}

idaman void* ida_export find_extlang(const void* name_or_ext, find_extlang_kind_t kind) {
    ensure_loaded();
    return g_api.find_extlang(name_or_ext, kind);
}

idaman THREAD_SAFE void ida_export free_idcv(idc_value_t* value) {
    ensure_loaded();
    g_api.free_idcv(value);
}
