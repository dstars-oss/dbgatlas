# IDA Core Functions

MVP 4 exposes IDA Core Functions through service RPC and HTTP MCP tools. Except for
`reverse.session.open` and `reverse.session.close`, every Core Function call requires an
active debug `session_id` and IDA `reverse_session_id`.

## RPC methods

- `reverse.lookup_funcs`: `{ session_id, reverse_session_id, queries, runtime_module_base?, ida_image_base? }`
- `reverse.int_convert`: `{ session_id, reverse_session_id, inputs }`
- `reverse.list_funcs`: `{ session_id, reverse_session_id, offset?, count?, filter? }`
- `reverse.list_globals`: `{ session_id, reverse_session_id, offset?, count?, filter? }`
- `reverse.imports`: `{ session_id, reverse_session_id, offset?, count?, filter? }`
- `reverse.decompile`: `{ session_id, reverse_session_id, addr }`
- `reverse.disasm`: `{ session_id, reverse_session_id, addr }`
- `reverse.xrefs_to`: `{ session_id, reverse_session_id, addrs }`
- `reverse.xrefs_to_field`: `{ session_id, reverse_session_id, queries }`
- `reverse.callees`: `{ session_id, reverse_session_id, addrs }`

List inputs accept either a JSON array or a comma-separated string. Integer inputs accept
JSON numbers, decimal strings, `0x` hex strings, and `0b` binary strings. `int_convert`
also accepts `bytes:` / `bytes_le:` little-endian byte lists and `ascii:` strings.

## Result shape

Successful calls return:

```json
{
  "session_id": { "id": "session-..." },
  "reverse_session_id": { "id": "session-..." },
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
artifacts/reverse_sessions/<debug_session_id>/core/<operation_id>.jsonl
```

The workspace registers this output as a `reverse.core` artifact and appends an operation
record with capability `reverse.<function>`, such as `reverse.list_funcs` or
`reverse.decompile`. Adapter failures are recorded as `reverse.adapter_error` artifacts.
