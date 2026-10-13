use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use aionui_api_types::{
    CreateSpecRequest, SpecDetail, SpecLocationType, SpecPhase, SpecType, SpecWorkflow,
    TaskStatus, WorkspaceSpecSummary,
};
use crate::specs::ast::{parse_tasks_document, update_task_status};
use crate::specs::scanner::{scan_workspace_specs, RawSpecJson};
use crate::specs::types::SpecError;

/// High-level operations for Spec-Driven Development (SDD) artifacts.
pub struct SpecService;

impl SpecService {
    /// Enumerate all specifications within the project workspace.
    pub fn list_specs(workspace: &Path) -> Result<Vec<WorkspaceSpecSummary>, SpecError> {
        scan_workspace_specs(workspace)
    }

    /// Retrieve full specification detail, including constituent markdown and parsed tasks.
    pub fn get_spec(workspace: &Path, spec_id: &str) -> Result<SpecDetail, SpecError> {
        let (spec_dir, location_type) = find_spec_dir(workspace, spec_id)?;
        read_spec_detail(&spec_dir, spec_id, location_type)
    }

    /// Create and scaffold a new specification in `<workspace>/.aion/specs/<slug>/`.
    pub fn create_spec(workspace: &Path, req: CreateSpecRequest) -> Result<SpecDetail, SpecError> {
        let slug = req
            .slug
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| slugify(&req.title));

        let spec_dir = workspace.join(".aion").join("specs").join(&slug);
        if spec_dir.exists() {
            return Err(SpecError::SpecAlreadyExists { id: slug });
        }

        fs::create_dir_all(&spec_dir).map_err(|e| (spec_dir.clone(), e))?;

        let spec_type = req.spec_type.unwrap_or(SpecType::Feature);
        let workflow = req.workflow.unwrap_or(SpecWorkflow::RequirementsFirst);
        let now_ms = chrono::Utc::now().timestamp_millis();

