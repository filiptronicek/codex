use std::collections::HashSet;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;

use codex_core_skills::loader::ParsedSkillFrontmatter;
use codex_core_skills::loader::parse_skill_frontmatter_metadata;
use codex_exec_server::EnvironmentManager;
use codex_exec_server::ExecutorFileSystem;
use codex_protocol::capabilities::CapabilityRootLocation;
use codex_protocol::protocol::Product;
use codex_utils_path_uri::PathUri;

use crate::catalog::SkillAuthority;
use crate::catalog::SkillCatalog;
use crate::catalog::SkillCatalogEntry;
use crate::catalog::SkillPackageId;
use crate::catalog::SkillProviderError;
use crate::catalog::SkillReadResult;
use crate::catalog::SkillResourceId;
use crate::catalog::SkillSearchResult;
use crate::catalog::SkillSourceKind;
use crate::provider::SkillListQuery;
use crate::provider::SkillProvider;
use crate::provider::SkillProviderFuture;
use crate::provider::SkillReadRequest;
use crate::provider::SkillSearchRequest;

const SKILLS_FILENAME: &str = "SKILL.md";
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SKILLS_DIRS_PER_ROOT: usize = 2000;

/// Discovers and reads skills through the filesystem owned by an execution environment.
#[derive(Clone, Debug)]
pub struct ExecutorSkillProvider {
    environment_manager: Arc<EnvironmentManager>,
    restriction_product: Option<Product>,
}

impl ExecutorSkillProvider {
    pub fn new_with_restriction_product(
        environment_manager: Arc<EnvironmentManager>,
        restriction_product: Option<Product>,
    ) -> Self {
        Self {
            environment_manager,
            restriction_product,
        }
    }
}

impl SkillProvider for ExecutorSkillProvider {
    fn list(&self, query: SkillListQuery) -> SkillProviderFuture<'_, SkillCatalog> {
        Box::pin(async move {
            let mut catalog = SkillCatalog::default();
            for selected_root in query.executor_roots {
                let selected_root_id = selected_root.id;
                let CapabilityRootLocation::Environment {
                    environment_id,
                    path,
                } = selected_root.location;
                let authority =
                    SkillAuthority::new(SkillSourceKind::Executor, selected_root_id.clone());
                let Some(environment) = self.environment_manager.get_environment(&environment_id)
                else {
                    catalog.warnings.push(format!(
                        "Selected capability root `{selected_root_id}` references unavailable environment `{environment_id}`."
                    ));
                    continue;
                };
                let file_system = environment.get_filesystem();
                let outcome = load_executor_skills_from_root(file_system.as_ref(), &path).await;
                catalog.warnings.extend(outcome.warnings);
                for skill in outcome.skills {
                    catalog.push_entry(catalog_entry_from_skill(
                        &skill,
                        authority.clone(),
                        &selected_root_id,
                        &environment_id,
                    ));
                }
            }

            Ok(catalog)
        })
    }

    fn read(&self, request: SkillReadRequest) -> SkillProviderFuture<'_, SkillReadResult> {
        Box::pin(async move {
            if request.authority.kind != SkillSourceKind::Executor {
                return Err(SkillProviderError::new(format!(
                    "executor skill provider cannot read {} resources",
                    request.authority.kind
                )));
            }
            if request.package.0 != request.resource.as_str() {
                return Err(SkillProviderError::new(
                    "executor skill resource does not match its package",
                ));
            }
            let Some((environment_id, resource_path)) = request.resource.environment_path() else {
                return Err(SkillProviderError::new(
                    "executor skill resource is not bound to an environment",
                ));
            };
            let Some(environment) = self.environment_manager.get_environment(environment_id) else {
                return Err(SkillProviderError::new(format!(
                    "executor skill resource references unavailable environment `{environment_id}`"
                )));
            };
            let contents = environment
                .get_filesystem()
                .read_file_text(resource_path, /*sandbox*/ None)
                .await
                .map_err(|err| {
                    SkillProviderError::new(format!(
                        "failed to read executor skill resource {}: {err}",
                        request.resource.as_str()
                    ))
                })?;

            Ok(SkillReadResult {
                resource: request.resource,
                contents,
            })
        })
    }

    fn search(&self, _request: SkillSearchRequest) -> SkillProviderFuture<'_, SkillSearchResult> {
        Box::pin(async { Ok(SkillSearchResult::default()) })
    }
}

