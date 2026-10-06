//! Unit and integration tests for Phase 3.9 Workspace Skills Discovery.

use std::fs;
use std::path::{Path, PathBuf};

use aionui_db::{ISkillRepository, SqliteSkillRepository, UpsertSkillParams, init_database_memory};
use aionui_extension::skill_service::{
    SkillPaths, materialize_skills_for_agent_with_repo_for_user, scan_workspace_skills,
    scan_workspace_skills_bounded,
};
use tempfile::TempDir;

const SKILL_MD: &str = "SKILL.md";

fn make_paths(base: &Path) -> SkillPaths {
    SkillPaths {
        data_dir: base.to_path_buf(),
        user_skills_dir: base.join("skills"),
        cron_skills_dir: base.join("cron").join("skills"),
        builtin_skills_dir: base.join("builtin-skills"),
        builtin_rules_dir: base.join("builtin-rules"),
        assistant_rules_dir: base.join("assistant-rules"),
        assistant_skills_dir: base.join("assistant-skills"),
    }
}

fn write_skill_file(skill_dir: &Path, name: Option<&str>, description: &str, body: &str) {
    fs::create_dir_all(skill_dir).unwrap();
    let frontmatter = match name {
        Some(n) => format!("---\nname: {n}\ndescription: \"{description}\"\n---\n{body}"),
        None => format!("---\ndescription: \"{description}\"\n---\n{body}"),
    };
    fs::write(skill_dir.join(SKILL_MD), frontmatter).unwrap();
}

#[tokio::test]
async fn test_scan_workspace_skills_detects_candidate_directories() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("my-project");
    fs::create_dir_all(&ws).unwrap();

    // 1. .claude/skills/ops-aws
    write_skill_file(
        &ws.join(".claude").join("skills").join("ops-aws"),
        Some("ops-aws"),
        "AWS operations",
        "aws body",
    );

    // 2. .agents/skills/ops-gcp
    write_skill_file(
        &ws.join(".agents").join("skills").join("ops-gcp"),
        Some("ops-gcp"),
        "GCP operations",
        "gcp body",
    );

    // 3. .aionrs/skills/ops-security
    write_skill_file(
        &ws.join(".aionrs").join("skills").join("ops-security"),
        Some("ops-security"),
        "Security operations",
        "security body",
    );

    // 4. .aion/skills/ops-k8s
    write_skill_file(
        &ws.join(".aion").join("skills").join("ops-k8s"),
        Some("ops-k8s"),
        "Kubernetes operations",
        "k8s body",
    );

    let skills = scan_workspace_skills(&ws).await;

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["ops-aws", "ops-gcp", "ops-k8s", "ops-security"]);
}

#[tokio::test]
async fn test_ascending_search_stops_at_workspace_boundary_without_git() {
    let tmp = TempDir::new().unwrap();
    // Outside boundary: contains a trap skill that must NEVER be discovered
    let outside = tmp.path();
    write_skill_file(
        &outside.join(".claude").join("skills").join("trap-skill"),
        Some("trap-skill"),
        "Should not be seen",
        "body",
    );

    // Registered boundary
    let boundary = outside.join("tenant-boundary");
    let nested_sub = boundary.join("packages").join("backend");
    fs::create_dir_all(&nested_sub).unwrap();

    // Skill inside boundary
    write_skill_file(
        &boundary.join(".claude").join("skills").join("valid-skill"),
        Some("valid-skill"),
        "Valid boundary skill",
        "body",
    );

    // Ascending scan from deep subpath with registered boundary
    let skills = scan_workspace_skills_bounded(&nested_sub, &boundary).await;

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["valid-skill"]);
    assert!(!names.contains(&"trap-skill"));
}

#[tokio::test]
async fn test_ascending_search_stops_at_git_root_within_boundary() {
    let tmp = TempDir::new().unwrap();
    let boundary = tmp.path().join("boundary");

    // Repo root with .git inside boundary
    let repo = boundary.join("my-repo");
    fs::create_dir_all(repo.join(".git")).unwrap();

    // Skill above the git root but inside boundary (e.g. boundary root)
    write_skill_file(
        &boundary.join(".claude").join("skills").join("boundary-skill"),
        Some("boundary-skill"),
        "Boundary skill",
        "body",
    );

    // Skill inside repo
    write_skill_file(
        &repo.join(".claude").join("skills").join("repo-skill"),
        Some("repo-skill"),
        "Repo skill",
        "body",
    );

    let deep_dir = repo.join("src").join("handlers");
    fs::create_dir_all(&deep_dir).unwrap();

    let skills = scan_workspace_skills_bounded(&deep_dir, &boundary).await;

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    // Stops at git root (repo), so repo-skill is detected, but search does not climb beyond repo
    assert_eq!(names, vec!["repo-skill"]);
}

