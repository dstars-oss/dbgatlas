#include "dbgatlas_ida_runtime.h"

#define NOMINMAX
#include <windows.h>

#include <cstdarg>
#include <cstring>
#include <cwchar>
#include <mutex>
#include <sstream>
#include <stdexcept>

namespace {

constexpr int HX_HEXRAYS_FAILURE_T_DESC = 440;
constexpr int HX_CFUNC_T_GET_PSEUDOCODE = 560;
constexpr int HX_CFUNC_T_CLEANUP = 564;
constexpr int HX_DECOMPILE = 566;
constexpr int HX_MARK_CFUNC_DIRTY = 569;
constexpr int HX_CLEAR_CACHED_CFUNCS = 570;
constexpr int HX_HEXRAYS_FREE = 130;
// IDA 9.3 SP1 cfunc_t::refcnt offset, verified against the SDK x64 layout.
constexpr size_t CFUNC_T_REFCNT_OFFSET = 104;

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

using init_library_fn = int(DA_IDAAPI*)(int, char*[]);
using open_database_fn = int(DA_IDAAPI*)(const char*, bool, const char*);
using close_database_fn = void(DA_IDAAPI*)(bool);
using auto_wait_fn = bool(DA_IDAAPI*)();
using save_database_fn = bool(DA_IDAAPI*)(const char*, uint32, const void*, const void*);
using qvector_reserve_fn = void*(DA_IDAAPI*)(void*, void*, size_t, size_t);
using qfree_fn = void(DA_IDAAPI*)(void*);
using get_func_fn = func_t*(DA_IDAAPI*)(ea_t);
using getn_func_fn = func_t*(DA_IDAAPI*)(size_t);
using get_func_qty_fn = size_t(DA_IDAAPI*)();
using get_func_name_fn = ssize_t(DA_IDAAPI*)(qstring*, ea_t);
using get_frame_size_fn = asize_t(DA_IDAAPI*)(const func_t*);
using get_tinfo_fn = bool(DA_IDAAPI*)(tinfo_t*, ea_t);
using clear_tinfo_t_fn = void(DA_IDAAPI*)(tinfo_t*);
using get_name_ea_fn = ea_t(DA_IDAAPI*)(ea_t, const char*);
using get_ea_name_fn = ssize_t(DA_IDAAPI*)(qstring*, ea_t, int, void*);
using get_nlist_size_fn = size_t(DA_IDAAPI*)();
using get_nlist_ea_fn = ea_t(DA_IDAAPI*)(size_t);
using get_nlist_name_fn = const char*(DA_IDAAPI*)(size_t);
using set_name_fn = bool(DA_IDAAPI*)(ea_t, const char*, int);
using build_strlist_fn = void(DA_IDAAPI*)();
using get_strlist_qty_fn = size_t(DA_IDAAPI*)();
using get_strlist_item_fn = bool(DA_IDAAPI*)(string_info_t*, size_t);
using get_strlit_contents_fn = ssize_t(DA_IDAAPI*)(qstring*, ea_t, size_t, int32, size_t*, int32);
using get_bytes_fn = ssize_t(DA_IDAAPI*)(void*, ssize_t, ea_t, int, void*);
using get_item_end_fn = ea_t(DA_IDAAPI*)(ea_t);
using get_str_type_fn = uint32(DA_IDAAPI*)(ea_t);
using generate_disasm_line_fn = bool(DA_IDAAPI*)(qstring*, ea_t, int);
using get_cmt_fn = ssize_t(DA_IDAAPI*)(qstring*, ea_t, bool);
using set_cmt_fn = bool(DA_IDAAPI*)(ea_t, const char*, bool);
using tag_remove_fn = ssize_t(DA_IDAAPI*)(qstring*, const char*, int);
using get_import_module_qty_fn = uint(DA_IDAAPI*)();
using get_import_module_name_fn = bool(DA_IDAAPI*)(qstring*, int);
using enum_import_names_fn = int(DA_IDAAPI*)(int, import_enum_cb_t, void*);
using get_first_seg_fn = segment_t*(DA_IDAAPI*)();
using get_next_seg_fn = segment_t*(DA_IDAAPI*)(ea_t);
using xref_first_fn = ea_t(DA_IDAAPI*)(ea_t);
using xref_next_fn = ea_t(DA_IDAAPI*)(ea_t, ea_t);
using get_idati_fn = void*(DA_IDAAPI*)();
using apply_cdecl_fn = bool(DA_IDAAPI*)(void*, ea_t, const char*, int);
using parse_decls_fn = int(DA_IDAAPI*)(void*, const char*, void*, int);
using find_extlang_fn = void*(DA_IDAAPI*)(const void*, int);
using free_idcv_fn = void(DA_IDAAPI*)(idc_value_t*);
using hexdsp_fn = void*(DA_HEXAPI*)(int, ...);
using get_hexdsp_fn = hexdsp_fn(DA_IDAAPI*)();

struct IdaApis {
    HMODULE ida = nullptr;
    HMODULE idalib = nullptr;
    init_library_fn init_library = nullptr;
    open_database_fn open_database = nullptr;
    close_database_fn close_database = nullptr;
    auto_wait_fn auto_wait = nullptr;
    save_database_fn save_database = nullptr;
    qvector_reserve_fn qvector_reserve = nullptr;
    qfree_fn qfree = nullptr;
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
    hexdsp_fn hexdsp = nullptr;
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
        out << "failed to load " << name << " from IDA install dir, GetLastError=" << GetLastError();
        throw std::runtime_error(out.str());
    }
    return module;
}