        let meta = RawSpecJson {
            id: Some(slug.clone()),
            title: Some(req.title.clone()),
            spec_type: Some(spec_type),
            workflow: Some(workflow),
            current_phase: Some(SpecPhase::Requirements),
            active_wave: Some(1),
            created_at: Some(now_ms),
            updated_at: Some(now_ms),
            linked_conversation_id: None,
        };

        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| SpecError::Serialization(e.to_string()))?;
        fs::write(spec_dir.join("spec.json"), meta_json)
            .map_err(|e| (spec_dir.join("spec.json"), e))?;

        // Requirements / Bugfix template
        let (req_filename, req_template) = match spec_type {
            SpecType::Feature => ("requirements.md", feature_requirements_template(&req.title)),
            SpecType::Bugfix => ("bugfix.md", bugfix_requirements_template(&req.title)),
        };
        fs::write(spec_dir.join(req_filename), req_template)
            .map_err(|e| (spec_dir.join(req_filename), e))?;

        // Design template
        let design_template = design_template(&req.title);
        fs::write(spec_dir.join("design.md"), design_template)
            .map_err(|e| (spec_dir.join("design.md"), e))?;

        // Tasks checklist template
        let tasks_template = tasks_template(&req.title);
        fs::write(spec_dir.join("tasks.md"), tasks_template)
            .map_err(|e| (spec_dir.join("tasks.md"), e))?;

        read_spec_detail(&spec_dir, &slug, SpecLocationType::Aion)
    }

    /// Update a constituent markdown document with an optimistic concurrency check.
    pub fn update_spec_file(
        workspace: &Path,
        spec_id: &str,
        filename: &str,
        content: &str,
        expected_mtime: Option<i64>,
    ) -> Result<SpecDetail, SpecError> {
        validate_filename(filename)?;
        let (spec_dir, location_type) = find_spec_dir(workspace, spec_id)?;
        let file_path = spec_dir.join(filename);

        if let Some(expected) = expected_mtime {
            if file_path.exists() {
                let disk_mtime = get_file_mtime(&file_path);
                // Allow a small 50ms tolerance for filesystem timestamp resolution
                if disk_mtime > expected + 50 {
                    return Err(SpecError::ConcurrentModification {
                        file: filename.to_string(),
                        disk_mtime,
                        expected_mtime: expected,
                    });
                }
            }
        }

        fs::write(&file_path, content).map_err(|e| (file_path.clone(), e))?;
        touch_spec_json(&spec_dir)?;

        read_spec_detail(&spec_dir, spec_id, location_type)
    }

    /// In-place toggle of a task checkbox status, preserving 100% byte formatting.
    pub fn patch_task(
        workspace: &Path,
        spec_id: &str,
        task_id: &str,
        new_status: TaskStatus,
        expected_mtime: Option<i64>,
    ) -> Result<SpecDetail, SpecError> {
        let (spec_dir, location_type) = find_spec_dir(workspace, spec_id)?;
        let tasks_path = spec_dir.join("tasks.md");
        if !tasks_path.exists() {
            return Err(SpecError::TaskNotFound {
                id: task_id.to_string(),
            });
        }

        if let Some(expected) = expected_mtime {
            let disk_mtime = get_file_mtime(&tasks_path);
            if disk_mtime > expected + 50 {
                return Err(SpecError::ConcurrentModification {
                    file: "tasks.md".to_string(),
                    disk_mtime,
                    expected_mtime: expected,
                });
            }
        }

        let raw_content = fs::read_to_string(&tasks_path).map_err(|e| (tasks_path.clone(), e))?;
        let (updated_content, _) = update_task_status(&raw_content, task_id, new_status)?;

        fs::write(&tasks_path, updated_content).map_err(|e| (tasks_path.clone(), e))?;
        touch_spec_json(&spec_dir)?;

        read_spec_detail(&spec_dir, spec_id, location_type)
    }

    /// Transition current phase in `spec.json`.
    pub fn transition_phase(
        workspace: &Path,
        spec_id: &str,
        phase: SpecPhase,
    ) -> Result<SpecDetail, SpecError> {
        let (spec_dir, location_type) = find_spec_dir(workspace, spec_id)?;
        let spec_json_path = spec_dir.join("spec.json");

        let mut meta: RawSpecJson = if spec_json_path.exists() {
            let text = fs::read_to_string(&spec_json_path).map_err(|e| (spec_json_path.clone(), e))?;
            serde_json::from_str(&text).unwrap_or(RawSpecJson {
                id: Some(spec_id.to_string()),
                title: None,
                spec_type: None,
                workflow: None,
                current_phase: None,
                active_wave: None,
                created_at: None,
                updated_at: None,
                linked_conversation_id: None,
            })
        } else {
            RawSpecJson {
                id: Some(spec_id.to_string()),
                title: None,
                spec_type: None,
                workflow: None,
                current_phase: None,
                active_wave: None,
                created_at: None,
                updated_at: None,
                linked_conversation_id: None,
            }
        };

        meta.current_phase = Some(phase);
        meta.updated_at = Some(chrono::Utc::now().timestamp_millis());

        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| SpecError::Serialization(e.to_string()))?;
        fs::write(&spec_json_path, meta_json).map_err(|e| (spec_json_path.clone(), e))?;

        read_spec_detail(&spec_dir, spec_id, location_type)
    }

    /// Link or unlink a conversation in `spec.json`.
    pub fn link_conversation(
        workspace: &Path,
        spec_id: &str,
        conversation_id: Option<String>,
    ) -> Result<SpecDetail, SpecError> {
        let (spec_dir, location_type) = find_spec_dir(workspace, spec_id)?;
        let spec_json_path = spec_dir.join("spec.json");

        let mut meta: RawSpecJson = if spec_json_path.exists() {
            let text = fs::read_to_string(&spec_json_path).map_err(|e| (spec_json_path.clone(), e))?;
            serde_json::from_str(&text).unwrap_or(RawSpecJson {
                id: Some(spec_id.to_string()),
                title: None,
                spec_type: None,
                workflow: None,
                current_phase: None,
                active_wave: None,
                created_at: None,
                updated_at: None,
                linked_conversation_id: None,
            })
        } else {
            RawSpecJson {
                id: Some(spec_id.to_string()),
                title: None,
                spec_type: None,
                workflow: None,
                current_phase: None,
                active_wave: None,
                created_at: None,
                updated_at: None,
                linked_conversation_id: None,
            }
        };

        meta.linked_conversation_id = conversation_id;
        meta.updated_at = Some(chrono::Utc::now().timestamp_millis());

        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| SpecError::Serialization(e.to_string()))?;
        fs::write(&spec_json_path, meta_json).map_err(|e| (spec_json_path.clone(), e))?;

        read_spec_detail(&spec_dir, spec_id, location_type)
    }
}

// ── Private helpers ────────────────────────────────────────────────────────

