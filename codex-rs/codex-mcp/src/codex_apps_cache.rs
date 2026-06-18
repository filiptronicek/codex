//! Shared raw tool cache for the host-owned Codex Apps MCP server.
//!
//! Cache entries are process-local live state scoped by the actual Codex Apps
//! catalog principal and catalog source. Disk is best-effort cold-start
//! persistence; entries do not reread disk after creation.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::Context;
use arc_swap::ArcSwapOption;
use codex_config::McpServerConfig;
use codex_login::CodexAuth;
use codex_protocol::mcp::McpServerInfo;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use sha1::Digest;
use sha1::Sha1;

use crate::mcp::CODEX_APPS_MCP_SERVER_NAME;
use crate::runtime::emit_duration;
use crate::tools::MCP_TOOLS_CACHE_WRITE_DURATION_METRIC;
use crate::tools::ToolInfo;

const MCP_TOOLS_CACHE_PUBLISH_DURATION_METRIC: &str = "codex.mcp.tools.cache_publish.duration_ms";

/// The CodexAuth bits that identify a Codex Apps catalog.
///
/// This is enough for the normal CodexAuth-backed path. If
/// `CODEX_CONNECTORS_TOKEN` is set, Codex Apps uses that bearer token instead,
/// so the full cache identity uses the token's fingerprint instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodexAppsToolsCacheKey {
    pub(crate) account_id: Option<String>,
    pub(crate) chatgpt_user_id: Option<String>,
    pub(crate) is_workspace_account: bool,
}

/// Builds the CodexAuth-backed part of the Codex Apps cache key.
///
/// We cannot decide the env-token-backed case here because the per-server env
/// var has not been resolved yet.
pub fn codex_apps_tools_cache_key(auth: Option<&CodexAuth>) -> CodexAppsToolsCacheKey {
    CodexAppsToolsCacheKey {
        account_id: auth.and_then(CodexAuth::get_account_id),
        chatgpt_user_id: auth.and_then(CodexAuth::get_chatgpt_user_id),
        is_workspace_account: auth.is_some_and(CodexAuth::is_workspace_account),
    }
}

/// Process-scoped registry for shared Codex Apps raw tool snapshots.
///
/// Two clients share an entry only when they would read the same Codex Apps
/// catalog. New entries may seed from disk; live entries read from memory only.
#[derive(Clone, Default)]
pub struct CodexAppsToolsCache {
    entries: Arc<Mutex<HashMap<CodexAppsToolsCacheIdentity, Arc<CodexAppsToolsCacheEntry>>>>,
}

/// Handle to one shared Codex Apps tools cache entry.
///
/// The connection manager creates this from the catalog identity, then tool
/// reads and refreshes for that managed client use the same entry.
#[derive(Clone)]
pub(crate) struct CodexAppsToolsCacheContext {
    entry: Arc<CodexAppsToolsCacheEntry>,
}

impl CodexAppsToolsCacheContext {
    pub(crate) fn tools_cache_path(&self) -> PathBuf {
        self.entry
            .identity
            .cache_path_in(CODEX_APPS_TOOLS_CACHE_DIR)
    }

    pub(crate) fn server_info_cache_path(&self) -> PathBuf {
        self.entry
            .identity
            .cache_path_in(CODEX_APPS_SERVER_INFO_CACHE_DIR)
    }

    pub(crate) fn current_tools(&self) -> Option<Vec<ToolInfo>> {
        self.entry
            .current_tools
            .load_full()
            .map(|tools| tools.as_ref().clone())
    }

    pub(crate) fn has_current_tools(&self) -> bool {
        self.entry.current_tools.load_full().is_some()
    }

    pub(crate) fn begin_fetch(
        &self,
        source: CodexAppsToolsFetchSource,
    ) -> CodexAppsToolsFetchTicket {
        CodexAppsToolsFetchTicket {
            generation: self
                .entry
                .next_fetch_generation
                .fetch_add(1, Ordering::Relaxed)
                + 1,
            source,
        }
    }

