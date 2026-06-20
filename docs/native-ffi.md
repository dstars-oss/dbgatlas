# Native FFI

DbgAtlas 采用 Rust 主进程 + C++ native adapter DLL。每个 native adapter 维护自己的 C header 和对应 Rust `*-sys` crate，不通过中心化 protocol 层定义边界。

MVP 1 起，DbgEng adapter 使用 `native/include/dbgatlas_dbgeng.h` 作为 adapter-specific C ABI，并由 `dbgatlas-dbgeng-sys` crate 局部绑定。早期 `native/include/dbgatlas_native.h` 只保留为 MVP 0/0.5 bootstrap 历史文件，不再作为 DbgEng DLL 的当前导出边界。

## C ABI 规则

- C++ 使用 C++20。
- DLL 只导出 C ABI 函数。
- 不跨 ABI 暴露 C++ class、`std::string`、`std::vector`、exception、template、COM 对象。
- 对外结构使用固定宽度整数类型。
- 所有复杂函数返回 `int32_t` 状态码。
- native 内部异常必须被捕获并转换为状态码。
- 字符串边界使用 UTF-8；Rust safe wrapper 必须拒绝无法表示为 UTF-8 的路径。
- C++ 分配的 view 由 adapter-specific release 函数释放，例如 `da_dbgeng_release_view`。

## View + Owner

DbgEng MVP 1 的 `DA_DbgEngTextView` 形态：

```c
typedef struct DA_DbgEngTextView {
    uint32_t struct_size;
    uint32_t flags;
    const char* data;
    size_t len;
    void* owner;
} DA_DbgEngTextView;
```

Rust 侧只读 `data/len`，使用后调用 `da_dbgeng_release_view(owner)`。后续数组、string table、stack/module list 沿用同一释放原则。

## DbgEng Session Handle

DbgEng adapter 对 Rust 只暴露 opaque handle：

```c
typedef struct DA_DbgEngSessionHandle DA_DbgEngSessionHandle;
```

最小 session ABI 包括：

- `da_dbgeng_session_open_dump(path_utf8, out_handle)`
- `da_dbgeng_session_attach_process(pid, out_handle)`
- `da_dbgeng_session_execute(handle, command_utf8, out_text)`
- `da_dbgeng_session_add_symbols(handle, symbol_path_utf8, reload, out_text)`
- `da_dbgeng_session_read_virtual(handle, address, length, out_bytes)`
- `da_dbgeng_session_close(handle)`

当前实现使用 DbgEng `DebugCreate` 建立 session。dump 通过 `OpenDumpFile` 打开，attach 使用非侵入 attach，不在 close 时终止目标进程。raw command 通过 `IDebugControl::Execute` 执行，并用 output callbacks 捕获输出。`add_symbols` 追加当前 session 的 symbol path，可选执行 `.reload`。`read_virtual` 用 `IDebugDataSpaces::ReadVirtual` 返回二进制 view。

## IDA native adapter ABI

IDA adapter 使用 `native/include/dbgatlas_ida.h` 定义独立 C ABI。Rust safe wrapper 根据请求中的 IDA install dir 配置 Windows DLL search path，再运行时加载 `dbgatlas_ida.dll`；adapter 维护 IDA 9.3 SP1 最小自有 ABI 声明，并在 session open 时按需加载用户本机安装目录里的 `ida.dll` / `idalib.dll` / Hex-Rays dispatcher。构建不依赖 IDA SDK headers 或 import libs，导出边界只暴露 opaque handle、固定宽度整数和 text view。

当前最小 ABI：

- `da_ida_abi_version(out)`
- `da_ida_session_open(install_dir_utf8, database_path_utf8, out_handle)`
- `da_ida_lookup_function(handle, runtime_address, runtime_module_base, ida_image_base, out)`
- `da_ida_core_function(handle, function_utf8, arguments_json_utf8, out)`
- `da_ida_session_close(handle)`
- `da_ida_last_error(buffer, buffer_len, required_len)`
- `da_ida_release_view(owner)`

第一条 MVP 链路只做定位和记录，不写 IDA comment，不保存 IDB。

`DA_DbgEngTextView` 也用于 read memory 的 byte view；Rust safe wrapper 按 `data/len` 复制为 `Vec<u8>` 后调用 `da_dbgeng_release_view(owner)`。

## 错误处理

- adapter-specific OK 状态表示成功，例如 `DA_DBGENG_OK`。
- 参数错误返回 adapter-specific invalid argument 状态，例如 `DA_DBGENG_ERR_INVALID_ARGUMENT`。
- 错误消息通过 adapter-specific `last_error` 获取，例如 `da_dbgeng_last_error`。
- `last_error` 支持先传空 buffer 获取 required length。

## 线程模型

DbgEng session、callback、event polling、COM 初始化和线程归属必须封装在 C++ DLL 内部，Rust 只看到 session handle 或 safe wrapper。

MVP 1 的真实 DbgEng session 由 per-session worker 持有。worker 主循环串行处理同一 session 的 start/eval/add_symbols/read_memory/close 请求，避免跨线程直接操作同一 DbgEng client。

DbgEng、ETW、DIA 后续必须各自维护 header、DLL adapter 和 `*-sys` crate。不要新增中心化 protocol crate，也不要把多个 native adapter 过早聚合到一个 DLL ABI。
