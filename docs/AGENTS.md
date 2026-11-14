# AGENTS.md

Architecture documents must follow the current confirmed DbgAtlas direction.

- The source repository produces the tool, not real investigation data.
- Real analysis data belongs in an explicit analysis workspace.
- Do not reintroduce hidden `.dbgatlas` workspace state.
- Keep architecture documentation focused on environment/target/session semantics, process ownership, backend and transport boundaries, workspace, runtime, native ABI, and service entry points.
- Avoid specifying full GUI, full DbgEng wrapper, or complex serialization frameworks before the corresponding capability exists.

- `architecture.md` and its linked topic documents own the design specification; root `PLAN.md` owns iteration scope and acceptance status; `roadmap.md` owns the current implementation baseline. Update them together when a decision changes, without duplicating an independent roadmap.
- Iteration 2 restructures environments, sessions and process ownership, not merely a worker interface. Distinguish this model change from iteration 3 execution interaction and iteration 5 remote transport.
- Verify current-code claims against owning sources. Debug workers, service-hosted ETW and TTD recorder processes are different paths; do not describe desired process isolation as implemented.
- Distinguish target requirements, implemented behavior and runtime evidence. Preserve dated validation records as historical evidence. IDA static analysis is external; do not add IDA migration or retirement guides.
- For documentation-only changes, check whitespace, relative links, renamed plan references and semantic consistency. Do not mark a feature accepted or redeploy the service because its specification was updated.