    pub(crate) fn publish_if_newest_accepted(
        &self,
        ticket: CodexAppsToolsFetchTicket,
        server_info: &McpServerInfo,
        tools: Vec<ToolInfo>,
    ) -> Vec<ToolInfo> {
        let publish_start = Instant::now();
        let mut publication_state = lock_unpoisoned(&self.entry.publication_state);
        if ticket.generation <= publication_state.last_accepted_generation {
            emit_duration(
                MCP_TOOLS_CACHE_PUBLISH_DURATION_METRIC,
                publish_start.elapsed(),
                &[("source", ticket.source.as_str()), ("result", "stale")],
            );
            return self.current_tools().unwrap_or(tools);
        }

        publication_state.last_accepted_generation = ticket.generation;
        self.entry
            .current_tools
            .store(Some(Arc::new(tools.clone())));
        persist_codex_apps_cache(self, server_info, &tools);
        emit_duration(
            MCP_TOOLS_CACHE_PUBLISH_DURATION_METRIC,
            publish_start.elapsed(),
            &[("source", ticket.source.as_str()), ("result", "published")],
        );
        tools
    }

    #[cfg(test)]
    pub(crate) fn store_current_tools_for_test(&self, tools: Vec<ToolInfo>) {
        self.entry.current_tools.store(Some(Arc::new(tools)));
    }
}

impl CodexAppsToolsCache {
    pub(crate) fn context(
        &self,
        codex_home: PathBuf,
        auth_key: CodexAppsToolsCacheKey,
        config: &McpServerConfig,
        resolved_bearer_token: Option<&str>,
    ) -> CodexAppsToolsCacheContext {
        // Build the cache entry from the credential the request will actually
        // use. Normal Codex Apps uses CodexAuth; an env bearer token overrides it.
        let identity =
            CodexAppsToolsCacheIdentity::new(codex_home, auth_key, config, resolved_bearer_token);
        let mut entries = lock_unpoisoned(&self.entries);
        let entry = entries
            .entry(identity.clone())
            .or_insert_with(|| Arc::new(CodexAppsToolsCacheEntry::new(identity)))
            .clone();
        CodexAppsToolsCacheContext { entry }
    }

    #[cfg(test)]
    pub(crate) fn context_for_test(
        &self,
        codex_home: PathBuf,
        auth_key: CodexAppsToolsCacheKey,
    ) -> CodexAppsToolsCacheContext {
        self.context(
            codex_home,
            auth_key,
            &crate::codex_apps_mcp_server_config(
                "https://chatgpt.com",
                /*apps_mcp_product_sku*/ None,
            ),
            /*resolved_bearer_token*/ None,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CodexAppsToolsFetchSource {
    Startup,
    HardRefresh,
}

impl CodexAppsToolsFetchSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::HardRefresh => "hard_refresh",
        }
    }
}

pub(crate) struct CodexAppsToolsFetchTicket {
    generation: u64,
    source: CodexAppsToolsFetchSource,
}

struct CodexAppsToolsCacheEntry {
    identity: CodexAppsToolsCacheIdentity,
    current_tools: ArcSwapOption<Vec<ToolInfo>>,
    next_fetch_generation: AtomicU64,
    publication_state: Mutex<CodexAppsToolsPublicationState>,
}

impl CodexAppsToolsCacheEntry {
    fn new(identity: CodexAppsToolsCacheIdentity) -> Self {
        let current_tools = load_cached_codex_apps_tools_for_identity(&identity).map(Arc::new);
        Self {
            identity,
            current_tools: ArcSwapOption::from(current_tools),
            next_fetch_generation: AtomicU64::new(0),
            publication_state: Mutex::new(CodexAppsToolsPublicationState::default()),
        }
    }
}

#[derive(Default)]
struct CodexAppsToolsPublicationState {
    last_accepted_generation: u64,
}

/// Everything that decides whether two Codex Apps clients can share tools.
///
/// The principal says whose catalog we are reading. The source fingerprint
/// says which Codex Apps endpoint/config we are reading from. `codex_home`
/// keeps the persisted cache under the right home directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
struct CodexAppsToolsCacheIdentity {
    codex_home: PathBuf,
    catalog_principal: CodexAppsCatalogPrincipal,
    catalog_source_fingerprint: CodexAppsCatalogSourceFingerprint,
}