void ensure_loaded() {
    if (g_api.ida == nullptr || g_api.idalib == nullptr) {
        throw std::runtime_error("IDA runtime has not been loaded");
    }
}

void ensure_hexrays() {
    if (!init_hexrays_plugin()) {
        throw std::runtime_error("Hex-Rays decompiler is unavailable");
    }
}

hexdsp_fn hexrays_dispatcher() {
    ensure_loaded();
    hexdsp_fn dispatcher = g_api.get_hexdsp == nullptr ? nullptr : g_api.get_hexdsp();
    if (dispatcher == nullptr) {
        dispatcher = g_api.hexdsp;
    }
    if (dispatcher == nullptr) {
        throw std::runtime_error("Hex-Rays dispatcher is unavailable");
    }
    return dispatcher;
}

} // namespace

void dbgatlas_ida_runtime_load(const std::wstring& install_dir) {
    std::lock_guard<std::mutex> guard(g_runtime_mutex);
    std::wstring normalized_install_dir = normalize_install_dir(install_dir);
    if (g_api.ida != nullptr && g_api.idalib != nullptr) {
        if (_wcsicmp(g_loaded_install_dir.c_str(), normalized_install_dir.c_str()) != 0) {
            throw std::runtime_error("IDA runtime is already loaded from a different install_dir");
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
    api.qvector_reserve = bind_proc<qvector_reserve_fn>(api.ida, "ida.dll", "qvector_reserve");
    api.qfree = bind_proc<qfree_fn>(api.ida, "ida.dll", "qfree");
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

void* dbgatlas_ida_qvector_reserve(void* qvector, void* old, size_t cnt, size_t elsize) {
    ensure_loaded();
    return g_api.qvector_reserve(qvector, old, cnt, elsize);
}

void dbgatlas_ida_qfree(void* ptr) {
    if (ptr != nullptr && g_api.qfree != nullptr) {
        g_api.qfree(ptr);
    }
}

void qstring::assign(const char* text) {
    const size_t len = text == nullptr ? 0 : std::strlen(text);
    body.resize(len + 1);
    if (len > 0) {
        std::memcpy(body.array, text, len);
    }
    body.array[len] = '\0';
}

idc_value_t::idc_value_t() : vtype(VT_LONG), num(0) {}

idc_value_t::~idc_value_t() {
    clear();
}

const char* idc_value_t::c_str() const {
    return reinterpret_cast<const qstring*>(&num)->c_str();
}

void idc_value_t::clear() {
    if (g_api.free_idcv != nullptr) {
        g_api.free_idcv(this);
    }
    vtype = VT_LONG;
    num = 0;
}

tinfo_t::~tinfo_t() {
    clear();
}

void tinfo_t::clear() {
    if (g_api.clear_tinfo_t != nullptr) {
        g_api.clear_tinfo_t(this);
    }
    typid = 0;
}

int DA_IDAAPI init_library(int argc, char* argv[]) {
    ensure_loaded();
    return g_api.init_library(argc, argv);
}

int DA_IDAAPI open_database(const char* file, bool run_auto, const char* args) {
    ensure_loaded();
    return g_api.open_database(file, run_auto, args);
}

void DA_IDAAPI close_database(bool save) {
    ensure_loaded();
    g_api.close_database(save);
}

bool DA_IDAAPI auto_wait() {
    ensure_loaded();
    return g_api.auto_wait();
}

bool DA_IDAAPI save_database(const char* outfile, uint32 flags) {
    ensure_loaded();
    return g_api.save_database(outfile, flags, nullptr, nullptr);
}

func_t* DA_IDAAPI get_func(ea_t ea) {
    ensure_loaded();
    return g_api.get_func(ea);
}

func_t* DA_IDAAPI getn_func(size_t n) {
    ensure_loaded();
    return g_api.getn_func(n);
}

size_t DA_IDAAPI get_func_qty() {
    ensure_loaded();
    return g_api.get_func_qty();
}

ssize_t DA_IDAAPI get_func_name(qstring* out, ea_t ea) {
    ensure_loaded();
    return g_api.get_func_name(out, ea);
}

asize_t DA_IDAAPI get_frame_size(const func_t* pfn) {
    ensure_loaded();
    return g_api.get_frame_size(pfn);
}

bool DA_IDAAPI get_tinfo(tinfo_t* tif, ea_t ea) {
    ensure_loaded();
    return g_api.get_tinfo(tif, ea);
}

void DA_IDAAPI clear_tinfo_t(tinfo_t* tif) {
    ensure_loaded();
    g_api.clear_tinfo_t(tif);
}

ea_t DA_IDAAPI get_name_ea(ea_t from, const char* name) {
    ensure_loaded();
    return g_api.get_name_ea(from, name);
}

qstring get_name(ea_t ea) {
    qstring out;
    get_ea_name(&out, ea, 0);
    return out;
}

ssize_t DA_IDAAPI get_ea_name(qstring* out, ea_t ea, int flags) {
    ensure_loaded();
    return g_api.get_ea_name(out, ea, flags, nullptr);
}

size_t DA_IDAAPI get_nlist_size() {
    ensure_loaded();
    return g_api.get_nlist_size();
}

ea_t DA_IDAAPI get_nlist_ea(size_t n) {
    ensure_loaded();
    return g_api.get_nlist_ea(n);
}

const char* DA_IDAAPI get_nlist_name(size_t n) {
    ensure_loaded();
    return g_api.get_nlist_name(n);
}

bool DA_IDAAPI set_name(ea_t ea, const char* name, int flags) {
    ensure_loaded();
    return g_api.set_name(ea, name, flags);
}

void DA_IDAAPI build_strlist() {
    ensure_loaded();
    g_api.build_strlist();
}

size_t DA_IDAAPI get_strlist_qty() {
    ensure_loaded();
    return g_api.get_strlist_qty();
}

bool DA_IDAAPI get_strlist_item(string_info_t* out, size_t n) {
    ensure_loaded();
    return g_api.get_strlist_item(out, n);
}

ssize_t DA_IDAAPI get_strlit_contents(qstring* out, ea_t ea, size_t len, int32 type, size_t* maxcps, int32 flags) {
    ensure_loaded();
    return g_api.get_strlit_contents(out, ea, len, type, maxcps, flags);
}

ssize_t DA_IDAAPI get_bytes(void* buf, ssize_t size, ea_t ea) {
    ensure_loaded();
    return g_api.get_bytes(buf, size, ea, 0, nullptr);
}

ea_t DA_IDAAPI get_item_end(ea_t ea) {
    ensure_loaded();
    return g_api.get_item_end(ea);
}

asize_t get_item_size(ea_t ea) {
    ea_t end = get_item_end(ea);
    return end > ea ? end - ea : 0;
}

uint32 DA_IDAAPI get_str_type(ea_t ea) {
    ensure_loaded();
    return g_api.get_str_type(ea);
}

bool DA_IDAAPI generate_disasm_line(qstring* out, ea_t ea, int flags) {
    ensure_loaded();
    return g_api.generate_disasm_line(out, ea, flags);
}

ssize_t DA_IDAAPI get_cmt(qstring* out, ea_t ea, bool repeatable) {
    ensure_loaded();
    return g_api.get_cmt(out, ea, repeatable);
}

bool DA_IDAAPI set_cmt(ea_t ea, const char* cmt, bool repeatable) {
    ensure_loaded();
    return g_api.set_cmt(ea, cmt, repeatable);
}

ssize_t DA_IDAAPI tag_remove(qstring* out, const char* line, int init_level) {
    ensure_loaded();
    return g_api.tag_remove(out, line, init_level);
}

ssize_t tag_remove(qstring* out, const qstring& line, int init_level) {
    return tag_remove(out, line.c_str(), init_level);
}

uint DA_IDAAPI get_import_module_qty() {
    ensure_loaded();
    return g_api.get_import_module_qty();
}

bool DA_IDAAPI get_import_module_name(qstring* out, int mod_index) {
    ensure_loaded();
    return g_api.get_import_module_name(out, mod_index);
}

int DA_IDAAPI enum_import_names(int mod_index, import_enum_cb_t cb, void* param) {
    ensure_loaded();
    return g_api.enum_import_names(mod_index, cb, param);
}

segment_t* DA_IDAAPI get_first_seg() {
    ensure_loaded();
    return g_api.get_first_seg();
}

segment_t* DA_IDAAPI get_next_seg(ea_t ea) {
    ensure_loaded();
    return g_api.get_next_seg(ea);
}

ea_t DA_IDAAPI get_first_cref_to(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_cref_to(ea);
}

ea_t DA_IDAAPI get_next_cref_to(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_cref_to(ea, current);
}

ea_t DA_IDAAPI get_first_dref_to(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_dref_to(ea);
}

ea_t DA_IDAAPI get_next_dref_to(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_dref_to(ea, current);
}

ea_t DA_IDAAPI get_first_cref_from(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_cref_from(ea);
}

ea_t DA_IDAAPI get_next_cref_from(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_cref_from(ea, current);
}

ea_t DA_IDAAPI get_first_dref_from(ea_t ea) {
    ensure_loaded();
    return g_api.get_first_dref_from(ea);
}

ea_t DA_IDAAPI get_next_dref_from(ea_t ea, ea_t current) {
    ensure_loaded();
    return g_api.get_next_dref_from(ea, current);
}

void* DA_IDAAPI get_idati() {
    ensure_loaded();
    return g_api.get_idati();
}

bool DA_IDAAPI apply_cdecl(void* til, ea_t ea, const char* decl, int flags) {
    ensure_loaded();
    return g_api.apply_cdecl(til, ea, decl, flags);
}

int DA_IDAAPI parse_decls(void* til, const char* input, void* printer, int flags) {
    ensure_loaded();
    return g_api.parse_decls(til, input, printer, flags);
}

extlang_t* DA_IDAAPI find_extlang(const void* name_or_ext, int kind) {
    ensure_loaded();
    return static_cast<extlang_t*>(g_api.find_extlang(name_or_ext, kind));
}

extlang_object_t find_extlang_by_name(const char* name) {
    return extlang_object_t(find_extlang(name, FIND_EXTLANG_BY_NAME));
}

extlang_object_t find_extlang_by_ext(const char* ext) {
    return extlang_object_t(find_extlang(ext, FIND_EXTLANG_BY_EXT));
}

void DA_IDAAPI free_idcv(idc_value_t* value) {
    ensure_loaded();
    g_api.free_idcv(value);
}

bool init_hexrays_plugin(int flags) {
    (void)flags;
    ensure_loaded();
    if (g_api.hexdsp != nullptr) {
        return true;
    }
    hexdsp_fn dispatcher = g_api.get_hexdsp == nullptr ? nullptr : g_api.get_hexdsp();
    if (dispatcher != nullptr) {
        g_api.hexdsp = dispatcher;
        return true;
    }
    return false;
}

void term_hexrays_plugin() {
    g_api.hexdsp = nullptr;
}

qstring hexrays_failure_t::desc() const {
    ensure_hexrays();
    qstring result;
    hexrays_dispatcher()(HX_HEXRAYS_FAILURE_T_DESC, &result, this);
    return result;
}

const strvec_t& cfunc_t::get_pseudocode() const {
    ensure_hexrays();
    return *static_cast<const strvec_t*>(hexrays_dispatcher()(HX_CFUNC_T_GET_PSEUDOCODE, this));
}

void cfunc_t::add_ref() noexcept {
    auto* refcnt = reinterpret_cast<int*>(reinterpret_cast<unsigned char*>(this) + CFUNC_T_REFCNT_OFFSET);
    ++*refcnt;
}

void cfunc_t::release() noexcept {
    try {
        auto* refcnt = reinterpret_cast<int*>(reinterpret_cast<unsigned char*>(this) + CFUNC_T_REFCNT_OFFSET);
        if (--*refcnt == 0) {
            hexdsp_fn dispatcher = hexrays_dispatcher();
            dispatcher(HX_CFUNC_T_CLEANUP, this);
            dispatcher(HX_HEXRAYS_FREE, this);
        }
    } catch (...) {
    }
}

cfuncptr_t decompile_func(func_t* function, hexrays_failure_t* failure, int flags) {
    ensure_hexrays();
    mba_ranges_t ranges(function);
    return cfuncptr_t(static_cast<cfunc_t*>(hexrays_dispatcher()(HX_DECOMPILE, &ranges, failure, flags)));
}

bool mark_cfunc_dirty(ea_t ea, bool close_views) {
    ensure_hexrays();
    return reinterpret_cast<size_t>(hexrays_dispatcher()(HX_MARK_CFUNC_DIRTY, ea, close_views)) != 0;
}

void clear_cached_cfuncs() {
    ensure_hexrays();
    hexrays_dispatcher()(HX_CLEAR_CACHED_CFUNCS);
}
