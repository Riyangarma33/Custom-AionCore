#[cfg(test)]
mod tests {
    use super::super::ast::{parse_tasks_document, update_task_status};
    use aionui_api_types::TaskStatus;

    #[test]
    fn parses_waves_hierarchy_and_references() {
        let markdown = r#"# Implementation Tasks

### Wave 1: Foundation
- [ ] 1. Core architecture setup (Refs: REQ-1.1, REQ-1.2)
  - Detail notes for architecture
  - Ensure compatibility
- [x] 1.1. Create data models
- [-] 1.2. Implement storage layer (Refs: REQ-1.3)

### Wave 2: Execution Engine
- [ ] 2. Task runner pipeline
- [/] 2.1. Concurrency guard
"#;

        let waves = parse_tasks_document(markdown);
        assert_eq!(waves.len(), 2);

        // Wave 1
        assert_eq!(waves[0].number, 1);
        assert_eq!(waves[0].title, "Wave 1: Foundation");
        assert_eq!(waves[0].tasks.len(), 3);

        assert_eq!(waves[0].tasks[0].id, "1");
        assert_eq!(waves[0].tasks[0].status, TaskStatus::Todo);
        assert_eq!(waves[0].tasks[0].refs, vec!["REQ-1.1", "REQ-1.2"]);
        assert!(waves[0].tasks[0].description.as_ref().unwrap().contains("Detail notes for architecture"));

        assert_eq!(waves[0].tasks[1].id, "1.1");
        assert_eq!(waves[0].tasks[1].parent_id.as_deref(), Some("1"));
        assert_eq!(waves[0].tasks[1].status, TaskStatus::Completed);

        assert_eq!(waves[0].tasks[2].id, "1.2");
        assert_eq!(waves[0].tasks[2].parent_id.as_deref(), Some("1"));
        assert_eq!(waves[0].tasks[2].status, TaskStatus::InProgress);

        // Wave 2
        assert_eq!(waves[1].number, 2);
        assert_eq!(waves[1].tasks.len(), 2);
        assert_eq!(waves[1].tasks[0].id, "2");
        assert_eq!(waves[1].tasks[1].id, "2.1");
        assert_eq!(waves[1].tasks[1].status, TaskStatus::InProgress);
    }

    #[test]
    fn round_trip_preserves_byte_identical_formatting_unix() {
        let input = "### Wave 1: Foundation\n- [ ] 1. First task   \n  - Note line with trailing space   \n- [ ] 1.1. Subtask\n- [x] 2. Second task\n";

        // Mutate task 1.1 from Todo to Completed
        let (updated, task) = update_task_status(input, "1.1", TaskStatus::Completed).expect("update succeeds");
        assert_eq!(task.status, TaskStatus::Completed);

        let expected = "### Wave 1: Foundation\n- [ ] 1. First task   \n  - Note line with trailing space   \n- [x] 1.1. Subtask\n- [x] 2. Second task\n";
        assert_eq!(updated, expected);

        // Mutate task 2 from Completed to InProgress
        let (updated2, task2) = update_task_status(&updated, "2", TaskStatus::InProgress).expect("update succeeds");
        assert_eq!(task2.status, TaskStatus::InProgress);

        let expected2 = "### Wave 1: Foundation\n- [ ] 1. First task   \n  - Note line with trailing space   \n- [x] 1.1. Subtask\n- [-] 2. Second task\n";
        assert_eq!(updated2, expected2);
    }

    #[test]
    fn round_trip_preserves_byte_identical_formatting_crlf_windows() {
        let input = "### Wave 1: Foundation\r\n- [ ] 1. Windows CRLF task\r\n- [ ] 2. Second task\r\n";

        let (updated, _) = update_task_status(input, "1", TaskStatus::Completed).expect("update succeeds");

        let expected = "### Wave 1: Foundation\r\n- [x] 1. Windows CRLF task\r\n- [ ] 2. Second task\r\n";
        assert_eq!(updated, expected);
        assert!(updated.contains("\r\n"));
    }

    #[test]
    fn ignores_checkboxes_inside_code_fences() {
        let markdown = r#"### Wave 1: Core
- [ ] 1. Implement checklist parser

```markdown
Here is an example:
- [ ] Not a real task
- [x] Also not a task
```

- [ ] 2. Next actual task
"#;

        let waves = parse_tasks_document(markdown);
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].tasks.len(), 2);
        assert_eq!(waves[0].tasks[0].id, "1");
        assert_eq!(waves[0].tasks[1].id, "2");
    }

    #[test]
    fn parses_non_standard_checkbox_markers() {
        let markdown = r#"### Wave 1: Edge Cases
- [-] 1. In progress with hyphen
- [/] 2. In progress with slash
- [X] 3. Completed with capital X
- [x] 4. Completed with lower x
- [ ] 5. Todo task
"#;

        let waves = parse_tasks_document(markdown);
        assert_eq!(waves[0].tasks[0].status, TaskStatus::InProgress);
        assert_eq!(waves[0].tasks[1].status, TaskStatus::InProgress);
        assert_eq!(waves[0].tasks[2].status, TaskStatus::Completed);
        assert_eq!(waves[0].tasks[3].status, TaskStatus::Completed);
        assert_eq!(waves[0].tasks[4].status, TaskStatus::Todo);
    }
}