impl CodexAppsToolsCacheIdentity {
    fn new(
        codex_home: PathBuf,
        auth_key: CodexAppsToolsCacheKey,
        config: &McpServerConfig,
        resolved_bearer_token: Option<&str>,
    ) -> Self {
        let catalog_principal = match resolved_bearer_token {
            Some(token) => {
                // For built-in Codex Apps this is the resolved
                // `CODEX_CONNECTORS_TOKEN` override. That token, not CodexAuth,
                // decides which catalog MCP returns, so it has to split the cache.
                // Store only a fingerprint; never put the raw token in cache state
                // or disk paths.
                CodexAppsCatalogPrincipal::EnvBearerTokenFingerprint(
                    CodexAppsBearerTokenFingerprint(sha1_hex(token)),
                )
            }
            None => CodexAppsCatalogPrincipal::CodexAuth(auth_key),
        };
        Self {
            codex_home,
            catalog_principal,
            catalog_source_fingerprint: codex_apps_catalog_source_fingerprint(config),
        }
    }

    fn cache_path_in(&self, cache_dir: &str) -> PathBuf {
        let identity_json = serde_json::to_string(self).unwrap_or_default();
        let identity_hash = sha1_hex(&identity_json);
        self.codex_home
            .join(cache_dir)
            .join(format!("{identity_hash}.json"))
    }
}

/// The credential-derived piece of the Codex Apps cache key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
enum CodexAppsCatalogPrincipal {
    /// Normal Codex Apps path: the active CodexAuth account/user/workspace.
    CodexAuth(CodexAppsToolsCacheKey),
    /// Env-token-backed path: the resolved bearer token selects the catalog.
    EnvBearerTokenFingerprint(CodexAppsBearerTokenFingerprint),
}

/// Fingerprint of a resolved env-backed Codex Apps bearer token.
///
/// We use this only to keep two different env tokens from sharing a tools
/// cache entry. The raw token never goes into the cache key or path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
struct CodexAppsBearerTokenFingerprint(String);

/// Fingerprint of the Codex Apps MCP config that can change the catalog source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
struct CodexAppsCatalogSourceFingerprint(String);

/// Config fields that can change which Codex Apps catalog we query.
///
/// The transport includes the URL and configured headers, including the product
/// SKU header added by `codex_apps_mcp_server_config`.
#[derive(Serialize)]
struct CodexAppsCatalogSource<'a> {
    environment_id: &'a str,
    transport: &'a codex_config::McpServerTransportConfig,
}

fn codex_apps_catalog_source_fingerprint(
    config: &McpServerConfig,
) -> CodexAppsCatalogSourceFingerprint {
    // Header maps can serialize in different orders. Normalize first so the
    // same config still lands in the same cache entry.
    let source = CodexAppsCatalogSource {
        environment_id: &config.environment_id,
        transport: &config.transport,
    };
    let source_json = serde_json::to_value(source)
        .map(canonical_json)
        .and_then(|source| serde_json::to_string(&source))
        .unwrap_or_default();
    CodexAppsCatalogSourceFingerprint(sha1_hex(&source_json))
}

fn canonical_json(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(canonical_json).collect())
        }
        JsonValue::Object(values) => JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
pub(crate) fn write_cached_codex_apps_tools_if_needed(
    server_name: &str,
    cache_context: Option<&CodexAppsToolsCacheContext>,
    server_info: &McpServerInfo,
    tools: &[ToolInfo],
) {
    if server_name != CODEX_APPS_MCP_SERVER_NAME {
        return;
    }

    if let Some(cache_context) = cache_context {
        cache_context
            .entry
            .current_tools
            .store(Some(Arc::new(tools.to_vec())));
        persist_codex_apps_cache(cache_context, server_info, tools);
    }
}

pub(crate) fn load_startup_cached_codex_apps_server_info(
    server_name: &str,
    cache_context: Option<&CodexAppsToolsCacheContext>,
) -> Option<McpServerInfo> {
    if server_name != CODEX_APPS_MCP_SERVER_NAME {
        return None;
    }

    load_cached_codex_apps_server_info(cache_context?)
}

#[cfg(test)]
pub(crate) fn read_cached_codex_apps_tools(
    cache_context: &CodexAppsToolsCacheContext,
) -> Option<Vec<ToolInfo>> {
    load_cached_codex_apps_tools_for_identity(&cache_context.entry.identity)
}

