#pragma once

// Minimal DbgAtlas-owned IDA 9.3 SP1 ABI subset used by dbgatlas_ida.cpp.

#include <cstddef>
#include <cstdint>
#include <new>
#include <string>
#include <type_traits>
#include <utility>

#ifdef _WIN32
#define DA_IDAAPI __stdcall
#else
#define DA_IDAAPI
#endif
#define DA_HEXAPI
#define idaapi DA_IDAAPI

using uchar = unsigned char;
using uint = unsigned int;
using uint32 = std::uint32_t;
using int32 = std::int32_t;
using uint64 = std::uint64_t;
using int64 = std::int64_t;
using ea_t = std::uint64_t;
using asize_t = std::uint64_t;
using uval_t = std::uint64_t;
using sval_t = std::int64_t;
using ssize_t = std::intptr_t;
using color_t = std::uint32_t;
using bgcolor_t = std::uint32_t;
using merror_t = int;

constexpr ea_t BADADDR = ~ea_t{0};
constexpr int32 STRCONV_ESCAPE = 1;
constexpr int32 STRTYPE_C = 0;
constexpr int32 GENDSM_REMOVE_TAGS = 4;
constexpr int32 SN_NOCHECK = 1;
constexpr int32 SN_FORCE = 0x800;
constexpr int32 TINFO_DEFINITE = 1;
constexpr int32 HTI_NWR = 0x100;
constexpr int32 HTI_DCL = 0x400;
constexpr int32 DECOMP_WARNINGS = 0x8;
constexpr int32 VT_LONG = 2;
constexpr int32 VT_STR = 7;
constexpr merror_t MERR_LICENSE = -23;
constexpr bgcolor_t DEFCOLOR = 0xFFFFFFFFu;

enum extlang_find_t : int {
    FIND_EXTLANG_BY_EXT = 0,
    FIND_EXTLANG_BY_NAME = 1,
    FIND_EXTLANG_BY_IDX = 2,
};

void dbgatlas_ida_runtime_load(const std::wstring& install_dir);

void* dbgatlas_ida_qvector_reserve(void* qvector, void* old, size_t cnt, size_t elsize);
void dbgatlas_ida_qfree(void* ptr);

template <typename T>
struct qvector {
    T* array = nullptr;
    size_t n = 0;
    size_t alloc = 0;

    qvector() = default;
    qvector(const qvector& rhs) {
        reserve(rhs.n);
        for (size_t i = 0; i < rhs.n; ++i) {
            new (array + i) T(rhs.array[i]);
        }
        n = rhs.n;
    }
    qvector& operator=(const qvector& rhs) {
        if (this == &rhs) {
            return *this;
        }
        clear();
        reserve(rhs.n);
        for (size_t i = 0; i < rhs.n; ++i) {
            new (array + i) T(rhs.array[i]);
        }
        n = rhs.n;
        return *this;
    }
    ~qvector() {
        clear();
    }

    T* begin() { return array; }
    const T* begin() const { return array; }
    T* end() { return array + n; }
    const T* end() const { return array + n; }
    size_t size() const { return n; }
    bool empty() const { return n == 0; }
    T& operator[](size_t index) { return array[index]; }
    const T& operator[](size_t index) const { return array[index]; }

    void reserve(size_t cnt) {
        if (cnt <= alloc) {
            return;
        }
        if constexpr (std::is_trivially_copyable_v<T>) {
            array = static_cast<T*>(dbgatlas_ida_qvector_reserve(this, array, cnt, sizeof(T)));
        } else {
            size_t old_alloc = alloc;
            T* new_array = static_cast<T*>(dbgatlas_ida_qvector_reserve(this, nullptr, cnt, sizeof(T)));
            size_t new_alloc = alloc;
            alloc = old_alloc;
            for (size_t i = 0; i < n; ++i) {
                new (new_array + i) T(std::move(array[i]));
                array[i].~T();
            }
            dbgatlas_ida_qfree(array);
            array = new_array;
            alloc = new_alloc;
        }
    }

    void resize(size_t cnt) {
        reserve(cnt);
        while (n < cnt) {
            new (array + n) T();
            ++n;
        }
        while (n > cnt) {
            --n;
            array[n].~T();
        }
    }

    void push_back(const T& value) {
        reserve(n + 1);
        new (array + n) T(value);
        ++n;
    }

    void clear() {
        while (n > 0) {
            --n;
            array[n].~T();
        }
        if (array != nullptr) {
            dbgatlas_ida_qfree(array);
        }
        array = nullptr;
        alloc = 0;
    }
};

struct qstring {
    qvector<char> body;

