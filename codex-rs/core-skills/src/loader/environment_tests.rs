use std::fs;

use codex_exec_server::LOCAL_FS;
use codex_protocol::protocol::Product;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::model::SkillDependencies;
use crate::model::SkillPolicy;
use crate::model::SkillToolDependency;

use super::load_environment_skills_from_root;

#[tokio::test]
async fn loads_plugin_namespace_dependencies_and_policy() {
    let root = tempdir().expect("tempdir");
    let skill_dir = root.path().join("skills/deploy");
    fs::create_dir_all(root.path().join(".codex-plugin")).expect("manifest dir");
    fs::create_dir_all(skill_dir.join("agents")).expect("metadata dir");
    fs::write(
        root.path().join(".codex-plugin/plugin.json"),
        r#"{"name":"demo-plugin"}"#,
    )
    .expect("manifest");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy the service.\n---\n",
    )
    .expect("skill");
    fs::write(
        skill_dir.join("agents/openai.yaml"),
        r#"
dependencies:
  tools:
    - type: mcp
      value: deploy-server
      description: Deploy MCP
policy:
  allow_implicit_invocation: false
  products: [codex]
"#,
    )
    .expect("metadata");

    let root_uri = PathUri::from_path(root.path()).expect("root URI");
    let outcome =
        load_environment_skills_from_root(LOCAL_FS.as_ref(), &root_uri, Some(Product::Codex)).await;

    assert_eq!(outcome.warnings, Vec::<String>::new());
    assert_eq!(outcome.skills.len(), 1);
    let skill = &outcome.skills[0];
    assert_eq!(skill.name, "demo-plugin:deploy");
    assert_eq!(skill.description, "Deploy the service.");
    assert_eq!(skill.short_description, None);
    let canonical_skill_dir = skill_dir.canonicalize().expect("canonical skill dir");
    assert_eq!(
        skill.package_root,
        PathUri::from_path(&canonical_skill_dir).unwrap()
    );
    assert_eq!(
        skill.path_to_skills_md,
        PathUri::from_path(canonical_skill_dir.join("SKILL.md")).unwrap()
    );
    assert_eq!(
        skill.dependencies,
        Some(SkillDependencies {
            tools: vec![SkillToolDependency {
                r#type: "mcp".to_string(),
                value: "deploy-server".to_string(),
                description: Some("Deploy MCP".to_string()),
                transport: None,
                command: None,
                url: None,
            }],
        })
    );
    assert_eq!(
        skill.policy,
        Some(SkillPolicy {
            allow_implicit_invocation: Some(false),
            products: vec![Product::Codex],
        })
    );
    assert!(!skill.allows_implicit_invocation());
}

#[tokio::test]
async fn filters_environment_skills_by_product_policy() {
    let root = tempdir().expect("tempdir");
    let skill_dir = root.path().join("skills/deploy");
    fs::create_dir_all(skill_dir.join("agents")).expect("metadata dir");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy the service.\n---\n",
    )
    .expect("skill");
    fs::write(
        skill_dir.join("agents/openai.yaml"),
        "policy:\n  products: [chatgpt]\n",
    )
    .expect("metadata");

    let outcome = load_environment_skills_from_root(
        LOCAL_FS.as_ref(),
        &PathUri::from_path(root.path()).expect("root URI"),
        Some(Product::Codex),
    )
    .await;

    assert!(outcome.skills.is_empty());
}