fn load_cached_codex_apps_tools_for_identity(
    identity: &CodexAppsToolsCacheIdentity,
) -> Option<Vec<ToolInfo>> {
    let cache_path = identity.cache_path_in(CODEX_APPS_TOOLS_CACHE_DIR);
    let bytes = std::fs::read(cache_path).ok()?;
    let cache: CodexAppsToolsDiskCache = serde_json::from_slice(&bytes).ok()?;
    (cache.schema_version == CODEX_APPS_TOOLS_CACHE_SCHEMA_VERSION).then_some(cache.tools)
}

pub(crate) fn write_cached_codex_apps_tools(
    cache_context: &CodexAppsToolsCacheContext,
    tools: &[ToolInfo],
) -> anyhow::Result<()> {
    let cache_path = cache_context.tools_cache_path();
    let bytes = serde_json::to_vec_pretty(&CodexAppsToolsDiskCache {
        schema_version: CODEX_APPS_TOOLS_CACHE_SCHEMA_VERSION,
        tools: tools.to_vec(),
    })
    .context("failed to serialize Codex Apps tools cache")?;
    write_codex_apps_cache_file(&cache_path, "tools", bytes)
}

fn load_cached_codex_apps_server_info(
    cache_context: &CodexAppsToolsCacheContext,
) -> Option<McpServerInfo> {
    let bytes = std::fs::read(cache_context.server_info_cache_path()).ok()?;
    let cache: CodexAppsServerInfoDiskCache = serde_json::from_slice(&bytes).ok()?;
    (cache.schema_version == CODEX_APPS_SERVER_INFO_CACHE_SCHEMA_VERSION)
        .then_some(cache.server_info)
}

fn write_cached_codex_apps_server_info(
    cache_context: &CodexAppsToolsCacheContext,
    server_info: &McpServerInfo,
) -> anyhow::Result<()> {
    let cache_path = cache_context.server_info_cache_path();
    let bytes = serde_json::to_vec_pretty(&CodexAppsServerInfoDiskCache {
        schema_version: CODEX_APPS_SERVER_INFO_CACHE_SCHEMA_VERSION,
        server_info: server_info.clone(),
    })
    .context("failed to serialize Codex Apps server info cache")?;
    write_codex_apps_cache_file(&cache_path, "server info", bytes)
}

fn write_codex_apps_cache_file(
    cache_path: &Path,
    cache_name: &str,
    bytes: Vec<u8>,
) -> anyhow::Result<()> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Codex Apps {cache_name} cache directory `{}`",
                parent.display()
            )
        })?;
    }
    std::fs::write(cache_path, bytes).with_context(|| {
        format!(
            "failed to write Codex Apps {cache_name} cache `{}`",
            cache_path.display()
        )
    })?;
    Ok(())
}

fn persist_codex_apps_cache(
    cache_context: &CodexAppsToolsCacheContext,
    server_info: &McpServerInfo,
    tools: &[ToolInfo],
) {
    let cache_write_start = Instant::now();
    let tools_result = write_cached_codex_apps_tools(cache_context, tools);
    if let Err(err) = &tools_result {
        tracing::warn!("failed to write Codex Apps tools cache: {err:#}");
    }
    let server_info_result = write_cached_codex_apps_server_info(cache_context, server_info);
    if let Err(err) = &server_info_result {
        tracing::warn!("failed to write Codex Apps server info cache: {err:#}");
    }
    let status = if tools_result.is_ok() && server_info_result.is_ok() {
        "success"
    } else {
        "failure"
    };
    emit_duration(
        MCP_TOOLS_CACHE_WRITE_DURATION_METRIC,
        cache_write_start.elapsed(),
        &[("status", status)],
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexAppsToolsDiskCache {
    schema_version: u8,
    tools: Vec<ToolInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexAppsServerInfoDiskCache {
    schema_version: u8,
    server_info: McpServerInfo,
}

const CODEX_APPS_TOOLS_CACHE_DIR: &str = "cache/codex_apps_tools";
pub(crate) const CODEX_APPS_TOOLS_CACHE_SCHEMA_VERSION: u8 = 4;

const CODEX_APPS_SERVER_INFO_CACHE_DIR: &str = "cache/codex_apps_server_info";
const CODEX_APPS_SERVER_INFO_CACHE_SCHEMA_VERSION: u8 = 1;

fn sha1_hex(s: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(s.as_bytes());
    let sha1 = hasher.finalize();
    format!("{sha1:x}")
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
