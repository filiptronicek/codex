use codex_config::McpServerConfig;
use codex_config::McpServerEnvVar;
use codex_config::McpServerTransportConfig;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::Component;
use std::path::Path;
use tracing::warn;

/// Placement applied while normalizing MCP servers declared by a plugin.
#[derive(Clone, Copy, Debug)]
pub enum PluginMcpServerPlacement<'a> {
    /// Preserve declared placement, resolving a relative working directory below the plugin root.
    Declared,
    /// Bind stdio servers to one environment and default their working directory to the plugin root.
    Environment { environment_id: &'a str },
}

/// One plugin MCP server that could not be normalized into runtime configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginMcpServerParseError {
    pub name: String,
    pub message: String,
}

/// Valid servers and per-server errors parsed from one plugin MCP file.
#[derive(Debug, Default, PartialEq)]
pub struct PluginMcpConfigParseOutcome {
    pub servers: BTreeMap<String, McpServerConfig>,
    pub errors: Vec<PluginMcpServerParseError>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginMcpServersFile {
    mcp_servers: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PluginMcpFile {
    McpServersObject(PluginMcpServersFile),
    ServerMap(BTreeMap<String, JsonValue>),
}

impl PluginMcpFile {
    fn into_mcp_servers(self) -> BTreeMap<String, JsonValue> {
        match self {
            Self::McpServersObject(file) => file.mcp_servers,
            Self::ServerMap(mcp_servers) => mcp_servers,
        }
    }
}

/// Parses the two supported plugin MCP file shapes and normalizes each server.
///
/// Invalid individual servers are returned as errors without discarding valid
/// siblings. A malformed top-level document fails the whole parse.
pub fn parse_plugin_mcp_config(
    plugin_root: &Path,
    contents: &str,
    placement: PluginMcpServerPlacement<'_>,
) -> Result<PluginMcpConfigParseOutcome, serde_json::Error> {
    parse_plugin_mcp_config_with_root(PluginMcpRoot::Host(plugin_root), contents, placement)
}

/// Parses executor-owned plugin MCP config without interpreting the plugin root
/// as a path on the orchestrator host.
pub fn parse_executor_plugin_mcp_config(
    plugin_root: &PathUri,
    contents: &str,
    environment_id: &str,
) -> Result<PluginMcpConfigParseOutcome, serde_json::Error> {
    parse_plugin_mcp_config_with_root(
        PluginMcpRoot::Uri(plugin_root),
        contents,
        PluginMcpServerPlacement::Environment { environment_id },
    )
}

#[derive(Clone, Copy)]
enum PluginMcpRoot<'a> {
    Host(&'a Path),
    Uri(&'a PathUri),
}

impl PluginMcpRoot<'_> {
    fn display(self) -> String {
        match self {
            Self::Host(path) => path.display().to_string(),
            Self::Uri(path) => path.to_string(),
        }
    }

    fn environment_cwd(self, configured_cwd: Option<&str>) -> Result<String, String> {
        match configured_cwd {
            Some(cwd) => executor_plugin_cwd(self, cwd),
            None => Ok(match self {
                Self::Host(path) => path.to_string_lossy().into_owned(),
                Self::Uri(path) => path.to_string(),
            }),
        }
    }

    fn declared_cwd(self, cwd: &str) -> Option<String> {
        match self {
            Self::Host(plugin_root) if !Path::new(cwd).is_absolute() => {
                Some(plugin_root.join(cwd).display().to_string())
            }
            Self::Host(_) | Self::Uri(_) => None,
        }
    }
}

fn parse_plugin_mcp_config_with_root(
    plugin_root: PluginMcpRoot<'_>,
    contents: &str,
    placement: PluginMcpServerPlacement<'_>,
) -> Result<PluginMcpConfigParseOutcome, serde_json::Error> {
    let parsed = serde_json::from_str::<PluginMcpFile>(contents)?;
    let mut outcome = PluginMcpConfigParseOutcome::default();

    for (name, config_value) in parsed.into_mcp_servers() {
        match normalize_plugin_mcp_server(plugin_root, config_value, placement) {
            Ok(config) => {
                outcome.servers.insert(name, config);
            }
            Err(message) => outcome
                .errors
                .push(PluginMcpServerParseError { name, message }),
        }
    }

    Ok(outcome)
}

fn normalize_plugin_mcp_server(
    plugin_root: PluginMcpRoot<'_>,
    value: JsonValue,
    placement: PluginMcpServerPlacement<'_>,
) -> Result<McpServerConfig, String> {
    let mut object = normalize_plugin_mcp_server_value(plugin_root, value, placement);
    if let PluginMcpServerPlacement::Environment { environment_id } = placement {
        object.insert(
            "environment_id".to_string(),
            JsonValue::String(environment_id.to_string()),
        );
        if object.contains_key("command") {
            match object.remove("cwd") {
                Some(JsonValue::String(cwd)) => object.insert(
                    "cwd".to_string(),
                    JsonValue::String(plugin_root.environment_cwd(Some(&cwd))?),
                ),
                Some(JsonValue::Null) | None => object.insert(
                    "cwd".to_string(),
                    JsonValue::String(plugin_root.environment_cwd(None)?),
                ),
                Some(value) => object.insert("cwd".to_string(), value),
            };
        }
    }

    let mut config = serde_json::from_value::<McpServerConfig>(JsonValue::Object(object))
        .map_err(|err| err.to_string())?;
    if matches!(placement, PluginMcpServerPlacement::Environment { .. }) {
        bind_environment_env_vars(&mut config)?;
    }
    Ok(config)
}

fn executor_plugin_cwd(
    plugin_root: PluginMcpRoot<'_>,
    configured_cwd: &str,
) -> Result<String, String> {
    if let Ok(cwd) = PathUri::parse(configured_cwd) {
        return Ok(cwd.to_string());
    }
    if native_path_str_is_absolute(configured_cwd) {
        return match plugin_root {
            PluginMcpRoot::Host(_) => Ok(configured_cwd.to_string()),
            PluginMcpRoot::Uri(path) => path
                .join(configured_cwd)
                .map(|cwd| cwd.to_string())
                .map_err(|err| format!("invalid cwd `{configured_cwd}`: {err}")),
        };
    }
    let cwd = Path::new(configured_cwd);
    if cwd
        .components()
        .any(|component| matches!(component, Component::ParentDir))
        || relative_path_has_parent_component(configured_cwd)
        || configured_cwd.starts_with('/')
        || configured_cwd.starts_with('\\')
        || has_windows_drive_prefix(configured_cwd)
    {
        return Err(format!(
            "relative cwd `{configured_cwd}` must remain within plugin root `{}`",
            plugin_root.display()
        ));
    }
    match plugin_root {
        PluginMcpRoot::Host(path) => Ok(path.join(cwd).to_string_lossy().into_owned()),
        PluginMcpRoot::Uri(path) => path
            .join(configured_cwd)
            .map(|cwd| cwd.to_string())
            .map_err(|err| {
                format!(
                    "relative cwd `{configured_cwd}` must remain within plugin root `{}`: {err}",
                    plugin_root.display()
                )
            }),
    }
}

fn native_path_str_is_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with(r"\\")
        || matches!(
            path.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic()
                    && matches!(separator, b'/' | b'\\')
        )
}

fn has_windows_drive_prefix(path: &str) -> bool {
    matches!(path.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
}

fn relative_path_has_parent_component(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "..")
}

fn bind_environment_env_vars(config: &mut McpServerConfig) -> Result<(), String> {
    let is_local_environment = config.is_local_environment();
    let McpServerTransportConfig::Stdio { env_vars, .. } = &mut config.transport else {
        return Ok(());
    };
    for env_var in env_vars {
        match env_var {
            McpServerEnvVar::Name(name) if !is_local_environment => {
                *env_var = McpServerEnvVar::Config {
                    name: std::mem::take(name),
                    source: Some("remote".to_string()),
                };
            }
            McpServerEnvVar::Name(_) => {}
            McpServerEnvVar::Config { name, source } => {
                match (is_local_environment, source.as_deref()) {
                    (true, None | Some("local")) | (false, Some("remote")) => {}
                    (true, Some("remote")) => {
                        return Err(format!(
                            "env_vars entry `{name}` cannot use source `remote` in a local environment"
                        ));
                    }
                    (false, None) => *source = Some("remote".to_string()),
                    (false, Some("local")) => {
                        return Err(format!(
                            "env_vars entry `{name}` cannot use source `local` in an executor-owned plugin"
                        ));
                    }
                    (_, Some(source)) => unreachable!("validated env_vars source `{source}`"),
                }
            }
        }
    }
    Ok(())
}

fn normalize_plugin_mcp_server_value(
    plugin_root: PluginMcpRoot<'_>,
    value: JsonValue,
    placement: PluginMcpServerPlacement<'_>,
) -> JsonMap<String, JsonValue> {
    let mut object = match value {
        JsonValue::Object(object) => object,
        _ => return JsonMap::new(),
    };

    if let Some(JsonValue::String(transport_type)) = object.remove("type") {
        match transport_type.as_str() {
            "http" | "streamable_http" | "streamable-http" | "stdio" => {}
            other => {
                let plugin_display = plugin_root.display();
                warn!(
                    plugin = %plugin_display,
                    transport = other,
                    "plugin MCP server uses an unknown transport type"
                );
            }
        }
    }

    if let Some(JsonValue::Object(mut oauth)) = object.remove("oauth") {
        if oauth.remove("callbackPort").is_some() {
            let plugin_display = plugin_root.display();
            warn!(
                plugin = %plugin_display,
                "plugin MCP server OAuth callbackPort is ignored; Codex uses global MCP OAuth callback settings"
            );
        }

        if let Some(client_id) = oauth.remove("clientId") {
            oauth.entry("client_id".to_string()).or_insert(client_id);
        }

        if !oauth.is_empty() {
            object.insert("oauth".to_string(), JsonValue::Object(oauth));
        }
    }

    if matches!(placement, PluginMcpServerPlacement::Declared)
        && let Some(JsonValue::String(cwd)) = object.get("cwd")
        && let Some(resolved_cwd) = plugin_root.declared_cwd(cwd)
    {
        object.insert("cwd".to_string(), JsonValue::String(resolved_cwd));
    }

    object
}

#[cfg(test)]
#[path = "plugin_config_tests.rs"]
mod tests;