fn find_spec_dir(workspace: &Path, spec_id: &str) -> Result<(PathBuf, SpecLocationType), SpecError> {
    let candidates = [
        (workspace.join(".aion").join("specs").join(spec_id), SpecLocationType::Aion),
        (workspace.join(".kiro").join("specs").join(spec_id), SpecLocationType::Kiro),
        (workspace.join(".specs").join(spec_id), SpecLocationType::OpenSpec),
    ];

    for (cand, loc_type) in candidates {
        if cand.is_dir() {
            return Ok((cand, loc_type));
        }
    }

    // Try scanning summaries in case spec_id does not equal directory slug
    let summaries = scan_workspace_specs(workspace)?;
    if let Some(matching) = summaries.into_iter().find(|s| s.id == spec_id) {
        return Ok((PathBuf::from(matching.directory_path), matching.location_type));
    }

    Err(SpecError::SpecNotFound {
        id: spec_id.to_string(),
    })
}

fn read_spec_detail(
    spec_dir: &Path,
    spec_id: &str,
    location_type: SpecLocationType,
) -> Result<SpecDetail, SpecError> {
    let spec_json_path = spec_dir.join("spec.json");
    let raw_meta: Option<RawSpecJson> = if spec_json_path.is_file() {
        fs::read_to_string(&spec_json_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    } else {
        None
    };

    let slug = spec_dir.file_name().and_then(|s| s.to_str()).unwrap_or(spec_id);

    let id = raw_meta
        .as_ref()
        .and_then(|m| m.id.clone())
        .unwrap_or_else(|| slug.to_string());

    let title = raw_meta
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| slug.replace('-', " "));

    let spec_type = raw_meta.as_ref().and_then(|m| m.spec_type).unwrap_or({
        if spec_dir.join("bugfix.md").is_file() {
            SpecType::Bugfix
        } else {
            SpecType::Feature
        }
    });

    let workflow = raw_meta
        .as_ref()
        .and_then(|m| m.workflow)
        .unwrap_or(SpecWorkflow::RequirementsFirst);

    let mut file_mtimes = HashMap::new();
    let mut max_mtime = 0i64;

    let read_file_and_mtime = |path: PathBuf, filename: &str, mtimes: &mut HashMap<String, i64>, max_m: &mut i64| -> Option<String> {
        if path.is_file() {
            let mtime = get_file_mtime(&path);
            mtimes.insert(filename.to_string(), mtime);
            if mtime > *max_m {
                *max_m = mtime;
            }
            fs::read_to_string(&path).ok()
        } else {
            None
        }
    };

    let requirements_md = if spec_type == SpecType::Bugfix && spec_dir.join("bugfix.md").is_file() {
        read_file_and_mtime(spec_dir.join("bugfix.md"), "bugfix.md", &mut file_mtimes, &mut max_mtime)
    } else {
        read_file_and_mtime(spec_dir.join("requirements.md"), "requirements.md", &mut file_mtimes, &mut max_mtime)
    };

    let design_md = read_file_and_mtime(spec_dir.join("design.md"), "design.md", &mut file_mtimes, &mut max_mtime);
    let tasks_md = read_file_and_mtime(spec_dir.join("tasks.md"), "tasks.md", &mut file_mtimes, &mut max_mtime);

    let waves = tasks_md
        .as_deref()
        .map(parse_tasks_document)
        .unwrap_or_default();

    let mut total_tasks = 0;
    let mut completed_tasks = 0;
    let mut active_wave = 1;
    let mut found_incomplete = false;

    for wave in &waves {
        for task in &wave.tasks {
            total_tasks += 1;
            if task.status == TaskStatus::Completed {
                completed_tasks += 1;
            } else if !found_incomplete {
                active_wave = wave.number;
                found_incomplete = true;
            }
        }
    }

    let current_phase = raw_meta
        .as_ref()
        .and_then(|m| m.current_phase)
        .unwrap_or_else(|| {
            if total_tasks > 0 && completed_tasks == total_tasks {
                SpecPhase::Completed
            } else if total_tasks > 0 && completed_tasks > 0 {
                SpecPhase::Executing
            } else if tasks_md.is_some() {
                SpecPhase::Tasks
            } else if design_md.is_some() {
                SpecPhase::Design
            } else {
                SpecPhase::Requirements
            }
        });

    let active_wave = raw_meta.as_ref().and_then(|m| m.active_wave).unwrap_or(active_wave);
    let created_at = raw_meta.as_ref().and_then(|m| m.created_at).unwrap_or(max_mtime);
    let updated_at = raw_meta.as_ref().and_then(|m| m.updated_at).unwrap_or(max_mtime);
    let linked_conversation_id = raw_meta.as_ref().and_then(|m| m.linked_conversation_id.clone());

    let summary = WorkspaceSpecSummary {
        id,
        title,
        spec_type,
        workflow,
        current_phase,
        active_wave,
        completed_tasks,
        total_tasks,
        directory_path: spec_dir.to_string_lossy().to_string(),
        location_type,
        created_at,
        updated_at,
        linked_conversation_id,
    };

    Ok(SpecDetail {
        summary,
        requirements_md,
        design_md,
        tasks_md,
        waves,
        mtime: max_mtime,
        file_mtimes,
    })
}

