# Native FFI

DbgAtlas 采用 Rust 主进程 + C++ native adapter DLL。每个 native adapter 维护自己的 C header 和对应 Rust `*-sys` crate，不通过中心化 protocol 层定义边界。

当前 `native/include/dbgatlas_native.h` 是 bootstrap ABI，只用于 MVP 0/0.5 的 hello/version 构建链验证。进入真实 DbgEng MVP 前，应拆出 adapter-specific header，例如 `dbgatlas_dbgeng.h`，并由对应 `dbgatlas-dbgeng-sys` crate 局部绑定。

## C ABI 规则

- C++ 使用 C++20。
- DLL 只导出 C ABI 函数。
- 不跨 ABI 暴露 C++ class、`std::string`、`std::vector`、exception、template、COM 对象。
- 对外结构使用固定宽度整数类型。
- 所有复杂函数返回 `int32_t` 状态码。
- native 内部异常必须被捕获并转换为状态码。
- 字符串边界使用 UTF-8。
- C++ 分配的 view 由 `da_release_view` 释放。

## View + Owner

MVP 0 的 `DA_TextView` 形态：

```c
typedef struct DA_TextView {
    uint32_t struct_size;
    uint32_t flags;
    const char* data;
    size_t len;
    void* owner;
} DA_TextView;
```

Rust 侧只读 `data/len`，使用后调用 `da_release_view(owner)`。后续数组、string table、stack/module list 沿用同一释放原则。

## 错误处理

- `DA_OK` 表示成功。
- 参数错误返回 `DA_ERR_INVALID_ARGUMENT`。
- 错误消息通过 `da_last_error` 获取。
- `da_last_error` 支持先传空 buffer 获取 required length。

## 线程模型

MVP 0 只有 hello/version，不涉及 DbgEng/COM 线程亲和性。进入 MVP 1 后，DbgEng session、callback、event polling、COM 初始化和线程归属必须封装在 C++ DLL 内部，Rust 只看到 session handle 或 safe wrapper。

DbgEng、ETW、DIA 后续必须各自维护 header、DLL adapter 和 `*-sys` crate。不要新增中心化 protocol crate，也不要把多个 native adapter 过早聚合到一个 DLL ABI。