#[derive(Debug, Default)]
struct ExecutorSkillLoadOutcome {
    skills: Vec<ExecutorSkill>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct ExecutorSkill {
    path: PathUri,
    name: String,
    description: String,
    short_description: Option<String>,
}

async fn load_executor_skills_from_root(
    file_system: &dyn ExecutorFileSystem,
    root: &PathUri,
) -> ExecutorSkillLoadOutcome {
    let mut outcome = ExecutorSkillLoadOutcome::default();
    let root = canonicalize_for_skill_identity(file_system, root).await;
    match file_system.get_metadata(&root, /*sandbox*/ None).await {
        Ok(metadata) if metadata.is_directory => {}
        Ok(_) => return outcome,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return outcome,
        Err(err) => {
            outcome
                .warnings
                .push(format!("Failed to load executor skills at {root}: {err}"));
            return outcome;
        }
    }

    let mut visited_dirs: HashSet<PathUri> = HashSet::new();
    visited_dirs.insert(root.clone());
    let mut queue: VecDeque<(PathUri, usize)> = VecDeque::from([(root.clone(), 0)]);
    let mut truncated_by_dir_limit = false;

    while let Some((dir, depth)) = queue.pop_front() {
        let entries = match file_system.read_directory(&dir, /*sandbox*/ None).await {
            Ok(entries) => entries,
            Err(err) => {
                outcome
                    .warnings
                    .push(format!("Failed to read executor skills dir {dir}: {err}"));
                continue;
            }
        };

        for entry in entries {
            let file_name = entry.file_name;
            if file_name.starts_with('.') {
                continue;
            }
            let path = match dir.join(&file_name) {
                Ok(path) => path,
                Err(err) => {
                    outcome.warnings.push(format!(
                        "Failed to resolve executor skill path {dir}/{file_name}: {err}"
                    ));
                    continue;
                }
            };
            let metadata = match file_system.get_metadata(&path, /*sandbox*/ None).await {
                Ok(metadata) => metadata,
                Err(err) => {
                    outcome
                        .warnings
                        .push(format!("Failed to stat executor skill path {path}: {err}"));
                    continue;
                }
            };

            if metadata.is_symlink {
                match file_system.read_directory(&path, /*sandbox*/ None).await {
                    Ok(_) => {
                        enqueue_executor_skill_dir(
                            file_system,
                            &mut queue,
                            &mut visited_dirs,
                            &mut truncated_by_dir_limit,
                            path,
                            depth + 1,
                        )
                        .await;
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::NotADirectory | io::ErrorKind::NotFound
                        ) => {}
                    Err(err) => {
                        outcome
                            .warnings
                            .push(format!("Failed to read executor symlink dir {path}: {err}"));
                    }
                }
                continue;
            }

            if metadata.is_directory {
                enqueue_executor_skill_dir(
                    file_system,
                    &mut queue,
                    &mut visited_dirs,
                    &mut truncated_by_dir_limit,
                    path,
                    depth + 1,
                )
                .await;
                continue;
            }

            if metadata.is_file && file_name == SKILLS_FILENAME {
                match parse_executor_skill_file(file_system, &path).await {
                    Ok(skill) => outcome.skills.push(skill),
                    Err(message) => outcome.warnings.push(format!(
                        "Failed to load executor skill at {path}: {message}"
                    )),
                }
            }
        }
    }

    if truncated_by_dir_limit {
        tracing::warn!(
            "executor skills scan truncated after {} directories (root: {})",
            MAX_SKILLS_DIRS_PER_ROOT,
            root
        );
    }

    outcome
}

async fn enqueue_executor_skill_dir(
    file_system: &dyn ExecutorFileSystem,
    queue: &mut VecDeque<(PathUri, usize)>,
    visited_dirs: &mut HashSet<PathUri>,
    truncated_by_dir_limit: &mut bool,
    path: PathUri,
    depth: usize,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    if visited_dirs.len() >= MAX_SKILLS_DIRS_PER_ROOT {
        *truncated_by_dir_limit = true;
        return;
    }
    let path = canonicalize_for_skill_identity(file_system, &path).await;
    if visited_dirs.insert(path.clone()) {
        queue.push_back((path, depth));
    }
}

async fn parse_executor_skill_file(
    file_system: &dyn ExecutorFileSystem,
    path: &PathUri,
) -> Result<ExecutorSkill, String> {
    let contents = file_system
        .read_file_text(path, /*sandbox*/ None)
        .await
        .map_err(|err| format!("failed to read file: {err}"))?;
    let ParsedSkillFrontmatter {
        name,
        description,
        short_description,
    } = parse_skill_frontmatter_metadata(&contents, || default_skill_name(path))?;
    let path = canonicalize_for_skill_identity(file_system, path).await;

    Ok(ExecutorSkill {
        path,
        name,
        description,
        short_description,
    })
}

async fn canonicalize_for_skill_identity(
    file_system: &dyn ExecutorFileSystem,
    path: &PathUri,
) -> PathUri {
    file_system
        .canonicalize(path, /*sandbox*/ None)
        .await
        .unwrap_or_else(|_| path.clone())
}

fn default_skill_name(path: &PathUri) -> String {
    path.parent()
        .and_then(|parent| parent.basename())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "skill".to_string())
}

fn catalog_entry_from_skill(
    skill: &ExecutorSkill,
    authority: SkillAuthority,
    selected_root_id: &str,
    environment_id: &str,
) -> SkillCatalogEntry {
    let skill_path = skill.path.inferred_native_path_string();
    let normalized_path = skill_path.replace('\\', "/");
    let display_path = format!(
        "skill://{selected_root_id}/{}",
        normalized_path.trim_start_matches('/')
    );
    let entry = SkillCatalogEntry::new(
        SkillPackageId(display_path.clone()),
        authority,
        skill.name.clone(),
        skill.description.clone(),
        SkillResourceId::environment(display_path.clone(), environment_id, skill.path.clone()),
    )
    .with_short_description(skill.short_description.clone())
    .with_display_path(display_path)
    .with_dependencies(None);

    entry
}