    qstring() = default;
    qstring(const char* text) { assign(text); }
    qstring(const qstring&) = default;
    qstring& operator=(const qstring&) = default;

    const char* c_str() const {
        return body.array == nullptr ? "" : body.array;
    }
    bool empty() const {
        return body.n <= 1;
    }
    size_t length() const {
        return body.n == 0 ? 0 : body.n - 1;
    }
    void clear() {
        body.clear();
    }
    void assign(const char* text);
};

struct range_t {
    ea_t start_ea = 0;
    ea_t end_ea = 0;
};

struct func_t : range_t {
    std::uint64_t flags = 0;
};

struct segment_t : range_t {
    uval_t name = 0;
    uval_t sclass = 0;
    uval_t orgbase = 0;
};

struct string_info_t {
    ea_t ea = BADADDR;
    int length = 0;
    int type = 0;
};

struct tinfo_t {
    std::uint64_t typid = 0;

    tinfo_t() = default;
    ~tinfo_t();
    void clear();
};

struct idc_value_t {
    char vtype = 0;
    union {
        sval_t num;
        void* pvoid;
        uchar reserve[sizeof(qstring)];
    };

    idc_value_t();
    ~idc_value_t();
    const char* c_str() const;
    void clear();
};

using compiler_info_t = void;
using syntax_highlighter_t = void;
struct extlang_t;
using compile_expr_t = bool (DA_IDAAPI*)(const char*, ea_t, const char*, qstring*);
using compile_file_t = bool (DA_IDAAPI*)(const char*, const char*, qstring*);
using call_func_t = bool (DA_IDAAPI*)(idc_value_t*, const char*, const idc_value_t[], size_t, qstring*);
using eval_expr_t = bool (DA_IDAAPI*)(idc_value_t*, ea_t, const char*, qstring*);
using eval_snippet_t = bool (DA_IDAAPI*)(const char*, qstring*);
using create_object_t = bool (DA_IDAAPI*)(idc_value_t*, const char*, const idc_value_t[], size_t, qstring*);
using get_attr_t = bool (DA_IDAAPI*)(idc_value_t*, const idc_value_t*, const char*);
using set_attr_t = bool (DA_IDAAPI*)(idc_value_t*, const char*, const idc_value_t&);
using call_method_t = bool (DA_IDAAPI*)(idc_value_t*, const idc_value_t*, const char*, const idc_value_t[], size_t, qstring*);
using load_procmod_t = bool (DA_IDAAPI*)(idc_value_t*, const char*, qstring*);
using unload_procmod_t = bool (DA_IDAAPI*)(const char*, qstring*);

struct extlang_t {
    size_t size = 0;
    uint32 flags = 0;
    int32 refcnt = 0;
    const char* name = nullptr;
    const char* fileext = nullptr;
    syntax_highlighter_t* highlighter = nullptr;
    compile_expr_t compile_expr = nullptr;
    compile_file_t compile_file = nullptr;
    call_func_t call_func = nullptr;
    eval_expr_t eval_expr = nullptr;
    eval_snippet_t eval_snippet = nullptr;
    create_object_t create_object = nullptr;
    get_attr_t get_attr = nullptr;
    set_attr_t set_attr = nullptr;
    call_method_t call_method = nullptr;
    load_procmod_t load_procmod = nullptr;
    unload_procmod_t unload_procmod = nullptr;

    void release() {}
};

class extlang_object_t {
    extlang_t* ptr = nullptr;

    void addref() const {
        if (ptr != nullptr) {
            ++ptr->refcnt;
        }
    }

public:
    explicit extlang_object_t(extlang_t* value = nullptr) : ptr(value) {}
    extlang_object_t(const extlang_object_t& rhs) : ptr(rhs.ptr) { addref(); }
    extlang_object_t(extlang_object_t&& rhs) noexcept : ptr(std::exchange(rhs.ptr, nullptr)) {}
    ~extlang_object_t() { reset(); }

    extlang_object_t& operator=(const extlang_object_t& rhs) {
        if (this != &rhs) {
            reset();
            ptr = rhs.ptr;
            addref();
        }
        return *this;
    }

    extlang_object_t& operator=(extlang_object_t&& rhs) noexcept {
        if (this != &rhs) {
            reset();
            ptr = std::exchange(rhs.ptr, nullptr);
        }
        return *this;
    }

    void reset() {
        if (ptr != nullptr && --ptr->refcnt == 0) {
            ptr->release();
        }
        ptr = nullptr;
    }

    extlang_t* get() const { return ptr; }
    extlang_t* operator->() const { return ptr; }
    explicit operator bool() const { return ptr != nullptr; }
    friend bool operator==(const extlang_object_t& value, std::nullptr_t) { return value.ptr == nullptr; }
    friend bool operator!=(const extlang_object_t& value, std::nullptr_t) { return value.ptr != nullptr; }
};

