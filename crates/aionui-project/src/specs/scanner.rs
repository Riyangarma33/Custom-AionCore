use std::fs;
use std::path::Path;
use aionui_api_types::{
    SpecLocationType, SpecPhase, SpecType, SpecWorkflow, TaskStatus, WorkspaceSpecSummary,
};
use crate::specs::ast::parse_tasks_document;
use crate::specs::types::SpecError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSpecJson {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub spec_type: Option<SpecType>,
    #[serde(default)]
    pub workflow: Option<SpecWorkflow>,
    #[serde(default)]
    pub current_phase: Option<SpecPhase>,
    #[serde(default)]
    pub active_wave: Option<usize>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub linked_conversation_id: Option<String>,
}

/// Discovers and summarizes all specification directories within a project workspace.
///
/// Scans:
/// - `<workspace>/.aion/specs/`
/// - `<workspace>/.kiro/specs/`
/// - `<workspace>/.specs/`
///
/// Enforces workspace root containment: specs attempting path traversal outside
/// `workspace_path` are rejected.
pub fn scan_workspace_specs(workspace_path: &Path) -> Result<Vec<WorkspaceSpecSummary>, SpecError> {
    let workspace_canonical = match fs::canonicalize(workspace_path) {
        Ok(c) => c,
        Err(_) => workspace_path.to_path_buf(),
    };

    let search_roots = [
        (workspace_path.join(".aion").join("specs"), SpecLocationType::Aion),
        (workspace_path.join(".kiro").join("specs"), SpecLocationType::Kiro),
        (workspace_path.join(".specs"), SpecLocationType::OpenSpec),
    ];

    let mut summaries = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for (root_dir, location_type) in search_roots {
        if !root_dir.is_dir() {
            continue;
        }

        let entries = match fs::read_dir(&root_dir) {
            Ok(iter) => iter,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let spec_dir = entry.path();
            if !spec_dir.is_dir() {
                continue;
            }

            // Containment check: prevent directory traversal or escaped symlinks
            let canonical_dir = match fs::canonicalize(&spec_dir) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if !canonical_dir.starts_with(&workspace_canonical) {
                tracing::warn!(
                    "Spec directory {:?} escapes workspace root {:?}",
                    canonical_dir,
                    workspace_canonical
                );
                continue;
            }

            let slug = entry.file_name().to_string_lossy().to_string();
            if slug.starts_with('.') {
                continue;
            }

            if let Some(summary) = inspect_spec_dir(&spec_dir, &slug, location_type) {
                if !seen_ids.contains(&summary.id) {
                    seen_ids.insert(summary.id.clone());
                    summaries.push(summary);
                }
            }
        }
    }

    // Sort by updated_at descending
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    Ok(summaries)
}

fn inspect_spec_dir(
    spec_dir: &Path,
    slug: &str,
    location_type: SpecLocationType,
) -> Option<WorkspaceSpecSummary> {
    let spec_json_path = spec_dir.join("spec.json");
    let raw_meta: Option<RawSpecJson> = if spec_json_path.is_file() {
        fs::read_to_string(&spec_json_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    } else {
        None
    };

    let dir_mtime = fs::metadata(spec_dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let id = raw_meta
        .as_ref()
        .and_then(|m| m.id.clone())
        .unwrap_or_else(|| slug.to_string());

    let title = raw_meta
        .as_ref()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| humanize_slug(slug));

    let spec_type = raw_meta
        .as_ref()
        .and_then(|m| m.spec_type)
        .unwrap_or_else(|| {
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

    // Parse tasks if tasks.md exists
    let tasks_path = spec_dir.join("tasks.md");
    let (completed_tasks, total_tasks, active_wave, tasks_mtime) = if tasks_path.is_file() {
        let text = fs::read_to_string(&tasks_path).unwrap_or_default();
        let waves = parse_tasks_document(&text);
        let mut total = 0;
        let mut completed = 0;
        let mut first_incomplete_wave = 1;
        let mut found_incomplete = false;

        for wave in &waves {
            for task in &wave.tasks {
                total += 1;
                if task.status == TaskStatus::Completed {
                    completed += 1;
                } else if !found_incomplete {
                    first_incomplete_wave = wave.number;
                    found_incomplete = true;
                }
            }
        }

        let mtime = fs::metadata(&tasks_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(dir_mtime);

        (completed, total, first_incomplete_wave, mtime)
    } else {
        (0, 0, 1, dir_mtime)
    };

    let current_phase = raw_meta
        .as_ref()
        .and_then(|m| m.current_phase)
        .unwrap_or_else(|| {
            if total_tasks > 0 && completed_tasks == total_tasks {
                SpecPhase::Completed
            } else if total_tasks > 0 && completed_tasks > 0 {
                SpecPhase::Executing
            } else if tasks_path.is_file() {
                SpecPhase::Tasks
            } else if spec_dir.join("design.md").is_file() {
                SpecPhase::Design
            } else {
                SpecPhase::Requirements
            }
        });

    let active_wave = raw_meta
        .as_ref()
        .and_then(|m| m.active_wave)
        .unwrap_or(active_wave);

    let created_at = raw_meta
        .as_ref()
        .and_then(|m| m.created_at)
        .unwrap_or(dir_mtime);

    let updated_at = raw_meta
        .as_ref()
        .and_then(|m| m.updated_at)
        .unwrap_or_else(|| std::cmp::max(dir_mtime, tasks_mtime));

    let linked_conversation_id = raw_meta.as_ref().and_then(|m| m.linked_conversation_id.clone());

    Some(WorkspaceSpecSummary {
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
    })
}

fn humanize_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
