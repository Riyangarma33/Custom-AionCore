//! Integration test for Phase 3.9c & 3.9e: Conversation Lifecycle Workspace Skill Discovery & Slash Commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aionui_ai_agent::{AgentError, IWorkerTaskManager};
use aionui_api_types::{CreateConversationRequest, UpdateConversationRuntimeBindingsRequest};
use aionui_common::{AgentKillReason, TimestampMs};
use aionui_conversation::ConversationService;
use aionui_conversation::skill_resolver::ExtensionSkillResolver;
use aionui_db::{
    SqliteAcpSessionRepository, SqliteAgentMetadataRepository, SqliteConversationRepository, SqliteSkillRepository,
    init_database_memory,
};
use aionui_extension::SkillPaths;
use aionui_realtime::EventBroadcaster;
use serde_json::json;
use tempfile::TempDir;

struct TestBroadcaster;
impl EventBroadcaster for TestBroadcaster {
    fn broadcast(&self, _event: aionui_api_types::WebSocketMessage<serde_json::Value>) {}
}

struct NoopTaskManager;
#[async_trait::async_trait]
impl IWorkerTaskManager for NoopTaskManager {
    fn get_task(&self, _: &str) -> Option<aionui_ai_agent::AgentInstance> {
        None
    }
    async fn get_or_build_task(
        &self,
        _: &str,
        _: aionui_ai_agent::types::BuildTaskOptions,
    ) -> Result<aionui_ai_agent::AgentInstance, AgentError> {
        Err(AgentError::internal("noop"))
    }
    fn kill(&self, _: &str, _: Option<AgentKillReason>) -> Result<(), AgentError> {
        Ok(())
    }
    fn kill_and_wait(
        &self,
        _: &str,
        _: Option<AgentKillReason>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(std::future::ready(()))
    }
    async fn clear(&self) {}
    fn active_count(&self) -> usize {
        0
    }
    fn collect_idle(&self, _: TimestampMs) -> Vec<String> {
        vec![]
    }
}

fn make_paths(base: &Path) -> Arc<SkillPaths> {
    Arc::new(SkillPaths {
        data_dir: base.to_path_buf(),
        user_skills_dir: base.join("skills"),
        cron_skills_dir: base.join("cron").join("skills"),
        builtin_skills_dir: base.join("builtin-skills"),
        builtin_rules_dir: base.join("builtin-rules"),
        assistant_rules_dir: base.join("assistant-rules"),
        assistant_skills_dir: base.join("assistant-skills"),
    })
}

#[tokio::test]
async fn test_conversation_creation_discovers_workspace_skills_without_auto_enabling() {
    let tmp = TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    let skill_dir = ws.join(".claude").join("skills").join("ops-aws");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ops-aws\ndescription: AWS Infrastructure Guardian\n---\n# AWS Ops\n\nManage AWS.",
    )
    .unwrap();

    let db = init_database_memory().await.unwrap();
    let repo = Arc::new(SqliteConversationRepository::new(db.pool().clone()));
    let skill_repo = Arc::new(SqliteSkillRepository::new(db.pool().clone()));
    let paths = make_paths(tmp.path());
    let skill_resolver = Arc::new(ExtensionSkillResolver::new(paths, skill_repo));

    let broadcaster = Arc::new(TestBroadcaster);
    let agent_metadata_repo = Arc::new(SqliteAgentMetadataRepository::new(db.pool().clone()));
    let acp_session_repo = Arc::new(SqliteAcpSessionRepository::new(db.pool().clone()));
    let task_mgr: Arc<dyn IWorkerTaskManager> = Arc::new(NoopTaskManager);

    let svc = ConversationService::new(
        tmp.path().to_path_buf(),
        broadcaster,
        skill_resolver,
        task_mgr.clone(),
        repo,
        agent_metadata_repo,
        acp_session_repo,
    );

    // 1. Create conversation with workspace containing ops-aws
    let req: CreateConversationRequest = serde_json::from_value(json!({
        "type": "acp",
        "name": "Ops Conv",
        "extra": {
            "workspace": ws.to_str().unwrap(),
            "backend": "claude",
        }
    }))
    .unwrap();

    let created = svc.create("user_test", req).await.unwrap();

    // Invariant: extra.skills remains empty of workspace skills!
    let skills = created.extra["skills"].as_array().expect("skills array");
    assert!(
        skills.is_empty() || !skills.iter().any(|s| s.as_str() == Some("ops-aws")),
        "ops-aws must NOT be auto-enabled in extra.skills on creation"
    );

    // Discovered workspace skills are surfaced under workspace_skills / suggested_skills
    let ws_skills = created.extra["workspace_skills"].as_array().expect("workspace_skills array");
    assert_eq!(ws_skills.len(), 1);
    assert_eq!(ws_skills[0]["name"], "ops-aws");
    assert_eq!(ws_skills[0]["description"], "AWS Infrastructure Guardian");

    // 2. Disabled skill does NOT appear in slash commands
    let slash_cmds = svc.get_slash_commands("user_test", &created.id).await.unwrap();
    assert!(
        !slash_cmds.iter().any(|c| c.command == "ops-aws"),
        "disabled workspace skill must not appear in slash commands"
    );

    // 3. User explicitly enables ops-aws via Phase 2A update_runtime_bindings
    let update_req = UpdateConversationRuntimeBindingsRequest {
        mcp_server_ids: None,
        skills: Some(vec!["ops-aws".to_string()]),
    };
    let update_resp = svc
        .update_runtime_bindings("user_test", &created.id, update_req, &task_mgr)
        .await
        .unwrap();

    assert_eq!(update_resp.skills, vec!["ops-aws"]);
    assert_eq!(update_resp.conversation.extra["skills"], json!(["ops-aws"]));

    // 4. Now that ops-aws is enabled, it surfaces in slash commands with its frontmatter description!
    let slash_cmds_after = svc.get_slash_commands("user_test", &created.id).await.unwrap();
    let aws_cmd = slash_cmds_after
        .iter()
        .find(|c| c.command == "ops-aws")
        .expect("ops-aws slash command must be present once enabled");
    assert_eq!(aws_cmd.description, "AWS Infrastructure Guardian");
}
