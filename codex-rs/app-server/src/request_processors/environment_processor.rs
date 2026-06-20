use super::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct EnvironmentRequestProcessor {
    environment_manager: Arc<EnvironmentManager>,
}

impl EnvironmentRequestProcessor {
    pub(crate) fn new(environment_manager: Arc<EnvironmentManager>) -> Self {
        Self {
            environment_manager,
        }
    }

    pub(crate) async fn environment_add(
        &self,
        params: EnvironmentAddParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.environment_manager
            .upsert_environment(
                params.environment_id,
                params.exec_server_url,
                params.connect_timeout_ms.map(Duration::from_millis),
            )
            .map_err(|err| invalid_request(err.to_string()))?;
        Ok(Some(EnvironmentAddResponse {}.into()))
    }

    pub(crate) async fn environment_upsert(
        &self,
        params: EnvironmentUpsertParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let EnvironmentUpsertParams {
            environment_id,
            transport,
        } = params;
        match transport {
            EnvironmentTransportParams::Websocket {
                exec_server_url,
                connect_timeout_ms,
            } => {
                self.environment_manager
                    .upsert_environment(
                        environment_id,
                        exec_server_url,
                        connect_timeout_ms.map(Duration::from_millis),
                    )
                    .map_err(|err| invalid_request(err.to_string()))?;
            }
            EnvironmentTransportParams::Stdio {
                program,
                args,
                env,
                cwd,
                initialize_timeout_ms,
            } => {
                let cwd = cwd
                    .map(AbsolutePathBuf::try_from)
                    .transpose()
                    .map_err(|err| invalid_request(err.to_string()))?
                    .map(AbsolutePathBuf::into_path_buf);
                self.environment_manager
                    .upsert_stdio_environment(
                        environment_id,
                        program,
                        args.unwrap_or_default(),
                        env.unwrap_or_default(),
                        cwd,
                        initialize_timeout_ms.map(Duration::from_millis),
                    )
                    .map_err(|err| invalid_request(err.to_string()))?;
            }
        }
        Ok(Some(EnvironmentUpsertResponse {}.into()))
    }
}
