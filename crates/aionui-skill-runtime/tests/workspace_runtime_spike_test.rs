//! Pre-flight Spike and Workspace Skill Runtime Integration Tests (Phase 3.9d).
//!
//! Confirms:
//! 1. Byte-for-byte equality between `SkillRuntimeService::show` output and
//!    `[LOAD_SKILL]` body resolution for a workspace skill.
//! 2. Runtime resolution against the conversation's workspace for enabled skills only.
//! 3. Rejection of discovered-but-disabled workspace skills as `skill_not_enabled`.

mod common;

use std::fs;
use std::path::Path;

use aionui_extension::skill_service::extract_skill_body;
use axum::http::StatusCode;
use common::TestHarness;
use tempfile::TempDir;

#[tokio::test]
async fn preflight_spike_runtime_cli_and_load_skill_bodies_match() {
    let h = TestHarness::new().await;

    // Use live ms-ops fixture if available, otherwise synthetic fixture
    let live_fixture = Path::new("/home/aionui/Projects/ICS/ms-ops/.claude/skills/ops-aws");
    let tmp = TempDir::new().unwrap();
    let (ws_dir, skill_name) = if live_fixture.exists() {
        (Path::new("/home/aionui/Projects/ICS/ms-ops"), "ops-aws")
    } else {
        let ws = tmp.path().join("workspace");
        let skill_dir = ws.join(".claude").join("skills").join("spike-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let content = "---\nname: spike-skill\ndescription: Spike test skill\n---\n# Spike Body\n\nExecute safely.";
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        (ws.as_path(), "spike-skill")
    };

    let conv = h
        .create_conversation_with_workspace("user_spike", &[skill_name], Some(ws_dir.to_str().unwrap()))
        .await;

    // 1. Invoke SkillRuntimeService::show via CLI route
    let cli_resp = h
        .get_json("user_spike", &conv, &format!("/api/runtime/skills/{skill_name}"))
        .await;
    let cli_body = cli_resp["data"]["body"].as_str().expect("body in response");
    let cli_path = cli_resp["data"]["path"].as_str().expect("path in response");

    // 2. Invoke the body extraction used by [LOAD_SKILL] (load_resolved_skill_bodies)
    let raw_file = Path::new(cli_path).join("SKILL.md");
    let raw_content = fs::read_to_string(&raw_file).expect("read SKILL.md");
    let load_skill_body = extract_skill_body(&raw_content);

    // 3. Diff: assert byte-identical output between the two delivery channels
    assert_eq!(
        cli_body, load_skill_body,
        "CLI 'skills show' body must match '[LOAD_SKILL]' body byte-for-byte"
    );
}

#[tokio::test]
async fn runtime_lists_and_reads_enabled_workspace_skills() {
    let h = TestHarness::new().await;
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("my-ws");
    let skill_dir = ws.join(".claude").join("skills").join("ops-deploy");
    fs::create_dir_all(skill_dir.join("references")).unwrap();

    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ops-deploy\ndescription: Deployment operations\n---\n# Deploy\n\nRun deploy.",
    )
    .unwrap();
    fs::write(
        skill_dir.join("references").join("checklist.md"),
        "# Pre-flight checklist",
    )
    .unwrap();

    // Skill ops-deploy is enabled; skill ops-other exists in workspace but is NOT enabled
    let other_dir = ws.join(".claude").join("skills").join("ops-other");
    fs::create_dir_all(&other_dir).unwrap();
    fs::write(
        other_dir.join("SKILL.md"),
        "---\nname: ops-other\ndescription: Other operations\n---\nOther body",
    )
    .unwrap();

    let conv = h
        .create_conversation_with_workspace("user_ws", &["ops-deploy"], Some(ws.to_str().unwrap()))
        .await;

    // 1. list shows enabled workspace skill with description
    let list_resp = h.get_json("user_ws", &conv, "/api/runtime/skills").await;
    let skills = list_resp["data"]["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "ops-deploy");
    assert_eq!(skills[0]["description"], "Deployment operations");

    // 2. show returns body and absolute path
    let show_resp = h.get_json("user_ws", &conv, "/api/runtime/skills/ops-deploy").await;
    assert_eq!(show_resp["data"]["name"], "ops-deploy");
    assert_eq!(show_resp["data"]["body"], "# Deploy\n\nRun deploy.");
    assert_eq!(
        show_resp["data"]["path"],
        skill_dir.canonicalize().unwrap().to_str().unwrap()
    );

    // 3. cat reads supplementary file relative to workspace skill root
    let cat_resp = h
        .get_json(
            "user_ws",
            &conv,
            "/api/runtime/skills/ops-deploy/file?path=references/checklist.md",
        )
        .await;
    assert_eq!(cat_resp["data"]["content"], "# Pre-flight checklist");

    // 4. disabled workspace skill is refused with 403 / skill_not_enabled
    let (status, err_body) = h.get_raw("user_ws", &conv, "/api/runtime/skills/ops-other").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err_body["error"]["code"], "skill_not_enabled");
}
