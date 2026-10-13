#[cfg(test)]
mod tests {
    use super::super::scanner::scan_workspace_specs;
    use super::super::service::SpecService;
    use super::super::types::SpecError;
    use aionui_api_types::{CreateSpecRequest, SpecLocationType, SpecPhase, SpecType, TaskStatus};
    use tempfile::tempdir;

    #[test]
    fn test_specs_lifecycle_and_scaffolding() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        // 1. Create a new spec
        let detail = SpecService::create_spec(
            ws,
            CreateSpecRequest {
                title: "User Profile Avatar".to_string(),
                spec_type: Some(SpecType::Feature),
                workflow: None,
                slug: Some("user-profile-avatar".to_string()),
            },
        )
        .expect("create spec succeeds");

        assert_eq!(detail.summary.id, "user-profile-avatar");
        assert_eq!(detail.summary.spec_type, SpecType::Feature);
        assert_eq!(detail.summary.current_phase, SpecPhase::Requirements);
        assert_eq!(detail.summary.location_type, SpecLocationType::Aion);
        assert!(detail.requirements_md.is_some());
        assert!(detail.design_md.is_some());
        assert!(detail.tasks_md.is_some());
        assert_eq!(detail.waves.len(), 3);

        // 2. Scan workspace specs
        let summaries = scan_workspace_specs(ws).expect("scan succeeds");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, "user-profile-avatar");

        // 3. Update requirements file
        let new_req = "# Updated Requirements\n- [REQ-1] Ubiquitous: System must support avatar uploads.\n";
        let updated = SpecService::update_spec_file(
            ws,
            "user-profile-avatar",
            "requirements.md",
            new_req,
            Some(detail.mtime),
        )
        .expect("update file succeeds");

        assert_eq!(updated.requirements_md.as_deref(), Some(new_req));

        // 4. Toggle a task status
        let patched = SpecService::patch_task(
            ws,
            "user-profile-avatar",
            "1",
            TaskStatus::Completed,
            None,
        )
        .expect("patch task succeeds");

        assert_eq!(patched.summary.completed_tasks, 1);
        let task_1 = patched.waves[0].tasks.iter().find(|t| t.id == "1").unwrap();
        assert_eq!(task_1.status, TaskStatus::Completed);

        // 5. Transition phase
        let transitioned = SpecService::transition_phase(
            ws,
            "user-profile-avatar",
            SpecPhase::Design,
        )
        .expect("transition phase succeeds");
        assert_eq!(transitioned.summary.current_phase, SpecPhase::Design);

        // 6. Link conversation
        let linked = SpecService::link_conversation(
            ws,
            "user-profile-avatar",
            Some("conv-123".to_string()),
        )
        .expect("link conversation succeeds");
        assert_eq!(linked.summary.linked_conversation_id.as_deref(), Some("conv-123"));

        // 7. Concurrency conflict check
        // Simulating a stale write where disk mtime is greater than expected mtime
        let stale_mtime = 100i64; // Stale timestamp from the past
        let err = SpecService::update_spec_file(
            ws,
            "user-profile-avatar",
            "requirements.md",
            "# Stale write",
            Some(stale_mtime),
        );

        match err {
            Err(SpecError::ConcurrentModification { .. }) => {
                // Expected conflict error!
            }
            other => panic!("expected ConcurrentModification, got {:?}", other),
        }
    }

    #[test]
    fn test_multi_tool_discovery_kiro_and_openspec() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        // Create a Kiro spec in .kiro/specs/kiro-spec
        let kiro_dir = ws.join(".kiro").join("specs").join("kiro-spec");
        std::fs::create_dir_all(&kiro_dir).unwrap();
        std::fs::write(
            kiro_dir.join("tasks.md"),
            "### Wave 1\n- [ ] 1. Kiro task\n- [x] 2. Done task\n",
        )
        .unwrap();

        // Create an OpenSpec in .specs/open-spec
        let open_dir = ws.join(".specs").join("open-spec");
        std::fs::create_dir_all(&open_dir).unwrap();
        std::fs::write(
            open_dir.join("tasks.md"),
            "### Wave 1\n- [ ] 1. OpenSpec task\n",
        )
        .unwrap();

        let summaries = scan_workspace_specs(ws).expect("scan succeeds");
        assert_eq!(summaries.len(), 2);

        let kiro_sum = summaries.iter().find(|s| s.id == "kiro-spec").unwrap();
        assert_eq!(kiro_sum.location_type, SpecLocationType::Kiro);
        assert_eq!(kiro_sum.total_tasks, 2);
        assert_eq!(kiro_sum.completed_tasks, 1);

        let open_sum = summaries.iter().find(|s| s.id == "open-spec").unwrap();
        assert_eq!(open_sum.location_type, SpecLocationType::OpenSpec);
        assert_eq!(open_sum.total_tasks, 1);
        assert_eq!(open_sum.completed_tasks, 0);
    }
}
