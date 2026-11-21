use super::*;

impl ServiceHost {
    pub(super) fn workspace_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, ServiceError> {
        if method == "history.query" {
            return self.management_query(&params);
        }
        let path = Path::new(
            params["path"]
                .as_str()
                .ok_or_else(|| ServiceError::Rpc("workspace path is required".into()))?,
        );
        if !path.is_absolute() {
            return Err(ServiceError::Rpc("workspace path must be absolute".into()));
        }
        match method {
            "workspace.init" => {
                let ws = self.history.init_workspace(
                    path,
                    dbgatlas_workspace::WorkspaceInitOptions {
                        create_inputs: params["with_inputs"].as_bool().unwrap_or(false),
                    },
                )?;
                Ok(
                    json!({"root":ws.root(),"manifest":ws.manifest_path(),"workspace_id":ws.manifest().workspace_id}),
                )
            }
            "workspace.info" => Ok(json!(self.history.lookup(path)?.info())),
            "workspace.relink" => Ok(json!(
                self.history.relink(path, &self.service_instance)?.info()
            )),
            _ => Err(ServiceError::Rpc(format!(
                "unknown workspace method {method}"
            ))),
        }
    }
    pub(super) fn ensure_project_workspace(
        &self,
        project: &Path,
    ) -> Result<Workspace, ServiceError> {
        Ok(self
            .history
            .ensure_workspace(project.join(INTERNAL_WORKSPACE_DIR))?)
    }
    pub(super) fn project_workspace(
        &self,
        params: Option<Value>,
    ) -> Result<Workspace, ServiceError> {
        let params: ProjectRootParams = parse_params(params)?;
        Ok(self
            .history
            .lookup(params.project_root.join(INTERNAL_WORKSPACE_DIR))?)
    }
}