#[tokio::test]
async fn test_directory_type_over_proximity_precedence() {
    let tmp = TempDir::new().unwrap();
    let boundary = tmp.path().join("boundary");
    let deep_dir = boundary.join("subproject");
    fs::create_dir_all(&deep_dir).unwrap();

    // At boundary (shallow / ancestor): .claude/skills/common-skill
    write_skill_file(
        &boundary.join(".claude").join("skills").join("common-skill"),
        Some("common-skill"),
        "Boundary Claude Skill",
        "claude-boundary-body",
    );

    // At deep_dir (nested / child): .agents/skills/common-skill
    write_skill_file(
        &deep_dir.join(".agents").join("skills").join("common-skill"),
        Some("common-skill"),
        "Child Agents Skill",
        "agents-child-body",
    );

    let skills = scan_workspace_skills_bounded(&deep_dir, &boundary).await;

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "common-skill");
    // Directory-type precedence (.claude > .agents) wins over proximity!
    assert_eq!(skills[0].description, "Boundary Claude Skill");
}

#[tokio::test]
async fn test_proximity_precedence_within_same_directory_type() {
    let tmp = TempDir::new().unwrap();
    let boundary = tmp.path().join("boundary");
    let deep_dir = boundary.join("subproject");
    fs::create_dir_all(&deep_dir).unwrap();

    // At boundary (ancestor): .claude/skills/my-skill
    write_skill_file(
        &boundary.join(".claude").join("skills").join("my-skill"),
        Some("my-skill"),
        "Ancestor Claude Skill",
        "ancestor-body",
    );

    // At deep_dir (closer): .claude/skills/my-skill
    write_skill_file(
        &deep_dir.join(".claude").join("skills").join("my-skill"),
        Some("my-skill"),
        "Child Claude Skill",
        "child-body",
    );

    let skills = scan_workspace_skills_bounded(&deep_dir, &boundary).await;

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-skill");
    // Within the SAME directory type, closer depth (child) wins!
    assert_eq!(skills[0].description, "Child Claude Skill");
}

#[tokio::test]
async fn test_symlink_escaping_boundary_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();

    write_skill_file(
        &outside.join("secret-skill"),
        Some("secret-skill"),
        "Secret external skill",
        "secret",
    );

    let boundary = tmp.path().join("boundary");
    let claude_skills = boundary.join(".claude").join("skills");
    fs::create_dir_all(&claude_skills).unwrap();

    // Symlink escaping boundary
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.join("secret-skill"), claude_skills.join("secret-skill")).unwrap();

    // Legitimate skill
    write_skill_file(
        &claude_skills.join("legit-skill"),
        Some("legit-skill"),
        "Legit skill",
        "legit",
    );

    let skills = scan_workspace_skills(&boundary).await;

    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["legit-skill"]);
    assert!(!names.contains(&"secret-skill"));
}

#[tokio::test]
async fn test_frontmatter_fallback_to_directory_name() {
    let tmp = TempDir::new().unwrap();
    let boundary = tmp.path().join("boundary");
    let skill_dir = boundary.join(".claude").join("skills").join("fallback-name");

    // Frontmatter without `name:` field
    write_skill_file(&skill_dir, None, "Description only", "body");

    let skills = scan_workspace_skills(&boundary).await;

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "fallback-name");
    assert_eq!(skills[0].description, "Description only");
}

#[tokio::test]
async fn test_workspace_skill_resolution_precedence_and_shadowing() {
    let tmp = TempDir::new().unwrap();
    let paths = make_paths(tmp.path());

    // 1. Built-in skill: "ops-aws"
    write_skill_file(
        &paths.builtin_skills_dir.join("ops-aws"),
        Some("ops-aws"),
        "Built-in AWS ops",
        "builtin-body",
    );

    // 2. User DB skill: "ops-aws"
    let db = init_database_memory().await.unwrap();
    let repo = SqliteSkillRepository::new(db.pool().clone());
    let user_skill_dir = tmp.path().join("user-skills").join("ops-aws");
    write_skill_file(&user_skill_dir, Some("ops-aws"), "User AWS ops", "user-body");
    repo.upsert_for_user(
        "test-user",
        UpsertSkillParams {
            name: "ops-aws",
            description: Some("User AWS ops"),
            path: user_skill_dir.to_string_lossy().as_ref(),
            source: "user",
            enabled: true,
        },
    )
    .await
    .unwrap();

    // 3. Workspace project skill: "ops-aws"
    let ws = tmp.path().join("workspace");
    let ws_skill_dir = ws.join(".claude").join("skills").join("ops-aws");
    write_skill_file(
        &ws_skill_dir,
        Some("ops-aws"),
        "Workspace project AWS ops",
        "workspace-body",
    );

    // Materialize with workspace: workspace project skill must win!
    let resolved = materialize_skills_for_agent_with_repo_for_user(
        &paths,
        &repo,
        "test-user",
        "conv-1",
        &["ops-aws".to_string()],
        Some(&ws),
    )
    .await
    .unwrap();

    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "ops-aws");
    assert_eq!(resolved[0].source_path, ws_skill_dir);

    // Materialize without workspace: user DB skill wins over built-in!
    let resolved_no_ws = materialize_skills_for_agent_with_repo_for_user(
        &paths,
        &repo,
        "test-user",
        "conv-1",
        &["ops-aws".to_string()],
        None,
    )
    .await
    .unwrap();

    assert_eq!(resolved_no_ws.len(), 1);
    assert_eq!(resolved_no_ws[0].name, "ops-aws");
    assert_eq!(resolved_no_ws[0].source_path, user_skill_dir);
}
