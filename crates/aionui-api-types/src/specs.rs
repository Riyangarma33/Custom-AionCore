use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Type of specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecType {
    #[default]
    Feature,
    Bugfix,
}

impl SpecType {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecType::Feature => "feature",
            SpecType::Bugfix => "bugfix",
        }
    }
}

/// Workflow variant chosen when initializing a spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecWorkflow {
    #[default]
    RequirementsFirst,
    DesignFirst,
    QuickSpec,
}

impl SpecWorkflow {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecWorkflow::RequirementsFirst => "requirements_first",
            SpecWorkflow::DesignFirst => "design_first",
            SpecWorkflow::QuickSpec => "quick_spec",
        }
    }
}

/// Current phase in the Spec-Driven Development cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecPhase {
    #[default]
    Requirements,
    Design,
    Tasks,
    Executing,
    Completed,
}

impl SpecPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecPhase::Requirements => "requirements",
            SpecPhase::Design => "design",
            SpecPhase::Tasks => "tasks",
            SpecPhase::Executing => "executing",
            SpecPhase::Completed => "completed",
        }
    }
}

/// Execution status of a checklist task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Todo,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        }
    }
}

/// Discovered directory origin of the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecLocationType {
    #[default]
    Aion,
    Kiro,
    OpenSpec,
}

impl SpecLocationType {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecLocationType::Aion => "aion",
            SpecLocationType::Kiro => "kiro",
            SpecLocationType::OpenSpec => "openspec",
        }
    }
}

/// Single parsed checklist item from `tasks.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecTask {
    pub id: String,
    pub line_number: usize,
    pub status: TaskStatus,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wave: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    #[serde(default)]
    pub indent: usize,
}

/// Wave execution grouping in `tasks.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecWave {
    pub number: usize,
    pub title: String,
    pub tasks: Vec<SpecTask>,
}

/// Lightweight summary of a workspace specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSpecSummary {
    pub id: String,
    pub title: String,
    pub spec_type: SpecType,
    pub workflow: SpecWorkflow,
    pub current_phase: SpecPhase,
    pub active_wave: usize,
    pub completed_tasks: usize,
    pub total_tasks: usize,
    pub directory_path: String,
    pub location_type: SpecLocationType,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_conversation_id: Option<String>,
}

/// Full specification detail including markdown documents and parsed waves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecDetail {
    pub summary: WorkspaceSpecSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirements_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks_md: Option<String>,
    #[serde(default)]
    pub waves: Vec<SpecWave>,
    pub mtime: i64,
    #[serde(default)]
    pub file_mtimes: HashMap<String, i64>,
}

/// Payload to initialize a new specification on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSpecRequest {
    pub title: String,
    #[serde(default)]
    pub spec_type: Option<SpecType>,
    #[serde(default)]
    pub workflow: Option<SpecWorkflow>,
    #[serde(default)]
    pub slug: Option<String>,
}

/// Payload to update a constituent markdown document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSpecFileRequest {
    pub content: String,
    #[serde(default)]
    pub expected_mtime: Option<i64>,
}

/// Payload to toggle/update a checklist task's status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchTaskRequest {
    pub status: TaskStatus,
    #[serde(default)]
    pub expected_mtime: Option<i64>,
}

/// Payload to transition the spec phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionPhaseRequest {
    pub phase: SpecPhase,
}

/// Payload to link or unlink a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkConversationRequest {
    pub conversation_id: Option<String>,
}
