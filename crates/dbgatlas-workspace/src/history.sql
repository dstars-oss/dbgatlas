CREATE TABLE workspaces (
    id TEXT PRIMARY KEY NOT NULL,
    root TEXT NOT NULL,
    root_key TEXT NOT NULL UNIQUE,
    manifest TEXT NOT NULL,
    ready INTEGER NOT NULL DEFAULT 0,
    with_inputs INTEGER NOT NULL DEFAULT 0,
    last_activity INTEGER NOT NULL
);
CREATE TABLE sessions (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    id TEXT NOT NULL,
    updated INTEGER NOT NULL,
    record TEXT NOT NULL,
    PRIMARY KEY (workspace_id, id)
);
CREATE TABLE executions (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    id TEXT NOT NULL,
    session_id TEXT,
    created INTEGER NOT NULL,
    status TEXT NOT NULL,
    record TEXT NOT NULL,
    audit TEXT,
    PRIMARY KEY (workspace_id, id)
);
CREATE INDEX executions_time ON executions(workspace_id, created DESC, id DESC);
CREATE INDEX executions_session ON executions(workspace_id, session_id, created DESC, id DESC);
CREATE INDEX executions_status ON executions(workspace_id, status, created DESC, id DESC);
CREATE TABLE artifacts (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    id TEXT NOT NULL,
    producer_session_id TEXT,
    producer_execution_id TEXT,
    record TEXT NOT NULL,
    PRIMARY KEY (workspace_id, id)
);
CREATE INDEX artifacts_producer ON artifacts(workspace_id, producer_execution_id);
CREATE TABLE history_entries (
    sequence INTEGER PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    session_id TEXT,
    execution_id TEXT,
    observed INTEGER NOT NULL,
    kind TEXT NOT NULL,
    record TEXT NOT NULL,
    CHECK (session_id IS NOT NULL OR execution_id IS NOT NULL)
);
CREATE INDEX history_execution ON history_entries(workspace_id, execution_id, sequence);
CREATE INDEX history_session ON history_entries(workspace_id, session_id, sequence);
PRAGMA user_version=1;