struct simpleline_t {
    qstring line;
    color_t color = 1;
    bgcolor_t bgcolor = DEFCOLOR;
};
using strvec_t = qvector<simpleline_t>;
using rangevec_t = qvector<range_t>;

struct hexrays_failure_t {
    merror_t code = 0;
    ea_t errea = BADADDR;
    qstring str;

    qstring desc() const;
};

struct cfunc_t {
    const strvec_t& get_pseudocode() const;
    void add_ref() noexcept;
    void release() noexcept;
};

class cfuncptr_t {
    cfunc_t* ptr = nullptr;

public:
    explicit cfuncptr_t(cfunc_t* value = nullptr) : ptr(value) {}
    cfuncptr_t(const cfuncptr_t& rhs) : ptr(rhs.ptr) {
        if (ptr != nullptr) {
            ptr->add_ref();
        }
    }
    cfuncptr_t(cfuncptr_t&& rhs) noexcept : ptr(std::exchange(rhs.ptr, nullptr)) {}
    ~cfuncptr_t() { reset(); }

    cfuncptr_t& operator=(const cfuncptr_t& rhs) {
        if (this != &rhs) {
            reset();
            ptr = rhs.ptr;
            if (ptr != nullptr) {
                ptr->add_ref();
            }
        }
        return *this;
    }

    cfuncptr_t& operator=(cfuncptr_t&& rhs) noexcept {
        if (this != &rhs) {
            reset();
            ptr = std::exchange(rhs.ptr, nullptr);
        }
        return *this;
    }

    void reset() noexcept {
        if (ptr != nullptr) {
            ptr->release();
            ptr = nullptr;
        }
    }

    cfunc_t* get() const { return ptr; }
    cfunc_t* operator->() const { return ptr; }
    explicit operator bool() const { return ptr != nullptr; }
    friend bool operator==(const cfuncptr_t& value, std::nullptr_t) { return value.ptr == nullptr; }
    friend bool operator!=(const cfuncptr_t& value, std::nullptr_t) { return value.ptr != nullptr; }
};

struct mba_ranges_t {
    func_t* pfn = nullptr;
    rangevec_t ranges;

    explicit mba_ranges_t(func_t* value) : pfn(value) {}
};

static_assert(sizeof(ea_t) == 8, "DbgAtlas IDA adapter only supports 64-bit ea_t");
static_assert(sizeof(qvector<char>) == 24, "qvector ABI layout must match IDA x64");
static_assert(sizeof(qstring) == 24, "qstring ABI layout must match IDA x64");
static_assert(offsetof(range_t, start_ea) == 0, "range_t::start_ea offset must match IDA");
static_assert(offsetof(range_t, end_ea) == 8, "range_t::end_ea offset must match IDA");
static_assert(offsetof(func_t, start_ea) == 0, "func_t range prefix must match IDA");
static_assert(offsetof(segment_t, start_ea) == 0, "segment_t range prefix must match IDA");
static_assert(offsetof(string_info_t, ea) == 0, "string_info_t::ea offset must match IDA");
static_assert(offsetof(string_info_t, length) == 8, "string_info_t::length offset must match IDA");
static_assert(offsetof(string_info_t, type) == 12, "string_info_t::type offset must match IDA");
static_assert(offsetof(idc_value_t, vtype) == 0, "idc_value_t::vtype offset must match IDA");
static_assert(offsetof(idc_value_t, num) == 8, "idc_value_t value offset must match IDA");
static_assert(sizeof(idc_value_t) == 32, "idc_value_t ABI layout must match IDA x64");
static_assert(sizeof(extlang_t) == 128, "extlang_t ABI layout must match IDA x64");
static_assert(sizeof(extlang_object_t) == sizeof(void*), "extlang_object_t must stay pointer-sized");
static_assert(sizeof(cfuncptr_t) == sizeof(void*), "cfuncptr_t must stay pointer-sized");
static_assert(offsetof(extlang_t, eval_expr) == 64, "extlang_t::eval_expr offset must match IDA x64");
static_assert(offsetof(extlang_t, eval_snippet) == 72, "extlang_t::eval_snippet offset must match IDA x64");
static_assert(offsetof(hexrays_failure_t, code) == 0, "hexrays_failure_t::code offset must match IDA");
static_assert(offsetof(hexrays_failure_t, errea) == 8, "hexrays_failure_t::errea offset must match IDA");
static_assert(offsetof(hexrays_failure_t, str) == 16, "hexrays_failure_t::str offset must match IDA");
static_assert(offsetof(mba_ranges_t, pfn) == 0, "mba_ranges_t::pfn offset must match IDA");
static_assert(offsetof(mba_ranges_t, ranges) == 8, "mba_ranges_t::ranges offset must match IDA");

