use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use ts_rs::TS;

use codex_utils_path_uri::LegacyAppPathString;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentAddParams {
    pub environment_id: String,
    pub exec_server_url: String,
    /// Optional WebSocket connection timeout. The server default applies when omitted.
    #[ts(type = "number | null")]
    #[ts(optional = nullable)]
    pub connect_timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentAddResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentUpsertParams {
    pub environment_id: String,
    pub transport: EnvironmentTransportParams,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum EnvironmentTransportParams {
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Websocket {
        exec_server_url: String,
        /// Optional WebSocket connection timeout. The server default applies when omitted.
        #[ts(type = "number | null")]
        #[ts(optional = nullable)]
        connect_timeout_ms: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Stdio {
        program: String,
        #[ts(optional = nullable)]
        args: Option<Vec<String>>,
        #[ts(optional = nullable)]
        env: Option<HashMap<String, String>>,
        #[ts(optional = nullable)]
        cwd: Option<LegacyAppPathString>,
        /// Optional stdio initialize timeout. The server default applies when omitted.
        #[ts(type = "number | null")]
        #[ts(optional = nullable)]
        initialize_timeout_ms: Option<u64>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct EnvironmentUpsertResponse {}
