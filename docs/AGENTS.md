# AGENTS.md

Architecture documents must follow the current confirmed DbgAtlas direction.

- The source repository produces the tool, not real investigation data.
- Real analysis data belongs in an explicit analysis workspace.
- Do not reintroduce hidden `.dbgatlas` workspace state.
- Keep MVP 0 and MVP 0.5 documentation focused on workspace, adapter boundaries, runtime separation, worker/session boundaries, native ABI, and CLI validation.
- Avoid specifying full GUI, full DbgEng wrapper, or complex serialization frameworks before the corresponding capability exists.