int DA_IDAAPI init_library(int argc, char* argv[]);
int DA_IDAAPI open_database(const char* file, bool run_auto, const char* args);
void DA_IDAAPI close_database(bool save);
bool DA_IDAAPI auto_wait();
bool DA_IDAAPI save_database(const char* outfile = nullptr, uint32 flags = 0);

func_t* DA_IDAAPI get_func(ea_t ea);
func_t* DA_IDAAPI getn_func(size_t n);
size_t DA_IDAAPI get_func_qty();
ssize_t DA_IDAAPI get_func_name(qstring* out, ea_t ea);
asize_t DA_IDAAPI get_frame_size(const func_t* pfn);
bool DA_IDAAPI get_tinfo(tinfo_t* tif, ea_t ea);
void DA_IDAAPI clear_tinfo_t(tinfo_t* tif);

ea_t DA_IDAAPI get_name_ea(ea_t from, const char* name);
qstring get_name(ea_t ea);
ssize_t DA_IDAAPI get_ea_name(qstring* out, ea_t ea, int flags = 0);
size_t DA_IDAAPI get_nlist_size();
ea_t DA_IDAAPI get_nlist_ea(size_t n);
const char* DA_IDAAPI get_nlist_name(size_t n);
bool DA_IDAAPI set_name(ea_t ea, const char* name, int flags);

void DA_IDAAPI build_strlist();
size_t DA_IDAAPI get_strlist_qty();
bool DA_IDAAPI get_strlist_item(string_info_t* out, size_t n);
ssize_t DA_IDAAPI get_strlit_contents(qstring* out, ea_t ea, size_t len, int32 type, size_t* maxcps, int32 flags);

ssize_t DA_IDAAPI get_bytes(void* buf, ssize_t size, ea_t ea);
ea_t DA_IDAAPI get_item_end(ea_t ea);
asize_t get_item_size(ea_t ea);
uint32 DA_IDAAPI get_str_type(ea_t ea);
bool DA_IDAAPI generate_disasm_line(qstring* out, ea_t ea, int flags);
ssize_t DA_IDAAPI get_cmt(qstring* out, ea_t ea, bool repeatable);
bool DA_IDAAPI set_cmt(ea_t ea, const char* cmt, bool repeatable);
ssize_t DA_IDAAPI tag_remove(qstring* out, const char* line, int init_level = 0);
ssize_t tag_remove(qstring* out, const qstring& line, int init_level = 0);

uint DA_IDAAPI get_import_module_qty();
bool DA_IDAAPI get_import_module_name(qstring* out, int mod_index);
using import_enum_cb_t = int(DA_IDAAPI*)(ea_t ea, const char* name, uval_t ordinal, void* param);
int DA_IDAAPI enum_import_names(int mod_index, import_enum_cb_t cb, void* param);

segment_t* DA_IDAAPI get_first_seg();
segment_t* DA_IDAAPI get_next_seg(ea_t ea);

ea_t DA_IDAAPI get_first_cref_to(ea_t ea);
ea_t DA_IDAAPI get_next_cref_to(ea_t ea, ea_t current);
ea_t DA_IDAAPI get_first_dref_to(ea_t ea);
ea_t DA_IDAAPI get_next_dref_to(ea_t ea, ea_t current);
ea_t DA_IDAAPI get_first_cref_from(ea_t ea);
ea_t DA_IDAAPI get_next_cref_from(ea_t ea, ea_t current);
ea_t DA_IDAAPI get_first_dref_from(ea_t ea);
ea_t DA_IDAAPI get_next_dref_from(ea_t ea, ea_t current);

void* DA_IDAAPI get_idati();
bool DA_IDAAPI apply_cdecl(void* til, ea_t ea, const char* decl, int flags);
int DA_IDAAPI parse_decls(void* til, const char* input, void* printer, int flags);

extlang_t* DA_IDAAPI find_extlang(const void* name_or_ext, int kind);
extlang_object_t find_extlang_by_name(const char* name);
extlang_object_t find_extlang_by_ext(const char* ext);
void DA_IDAAPI free_idcv(idc_value_t* value);

bool init_hexrays_plugin(int flags = 0);
void term_hexrays_plugin();
cfuncptr_t decompile_func(func_t* function, hexrays_failure_t* failure, int flags);
bool mark_cfunc_dirty(ea_t ea, bool close_views);
void clear_cached_cfuncs();
