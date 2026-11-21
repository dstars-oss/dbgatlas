use super::*;
use dbgatlas_model::Session;
use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One connection shared by the controller and its workspace handles. Workers only open files.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
    connection: Arc<Mutex<Option<Connection>>>,
}

fn normalized_root(path: &Path) -> Result<PathBuf, WorkspaceError> {
    let absolute = std::path::absolute(path).map_err(|source| WorkspaceError::Io {
        path: path.into(),
        source,
    })?;
    let mut ancestor = absolute.as_path();
    let root = loop {
        match fs::canonicalize(ancestor) {
            Ok(real) => break real.join(absolute.strip_prefix(ancestor).expect("path ancestor")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(parent) = ancestor.parent() else {
                    break absolute.clone();
                };
                ancestor = parent;
            }
            Err(source) => {
                return Err(WorkspaceError::Io {
                    path: ancestor.into(),
                    source,
                });
            }
        }
    };
    let root: PathBuf = root.components().collect();
    #[cfg(windows)]
    {
        let text = root.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(drive) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(drive));
        }
    }
    Ok(root)
}

fn root_key(root: &Path) -> String {
    let value = root.to_string_lossy();
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value.into_owned()
    }
}

fn records<T: DeserializeOwned>(
    db: &Connection,
    sql: &str,
    args: impl Params,
) -> Result<Vec<T>, WorkspaceError> {
    let mut statement = db.prepare(sql)?;
    let rows = statement.query_map(args, |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

fn record<T: DeserializeOwned>(
    db: &Connection,
    sql: &str,
    args: impl Params,
) -> Result<Option<T>, WorkspaceError> {
    let value: Option<String> = db.query_row(sql, args, |row| row.get(0)).optional()?;
    Ok(value.map(|s| serde_json::from_str(&s)).transpose()?)
}

impl HistoryStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            connection: Default::default(),
        }
    }

    pub fn local() -> Self {
        let path = std::env::var_os("DBGATLAS_HISTORY_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let base = std::env::var_os("LOCALAPPDATA")
                    .or_else(|| std::env::var_os("XDG_DATA_HOME"))
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share"))
                    })
                    .unwrap_or_else(|| PathBuf::from("."));
                base.join("DbgAtlas/data/history.sqlite3")
            });
        Self::new(path)
    }

    pub fn installed() -> Result<Self, WorkspaceError> {
        let base = std::env::var_os("ProgramData")
            .ok_or_else(|| WorkspaceError::History("ProgramData is missing".into()))?;
        Ok(Self::new(
            PathBuf::from(base).join("DbgAtlas/data/history.sqlite3"),
        ))
    }

    fn db<T>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T, WorkspaceError>,
    ) -> Result<T, WorkspaceError> {
        let mut slot = self
            .connection
            .lock()
            .map_err(|_| WorkspaceError::History("database lock poisoned".into()))?;
        if slot.is_none() {
            if let Some(parent) = self.path.parent() {
                create_dir(parent)?;
            }
            let mut db = Connection::open(&self.path)?;
            db.busy_timeout(Duration::from_secs(10))?;
            db.execute_batch("PRAGMA foreign_keys=ON;")?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let version: u32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            match version {
                0 => {
                    let existing: i64 = tx.query_row(
                        "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                        [],
                        |r| r.get(0),
                    )?;
                    if existing != 0 {
                        return Err(WorkspaceError::History(
                            "unrecognized history database; original preserved".into(),
                        ));
                    }
                    tx.execute_batch(include_str!("history.sql"))?;
                }
                1 => {}
                _ => {
                    return Err(WorkspaceError::History(format!(
                        "unsupported history database format {version}; original preserved"
                    )));
                }
            }
            tx.commit()?;
            *slot = Some(db);
        }
        action(slot.as_mut().expect("connection initialized"))
    }

    pub fn init_workspace(
        &self,
        root: impl AsRef<Path>,
        options: WorkspaceInitOptions,
    ) -> Result<Workspace, WorkspaceError> {
        let root = normalized_root(root.as_ref())?;
        self.db(|db| {
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing:Option<(String,bool)>=tx.query_row("SELECT manifest,ready FROM workspaces WHERE root_key=?1",[root_key(&root)],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            match existing {
                Some((_,true)) => return Err(WorkspaceError::AlreadyExists(root.clone())),
                Some((_,false)) => {},
                None => {
                    if root.join(MANIFEST_FILE).exists() { return Err(WorkspaceError::AlreadyExists(root.clone())); }
                    let manifest=WorkspaceManifest {schema_version:2,workspace_id:Id::new(uuid::Uuid::new_v4().to_string())?,created_at:Timestamp::now(),
                        tool:ToolInfo{name:"DbgAtlas".into(),version:env!("CARGO_PKG_VERSION").into()}};
                    tx.execute("INSERT INTO workspaces(id,root,root_key,manifest,with_inputs,last_activity) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![manifest.workspace_id.as_str(),root.to_string_lossy(),root_key(&root),serde_json::to_string(&manifest)?,options.create_inputs,manifest.created_at.unix_millis])?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        self.finish_registration(&root)
    }

    fn finish_registration(&self, root: &Path) -> Result<Workspace, WorkspaceError> {
        self.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (encoded, with_inputs, ready): (String, bool, bool) = tx.query_row(
                "SELECT manifest,with_inputs,ready FROM workspaces WHERE root_key=?1",
                [root_key(root)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let manifest: WorkspaceManifest = serde_json::from_str(&encoded)?;
            let path = root.join(MANIFEST_FILE);
            if !ready {
                create_dir(root)?;
                create_dir(root.join(ARTIFACTS_DIR))?;
                create_dir(root.join(ANALYSIS_DIR))?;
                if with_inputs {
                    create_dir(root.join(INPUTS_DIR))?;
                }
                if !path.exists() {
                    let io = |source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    };
                    let mut file = tempfile::NamedTempFile::new_in(root).map_err(io)?;
                    serde_json::to_writer_pretty(&mut file, &manifest)?;
                    file.as_file().sync_all().map_err(io)?;
                    file.persist_noclobber(&path).map_err(|e| io(e.error))?;
                }
            }
            if Workspace::load_manifest(root)?.workspace_id != manifest.workspace_id {
                return Err(WorkspaceError::History(
                    "conflicting workspace identity".into(),
                ));
            }
            tx.execute(
                "UPDATE workspaces SET ready=1 WHERE id=?1",
                [manifest.workspace_id.as_str()],
            )?;
            tx.commit()?;
            Ok(Workspace {
                root: root.to_owned(),
                manifest,
                history: self.clone(),
            })
        })
    }

    pub fn workspaces(&self) -> Result<Vec<PathBuf>, WorkspaceError> {
        self.db(|db| {
            let mut stmt = db.prepare(
                "SELECT root FROM workspaces WHERE ready=1 ORDER BY last_activity DESC, id",
            )?;
            let roots = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(roots
                .map(|v| v.map(PathBuf::from))
                .collect::<Result<_, _>>()?)
        })
    }

    /// A catalog lookup remains usable when the project volume is offline.
    pub fn lookup(&self, root: impl AsRef<Path>) -> Result<Workspace, WorkspaceError> {
        let path = root.as_ref();
        let absolute = std::path::absolute(path).map_err(|source| WorkspaceError::Io {
            path: path.into(),
            source,
        })?;
        self.db(|db| {
            let fetch = |key: String| {
                db.query_row(
                    "SELECT root,manifest FROM workspaces WHERE root_key=?1 AND ready=1",
                    [key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
            };
            // The catalog path works even if its entire volume cannot be opened.
            let root = absolute;
            let mut found = fetch(root_key(&root))?;
            if found.is_none() {
                let canonical = normalized_root(&root)?;
                if canonical != root {
                    found = fetch(root_key(&canonical))?;
                }
            }
            let Some((stored, json)) = found else {
                if root.join(MANIFEST_FILE).exists() {
                    Workspace::load_manifest(&root)?;
                }
                return Err(WorkspaceError::History(format!(
                    "workspace is not registered in this history database: {}",
                    root.display()
                )));
            };
            Ok(Workspace {
                root: stored.into(),
                manifest: serde_json::from_str(&json)?,
                history: self.clone(),
            })
        })
    }

    pub fn open_workspace(&self, root: impl AsRef<Path>) -> Result<Workspace, WorkspaceError> {
        let root = root.as_ref();
        let manifest = Workspace::load_manifest(root)?;
        let workspace = self.lookup(root)?;
        if manifest.workspace_id != workspace.manifest.workspace_id {
            return Err(WorkspaceError::History(
                "workspace identity conflicts with its registered path".into(),
            ));
        }
        Ok(workspace)
    }

    pub fn relink(&self, root: &Path, service_instance: &str) -> Result<Workspace, WorkspaceError> {
        let root = normalized_root(root)?;
        let manifest = Workspace::load_manifest(&root)?;
        self.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old: String = tx
                .query_row(
                    "SELECT root FROM workspaces WHERE id=?1 AND ready=1",
                    [manifest.workspace_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| WorkspaceError::History("unknown workspace identity".into()))?;
            let old = PathBuf::from(old);
            if root_key(&root) != root_key(&old)
                && old.try_exists().map_err(|source| WorkspaceError::Io {
                    path: old.clone(),
                    source,
                })?
            {
                return Err(WorkspaceError::History(
                    "the registered workspace still exists; refusing to bind a copy".into(),
                ));
            }
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE workspace_id=?1
                AND json_extract(record,'$.service_instance')=?2
                AND (json_extract(record,'$.lifecycle') NOT IN ('failed','closed')
                     OR coalesce(json_extract(record,'$.cleanup.complete'),0)=0))",
                params![manifest.workspace_id.as_str(), service_instance],
                |r| r.get(0),
            )?;
            if active {
                return Err(WorkspaceError::History(
                    "close active sessions before relocating their workspace".into(),
                ));
            }
            tx.execute(
                "UPDATE workspaces SET root=?2,root_key=?3 WHERE id=?1",
                params![
                    manifest.workspace_id.as_str(),
                    root.to_string_lossy(),
                    root_key(&root)
                ],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        self.open_workspace(root)
    }
    pub fn ensure_workspace(&self, root: impl AsRef<Path>) -> Result<Workspace, WorkspaceError> {
        let root = normalized_root(root.as_ref())?;
        let ready = self.db(|db| {
            Ok(db
                .query_row(
                    "SELECT ready FROM workspaces WHERE root_key=?1",
                    [root_key(&root)],
                    |r| r.get::<_, bool>(0),
                )
                .optional()?)
        })?;
        match ready {
            Some(true) => self.open_workspace(root),
            Some(false) => self.finish_registration(&root),
            None => match self.init_workspace(&root, Default::default()) {
                Err(WorkspaceError::AlreadyExists(_)) => self.open_workspace(root),
                result => result,
            },
        }
    }
}

impl Workspace {
    fn wid(&self) -> &str {
        self.manifest.workspace_id.as_str()
    }

    pub fn save_session(&self, session: &Session) -> Result<(), WorkspaceError> {
        self.history.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let registered: String = tx.query_row(
                "SELECT root_key FROM workspaces WHERE id=?1", [self.wid()], |r| r.get(0)
            )?;
            if registered != root_key(&session.workspace.root) {
                return Err(WorkspaceError::History("session workspace has moved".into()));
            }
            let session_id = session.session_id.id.as_str();
            let previous: Option<Session> = record(&tx,
                "SELECT record FROM sessions WHERE workspace_id=?1 AND id=?2",
                params![self.wid(), session_id],
            )?;
            if previous.as_ref().is_some_and(|old|
                old.updated_at.unix_millis > session.updated_at.unix_millis ||
                (old.lifecycle.is_terminal() && !session.lifecycle.is_terminal())
            ) { return Ok(()); }
            let data = serde_json::to_string(session)?;
            let observed=session.updated_at.unix_millis;
            tx.execute(
                "INSERT INTO sessions(workspace_id,id,updated,record) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(workspace_id,id) DO UPDATE SET updated=excluded.updated,record=excluded.record",
                params![self.wid(), session_id, observed, data],
            )?;
            if previous.as_ref().is_none_or(|old| old.lifecycle != session.lifecycle
                || old.target_state.state != session.target_state.state || old.connection != session.connection) {
                tx.execute(
                    "INSERT INTO history_entries(workspace_id,session_id,observed,kind,record)
                     VALUES(?1,?2,?3,'session_state',?4)",
                    params![self.wid(),session_id,observed,data],
                )?;
            }
            let before=previous.as_ref().and_then(|s|s.context.get("last_event"));
            if let Some(event)=session.context.get("last_event").filter(|event| !event.is_null() && Some(*event)!=before) {
                tx.execute(
                    "INSERT INTO history_entries(workspace_id,session_id,observed,kind,record)
                     VALUES(?1,?2,?3,'engine_event',?4)",
                    params![self.wid(),session_id,session.target_state.observed_at.unix_millis,serde_json::to_string(event)?],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn sessions(&self) -> Result<Vec<Session>, WorkspaceError> {
        self.history.db(|db| {
            records(
                db,
                "SELECT record FROM sessions WHERE workspace_id=?1 ORDER BY updated DESC,id",
                [self.wid()],
            )
        })
    }

    pub fn session_event(
        &self,
        id: &str,
        kind: &str,
        event: &serde_json::Value,
    ) -> Result<(), WorkspaceError> {
        self.history.db(|db| {
            db.execute("INSERT INTO history_entries(workspace_id,session_id,observed,kind,record) VALUES(?1,?2,?3,?4,?5)",
                params![self.wid(),id,Timestamp::now().unix_millis,kind,serde_json::to_string(event)?])?;
            Ok(())
        })
    }

    pub fn history_entries(
        &self,
        execution_id: Option<&str>,
        session_id: Option<&str>,
        after: i64,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, WorkspaceError> {
        self.history.db(|db| records(db,
            "SELECT json_object('sequence',sequence,'observed',observed,'kind',kind,'session_id',session_id,'execution_id',execution_id,'record',json(record))
             FROM history_entries WHERE workspace_id=?1 AND (?2 IS NULL OR execution_id=?2) AND (?3 IS NULL OR session_id=?3) AND sequence>?4 ORDER BY sequence LIMIT ?5",
            params![self.wid(),execution_id,session_id,after,limit.clamp(1,100)]))
    }

    pub fn append_operation(&self, record_in: &OperationRecord) -> Result<(), WorkspaceError> {
        self.history.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let id = record_in.operation_id.id.as_str();
            let previous = record(&tx,
                "SELECT record FROM executions WHERE workspace_id=?1 AND id=?2",
                params![self.wid(), id],
            )?;
            let mut item = record_in.clone();
            if let Some(previous) = previous { merge_update(&mut item,previous); }
            let encoded = serde_json::to_string(&item)?;
            let session_id = item.details.session_id.as_ref().map(|s| s.id.as_str());
            let now = Timestamp::now().unix_millis;
            let status = serde_json::to_value(&item.status)?;
            tx.execute(
                "INSERT INTO executions(workspace_id,id,session_id,created,status,record)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(workspace_id,id) DO UPDATE SET session_id=excluded.session_id,
                 status=excluded.status,record=excluded.record",
                params![self.wid(),id,session_id,item.created_at.unix_millis,status.as_str(),encoded],
            )?;
            tx.execute(
                "INSERT INTO history_entries(workspace_id,session_id,execution_id,observed,kind,record)
                 VALUES(?1,?2,?3,?4,'execution_update',?5)",
                params![self.wid(),session_id,id,now,encoded],
            )?;
            tx.execute(
                "UPDATE workspaces SET last_activity=max(last_activity,?2) WHERE id=?1",
                params![self.wid(),now],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn list_operations(&self) -> Result<Vec<OperationRecord>, WorkspaceError> {
        self.history.db(|db| {
            records(
                db,
                "SELECT record FROM executions WHERE workspace_id=?1 ORDER BY created,id",
                [self.wid()],
            )
        })
    }
    pub fn get_operation(
        &self,
        id: &OperationRef,
    ) -> Result<Option<OperationRecord>, WorkspaceError> {
        self.history.db(|db| {
            record(
                db,
                "SELECT record FROM executions WHERE workspace_id=?1 AND id=?2",
                params![self.wid(), id.id.as_str()],
            )
        })
    }
    pub fn append_command_audit(&self, item: &CommandAuditRecord) -> Result<(), WorkspaceError> {
        self.history.db(|db| {
            let changed = db.execute(
                "UPDATE executions SET audit=?3 WHERE workspace_id=?1 AND id=?2",
                params![
                    self.wid(),
                    item.operation_id.id.as_str(),
                    serde_json::to_string(item)?
                ],
            )?;
            if changed == 0 {
                return Err(WorkspaceError::History(
                    "command audit has no matching execution".into(),
                ));
            }
            Ok(())
        })
    }
    pub fn list_command_audit(&self) -> Result<Vec<CommandAuditRecord>, WorkspaceError> {
        self.history.db(|db|records(db,"SELECT audit FROM executions WHERE workspace_id=?1 AND audit IS NOT NULL ORDER BY created,id",[self.wid()]))
    }
    pub(super) fn save_artifact(&self, metadata: &ArtifactMetadata) -> Result<(), WorkspaceError> {
        self.history.db(|db| {
            let encoded=serde_json::to_string(metadata)?;
            let n=db.execute("INSERT INTO artifacts(workspace_id,id,producer_session_id,producer_execution_id,record) VALUES(?1,?2,?3,?4,?5)
                ON CONFLICT(workspace_id,id) DO UPDATE SET record=excluded.record WHERE artifacts.record=excluded.record",
                params![self.wid(),metadata.artifact_id.id.as_str(),metadata.producer_session_id.as_ref().map(|s|s.id.as_str()),metadata.producer_execution_id.as_ref().map(|s|s.id.as_str()),encoded])?;
            if n==0 {return Err(WorkspaceError::History("conflicting artifact identity".into()));}
            Ok(())
        })
    }
    pub fn list_artifacts(&self) -> Result<Vec<ArtifactMetadata>, WorkspaceError> {
        self.history.db(|db| {
            records(
                db,
                "SELECT record FROM artifacts WHERE workspace_id=?1 ORDER BY rowid",
                [self.wid()],
            )
        })
    }
    pub fn get_artifact(
        &self,
        id: &ArtifactRef,
    ) -> Result<Option<ArtifactMetadata>, WorkspaceError> {
        self.history.db(|db| {
            record(
                db,
                "SELECT record FROM artifacts WHERE workspace_id=?1 AND id=?2",
                params![self.wid(), id.id.as_str()],
            )
        })
    }
    pub fn artifact_consumers(&self, id: &ArtifactRef) -> Result<Vec<Session>, WorkspaceError> {
        self.history.db(|db|records(db,"SELECT s.record FROM sessions s WHERE workspace_id=?1 AND EXISTS
            (SELECT 1 FROM json_each(s.record,'$.input_artifacts') a WHERE json_extract(a.value,'$.id')=?2) ORDER BY updated,id",
            params![self.wid(),id.id.as_str()]))
    }
}

#[derive(Default, Deserialize)]
pub struct HistoryFilter {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub search: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    #[serde(default)]
    pub offset: u32,
    pub limit: Option<u32>,
}

#[derive(Serialize)]
pub struct OperationPage {
    pub operations: Vec<OperationRecord>,
    pub artifacts: Vec<ArtifactMetadata>,
    pub total_operations: usize,
    pub total_artifacts: usize,
    pub filtered_count: usize,
    pub offset: usize,
    pub has_more: bool,
}

impl Workspace {
    pub fn operation_page(
        &self,
        filter: &HistoryFilter,
        overrides: &serde_json::Value,
    ) -> Result<OperationPage, WorkspaceError> {
        self.history.db(|db| {
            let tx=db.transaction()?;
            let mut scope=String::new();
            if !filter.session_id.is_empty(){scope.push_str(" AND e.session_id=?3");}
            if filter.start_ms.is_some(){scope.push_str(" AND e.created>=?6");}
            if filter.end_ms.is_some(){scope.push_str(" AND e.created<?7");}
            let sql=include_str!("operation_page.sql").replace("/*scope*/",&scope);
            let (count,rows):(usize,String)=tx.query_row(&sql,params![self.wid(),serde_json::to_string(overrides)?,filter.session_id,
                filter.status,filter.search,filter.start_ms,filter.end_ms,filter.limit.unwrap_or(40).clamp(1,100),filter.offset],|r|Ok((r.get(0)?,r.get(1)?)))?;

            let operations:Vec<OperationRecord>=serde_json::from_str(&rows)?;
            let ids:Vec<_>=operations.iter().flat_map(|op|op.artifacts.iter().chain(op.raw_output.iter())).map(|a|a.id.as_str()).collect();
            let artifacts=records(&tx,"SELECT record FROM artifacts WHERE workspace_id=?1 AND id IN (SELECT value FROM json_each(?2))",
                params![self.wid(),serde_json::to_string(&ids)?])?;
            let (total_operations,total_artifacts)=tx.query_row("SELECT (SELECT count(*) FROM executions WHERE workspace_id=?1),
                (SELECT count(*) FROM artifacts WHERE workspace_id=?1)",[self.wid()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            tx.commit()?;
            let offset=(filter.offset as usize).min(count);
            let has_more=offset+operations.len()<count;
            Ok(OperationPage{operations,artifacts,total_operations,total_artifacts,filtered_count:count,offset,has_more})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_retry_finishes_a_persisted_intent_and_rejects_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("history.sqlite3");
        let root = tmp.path().join("project/dbgatlas");
        let catalog = HistoryStore::new(db.clone());
        let workspace = catalog.init_workspace(&root, Default::default()).unwrap();
        let id = workspace.manifest.workspace_id.clone();
        catalog
            .db(|conn| {
                conn.execute("UPDATE workspaces SET ready=0", [])?;
                Ok(())
            })
            .unwrap();
        drop(workspace);
        drop(catalog);
        let catalog = HistoryStore::new(db);
        assert!(catalog.workspaces().unwrap().is_empty());
        assert_eq!(
            catalog
                .ensure_workspace(&root)
                .unwrap()
                .manifest
                .workspace_id,
            id
        );
        let copy = tmp.path().join("copy");
        fs::create_dir(&copy).unwrap();
        fs::copy(root.join(MANIFEST_FILE), copy.join(MANIFEST_FILE)).unwrap();
        assert!(catalog.relink(&copy, "test").is_err());
        assert_eq!(catalog.workspaces().unwrap(), vec![root]);
    }
}
