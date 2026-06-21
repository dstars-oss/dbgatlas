# IDA Core Functions

DbgAtlas exposes IDA Core Functions through service RPC and HTTP MCP tools. `reverse.session.open`
creates a top-level reverse `session_id` backed by its own worker process. Every Core
Function call requires that reverse `session_id`.

## RPC methods

Canonical reverse method names use verb-first names: `list_*` for paginated
enumeration, `query_*` for structured filtering/search, and `lookup_*` for
resolving user-provided addresses or names to concrete IDA entities.

- `reverse.session.open`: `{ project_root, database_path, ida_install_dir? }`
- `reverse.session.close`: `{ session_id }`
- `reverse.lookup_funcs`: `{ session_id, queries, runtime_module_base?, ida_image_base? }`
- `reverse.int_convert`: `{ session_id, inputs }`
- `reverse.list_funcs`: `{ session_id, offset?, count?, filter? }`
- `reverse.list_globals`: `{ session_id, offset?, count?, filter? }`
- `reverse.list_imports`: `{ session_id, offset?, count?, filter? }`
- `reverse.list_strings`: `{ session_id, offset?, count?, filter? }`
- `reverse.get_string`: `{ session_id, addr, length?, type? }`
- `reverse.get_bytes`: `{ session_id, addr, length }`
- `reverse.get_int`: `{ session_id, addr, size?, endian? }`
- `reverse.decompile`: `{ session_id, addr }`
- `reverse.disasm`: `{ session_id, addr }`
- `reverse.xrefs_to`: `{ session_id, addrs }`
- `reverse.xrefs_to_field`: `{ session_id, queries }`
- `reverse.callees`: `{ session_id, addrs }`
- `reverse.rename`: `{ session_id, items }`
- `reverse.set_comments`: `{ session_id, items }`
- `reverse.set_type`: `{ session_id, items }`
- `reverse.declare_type`: `{ session_id, decls }`
- `reverse.force_recompile`: `{ session_id, addrs? }`
- `reverse.idb_save`: `{ session_id, path? }`
- `reverse.py_eval`: `{ session_id, code }`
- `reverse.find_bytes`: `{ session_id, patterns, offset?, limit? }`
- `reverse.search_text`: `{ session_id, query, scope?, offset?, limit? }`
- `reverse.query_xrefs`: `{ session_id, target, direction?, xref_type?, offset?, limit? }`
- `reverse.query_funcs`: `{ session_id, filter?, name_regex?, min_size?, max_size?, has_type?, sort_by?, descending?, offset?, count? }`
- `reverse.query_entities`: `{ session_id, kind, filter?, fields?, offset?, count? }`

List inputs accept either a JSON array or a comma-separated string. Integer inputs accept
JSON numbers, decimal strings, `0x` hex strings, and `0b` binary strings. `int_convert`
also accepts `bytes:` / `bytes_le:` little-endian byte lists and `ascii:` strings.

`reverse.list_strings`, `reverse.get_string`, `reverse.get_bytes`, and
`reverse.get_int` are read-only IDB context helpers. They read bytes and string
metadata from the IDA database, not live debugger process memory. Use
`debug.read_memory` for target virtual memory. `reverse.list_strings` uses
case-insensitive substring filtering; regex string search is intentionally deferred.

`reverse.rename`, `reverse.set_comments`, `reverse.set_type`,
`reverse.declare_type`, `reverse.force_recompile`, and `reverse.idb_save` modify
the open IDA database by default. The first write-capable batch only supports
function/global/address-level edits. Local variable rename, stack frame edits,
append-comment mode, operand/struct-field typing, and regex text search are
intentionally deferred.

`reverse.session.open` itself does not modify the IDB. Session metadata keeps
the legacy `writes_idb: false` field for compatibility and also records
`open_operation_writes_idb: false` plus `session_write_capable: true` to make it
explicit that later calls in the same reverse session may write to the database.

`reverse.py_eval` is a prototype high-privilege escape hatch that executes
Python code in the open IDA context through IDAPython's external language
interface. It captures `stdout`, `stderr`, and traceback text in the tool result,
and is disabled by default at the service layer. Hosts must explicitly enable the
IDA `py_eval` capability before this RPC is accepted or listed through MCP. In
installed service mode this is controlled by `[tools.ida] allow_py_eval = true`
in `runtime.toml`; in development service mode it is controlled by
`dbgatlas service run --allow-ida-py-eval`.

`reverse.search_text` performs case-insensitive substring search over `strings`,
`names`, `disasm`, `comments`, or `all`. `reverse.find_bytes` supports byte
patterns such as `48 8B ?? ??` and compact even-length hex such as
`488b9090` or `0x488b9090`. Search-style tools return `items`, `offset`,
`count`, `total`, and `next_offset`. `reverse.find_bytes` stops scanning after it
has enough matches to answer the requested page plus `next_offset`; when
`next_offset` is non-null, `total` is the observed lower bound rather than an
exact database-wide total.

## Result shape

Successful calls return:

```json
{
  "session_id": { "id": "session-..." },
  "operation_id": { "id": "op-..." },
  "operation_status": "success",
  "artifact_refs": [{ "id": "artifact-..." }],
  "function": "list_funcs",
  "result": {},
  "warnings": [],
  "operation": {
    "status": "success",
    "artifact_refs": [{ "id": "artifact-..." }],
    "raw_output_ref": null
  }
}
```

The same methods are exposed as MCP tools with identical names and argument shapes.

`reverse.decompile` uses the IDA native adapter with Hex-Rays initialized through
IDALib. Successful results contain Hex-Rays C pseudocode with `language: "c"`.
If Hex-Rays is unavailable, a license is unavailable, or decompilation fails, the
operation fails and records a `reverse.adapter_error` artifact. Assembly text is only
available through an explicit `reverse.disasm` call.

## Artifact and operation records

Each Core Function call writes one JSONL record under:

```text
artifacts/reverse_sessions/<session_id>/core/<operation_id>.jsonl
```

The workspace registers this output as a `reverse.core` artifact and appends an operation
record with capability `reverse.<function>`, such as `reverse.list_funcs` or
`reverse.decompile`. Adapter failures are recorded as `reverse.adapter_error` artifacts.