fn touch_spec_json(spec_dir: &Path) -> Result<(), SpecError> {
    let spec_json_path = spec_dir.join("spec.json");
    if spec_json_path.exists() {
        if let Ok(text) = fs::read_to_string(&spec_json_path) {
            if let Ok(mut meta) = serde_json::from_str::<RawSpecJson>(&text) {
                meta.updated_at = Some(chrono::Utc::now().timestamp_millis());
                if let Ok(updated_text) = serde_json::to_string_pretty(&meta) {
                    let _ = fs::write(&spec_json_path, updated_text);
                }
            }
        }
    }
    Ok(())
}

fn get_file_mtime(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn validate_filename(filename: &str) -> Result<(), SpecError> {
    match filename {
        "requirements.md" | "bugfix.md" | "design.md" | "tasks.md" | "spec.json" => Ok(()),
        _ => Err(SpecError::InvalidFileName {
            filename: filename.to_string(),
        }),
    }
}

fn slugify(title: &str) -> String {
    let slug = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let clean = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if clean.is_empty() {
        format!("spec-{}", chrono::Utc::now().timestamp())
    } else {
        clean
    }
}

fn feature_requirements_template(title: &str) -> String {
    format!(
        r#"# Requirements Specification: {title}

## 1. User Stories
- As a user, I want this feature so that I can achieve my goals efficiently.

## 2. Acceptance Criteria (EARS Notation)
- [REQ-1.1] Ubiquitous: The system shall provide a responsive user interface for {title}.
- [REQ-1.2] Event-driven: When the user initiates the action, the system shall process and validate the request.
- [REQ-1.3] State-driven: While the system is executing the task, it shall present real-time progress indicators.
- [REQ-1.4] Unwanted behavior: If the request encounters an unexpected error, the system shall safely halt and surface a clear diagnostic error message.
- [REQ-1.5] Optional: Where enhanced telemetry is enabled, the system shall record detailed metrics.
"#
    )
}

fn bugfix_requirements_template(title: &str) -> String {
    format!(
        r#"# Bugfix Specification: {title}

## 1. Problem Description
- Summary of defect or unexpected behavior in current production code.

## 2. Root Cause Analysis
- Identified root cause and affected components.

## 3. Corrective Acceptance Criteria (EARS Notation)
- [REQ-1.1] Event-driven: When the error-triggering condition occurs, the system shall handle it gracefully without crashing.
- [REQ-1.2] Ubiquitous: The system shall maintain consistent state before and after execution.
- [REQ-1.3] State-driven: While under concurrent load, the system shall guard against race conditions.
"#
    )
}

fn design_template(title: &str) -> String {
    format!(
        r#"# Technical Design: {title}

## 1. Overview & Architectural Principles
Detailed technical architecture and design decisions for {title}.

## 2. Component Structure & Data Models
- Schemas and interfaces
- Sequence flows

## 3. APIs & Service Signatures
- REST / IPC endpoint definitions
- Error code definitions

## 4. Steering Alignment & Failure Recovery
- Adherence to project conventions (AGENTS.md)
- Graceful degradation and rollback procedures
"#
    )
}

fn tasks_template(_title: &str) -> String {
    r#"# Implementation Tasks

### Wave 1: Foundation
- [ ] 1. Initialize data contracts and core types (Refs: REQ-1.1)
- [ ] 1.1. Setup baseline unit test fixtures

### Wave 2: Core Implementation
- [ ] 2. Implement business logic and service handlers (Refs: REQ-1.2)
- [ ] 2.1. Verify edge-case handling and error recovery (Refs: REQ-1.4)

### Wave 3: Verification & Integration
- [ ] 3. End-to-end verification and UI integration (Refs: REQ-1.3)
"#
    .to_string()
}
