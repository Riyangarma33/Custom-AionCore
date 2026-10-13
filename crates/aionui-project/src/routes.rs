// `ApiError` is the intended error type at this HTTP boundary (routes map the
// crate-owned `ProjectError` to it here), so the disallowed_types lint that
// steers service code away from `ApiError` does not apply to this module.
#![allow(clippy::disallowed_types)]

//! Project Explorer control-plane HTTP routes.
//!
//! Read a project's roots (`GET /api/projects/{id}`) and mutate its attached
//! folders (`POST`/`DELETE .../folders`). Filesystem content is served
//! separately over the `fs/*` WebSocket protocol; these routes only expose the
//! project shell + root list the explorer needs to open subscriptions.
//!
//! Handlers do request/response transformation only; all business logic lives
//! in [`ProjectService`]. Domain [`ProjectError`]s map to `ApiError` with
//! stable, machine-readable codes (`project_explorer_duplicate`,
//! `project_explorer_overlap`, …) so the frontend can branch without parsing
//! human messages.

use std::sync::Arc;

use aionui_api_types::{
    ApiResponse, AttachFolderRequest, CreateSpecRequest, LinkConversationRequest, PatchTaskRequest,
    ProjectDetailResponse, ProjectEntry, ProjectExplorer, ResolveRefRequest, ResolveRefResponse, SpecDetail,
    TransitionPhaseRequest, UpdateSpecFileRequest, WorkspaceSpecSummary,
};
use aionui_auth::CurrentUser;
use aionui_common::ApiError;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Router};
use serde_json::json;

use crate::canonical;
use crate::service::ProjectService;
use crate::specs::{SpecError, SpecService};
use crate::types::{AttachInput, ProjectDetail, ProjectError, ProjectExplorerEntry};

/// Shared state for project route handlers.
#[derive(Clone)]
pub struct ProjectRouterState {
    pub project: Arc<ProjectService>,
}

/// Build the project control-plane router (`/api/projects/*`).
///
/// All routes require authentication (applied by the caller).
pub fn project_routes(state: ProjectRouterState) -> Router {
    Router::new()
        .route("/api/projects/{project_id}", get(get_project))
        .route("/api/projects/{project_id}/folders", post(attach_folder))
        .route("/api/projects/{project_id}/folders/{pe_id}", delete(remove_folder))
        .route("/api/projects/{project_id}/resolve-ref", post(resolve_ref))
        .route("/api/projects/{project_id}/specs", get(list_specs).post(create_spec))
        .route("/api/projects/{project_id}/specs/{spec_id}", get(get_spec))
        .route("/api/projects/{project_id}/specs/{spec_id}/files/{filename}", put(update_spec_file))
        .route("/api/projects/{project_id}/specs/{spec_id}/tasks/{task_id}", patch(patch_spec_task))
        .route("/api/projects/{project_id}/specs/{spec_id}/phase", post(transition_spec_phase))
        .route("/api/projects/{project_id}/specs/{spec_id}/link", post(link_spec_conversation))
        .with_state(state)
}

/// `GET /api/projects/{project_id}` — full project detail + all roots in one
/// call (frontend does not fan out per root).
async fn get_project(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<ApiResponse<ProjectDetailResponse>>, ApiError> {
    let detail = state.project.get_project(&user.id, &project_id).await?;
    Ok(Json(ApiResponse::ok(to_detail_response(detail))))
}

/// `POST /api/projects/{project_id}/resolve-ref` — re-express a `ChatFileRef` in
/// its strongest form for this project.
///
/// The explorer and a chat link produce different refs for the same file
/// (`Project` vs `Local`), so callers that key on the ref — tab identity, the
/// `fs` change signal — would otherwise treat one file as two. This resolves a
/// `Local` path that lives under one of the project's roots into the `Project`
/// form; `Project` and `Upload` come back untouched.
///
/// Always succeeds with a usable ref: a path outside every root, or one that does
/// not exist, is echoed back unchanged rather than raising. "Not upgradeable" is
/// an ordinary answer here, and a caller mid-way through opening a missing file
/// still needs its ref to render that state.
///
/// The judgement stays server-side because case folding is a compile-time
/// platform fork (`canonical::IGNORE_PATH_CASING`); a client comparing path
/// strings would miss matches on macOS and merge distinct files on Linux.
async fn resolve_ref(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    body: Result<Json<ResolveRefRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ResolveRefResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let file = state
        .project
        .upgrade_chat_file_ref(&user.id, &project_id, &req.file)
        .await?;
    let upgraded = file != req.file;
    Ok(Json(ApiResponse::ok(ResolveRefResponse { file, upgraded })))
}

/// `POST /api/projects/{project_id}/folders` — attach a folder. Returns the
/// single new (or focused-existing) entry so the frontend can splice it in
/// without re-fetching the project.
async fn attach_folder(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    body: Result<Json<AttachFolderRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ProjectEntry>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let row = state
        .project
        .attach_folder(
            &user.id,
            AttachInput {
                project_id: project_id.clone(),
                uri: req.uri,
                display_name: req.display_name,
            },
        )
        .await?;

    // `attach_folder` returns the bare explorer row (no folder metadata). Re-read
    // the project to build the fully-shaped entry (display_path + runtime_status).
    // A descendant-attach focuses an existing entry, so match on the returned
    // pe_id rather than assuming the last row.
    let detail = state.project.get_project(&user.id, &project_id).await?;
    let entry = detail
        .explorer
        .entries
        .into_iter()
        .find(|e| e.pe_id == row.pe_id)
        .ok_or_else(|| ApiError::Internal("attached entry missing after insert".to_owned()))?;
    Ok(Json(ApiResponse::ok(to_entry(entry))))
}

/// `DELETE /api/projects/{project_id}/folders/{pe_id}` — detach an attached
/// folder. The workspace root is immutable (`workspace_entry_immutable`).
async fn remove_folder(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((_project_id, pe_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    state.project.remove_attached(&user.id, &pe_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── mapping: domain → wire DTO ───────────────────────────────────────────────

fn to_detail_response(detail: ProjectDetail) -> ProjectDetailResponse {
    ProjectDetailResponse {
        project_id: detail.id,
        name: detail.name,
        explorer: ProjectExplorer {
            workspace_pe_id: detail.explorer.workspace_pe_id,
            // `get_project` yields entries ordered by order_index ASC.
            entries: detail.explorer.entries.into_iter().map(to_entry).collect(),
        },
    }
}

fn to_entry(entry: ProjectExplorerEntry) -> ProjectEntry {
    // `display_path` is a human-facing rendering of the folder's original
    // resource_uri; fall back to the raw uri if it is not a decodable path.
    let display_path = canonical::uri_to_path(&entry.folder.resource_uri)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| entry.folder.resource_uri.clone());
    ProjectEntry {
        pe_id: entry.pe_id,
        role: entry.role,
        display_name: entry.display_name,
        display_path,
        order_index: entry.order_index,
        runtime_status: entry.folder.runtime_status.as_str().to_owned(),
    }
}

// ── error mapping: ProjectError → ApiError (stable domain codes) ─────────────

impl From<ProjectError> for ApiError {
    fn from(err: ProjectError) -> Self {
        let (status, code, details) = match &err {
            ProjectError::ProjectNotFound { project_id } => (
                StatusCode::NOT_FOUND,
                "project_not_found",
                Some(json!({ "project_id": project_id })),
            ),
            ProjectError::ProjectExplorerNotFound { pe_id } => (
                StatusCode::NOT_FOUND,
                "project_explorer_not_found",
                Some(json!({ "pe_id": pe_id })),
            ),
            ProjectError::ProjectExplorerDuplicate { project_id, folder_id } => (
                StatusCode::CONFLICT,
                "project_explorer_duplicate",
                Some(json!({ "project_id": project_id, "folder_id": folder_id })),
            ),
            ProjectError::ProjectExplorerOverlap { project_id } => (
                StatusCode::CONFLICT,
                "project_explorer_overlap",
                Some(json!({ "project_id": project_id })),
            ),
            ProjectError::WorkspaceEntryImmutable { pe_id } => (
                StatusCode::CONFLICT,
                "workspace_entry_immutable",
                Some(json!({ "pe_id": pe_id })),
            ),
            ProjectError::StandardProjectConflict { folder_id } => (
                StatusCode::CONFLICT,
                "standard_project_conflict",
                Some(json!({ "folder_id": folder_id })),
            ),
            ProjectError::WorkspaceFolderMismatch { project_id, folder_id } => (
                StatusCode::CONFLICT,
                "workspace_folder_mismatch",
                Some(json!({ "project_id": project_id, "folder_id": folder_id })),
            ),
            ProjectError::FolderNotFound { .. } => (StatusCode::NOT_FOUND, "folder_not_found", None),
            ProjectError::FolderNotDirectory { .. } => (StatusCode::BAD_REQUEST, "folder_not_directory", None),
            ProjectError::FolderPermissionDenied { .. } => (StatusCode::FORBIDDEN, "folder_permission_denied", None),
            ProjectError::FolderCanonicalizeFailed { .. }
            | ProjectError::UnsupportedResourceScheme { .. }
            | ProjectError::InvalidRelativePath { .. }
            | ProjectError::ResourceOutsideFolder { .. } => (StatusCode::BAD_REQUEST, "invalid_resource", None),
            ProjectError::TempDirExists { .. } | ProjectError::WorkspaceMissing => {
                (StatusCode::BAD_REQUEST, "invalid_request", None)
            }
            ProjectError::UploadPathOutsideRoot { path } => (
                StatusCode::BAD_REQUEST,
                "upload_path_outside_root",
                Some(json!({ "path": path })),
            ),
            ProjectError::ChatFileMissing { path } => (
                StatusCode::NOT_FOUND,
                "chat_file_missing",
                Some(json!({ "path": path })),
            ),
            ProjectError::LocalPathNotReadable { path } => (
                StatusCode::BAD_REQUEST,
                "local_path_not_readable",
                Some(json!({ "path": path })),
            ),
            ProjectError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", None),
        };
        // Never leak internal DB detail to clients (Security: no internal leakage).
        let message = match &err {
            ProjectError::Database(_) => "internal error".to_owned(),
            other => other.to_string(),
        };
        ApiError::coded(status, code, message, details)
    }
}

// ── SDD Spec Route Handlers ──────────────────────────────────────────────────

async fn resolve_workspace_path(
    state: &ProjectRouterState,
    user_id: &str,
    project_id: &str,
) -> Result<std::path::PathBuf, ApiError> {
    let detail = state.project.get_project(user_id, project_id).await?;
    let ws_entry = detail
        .explorer
        .entries
        .into_iter()
        .find(|e| e.pe_id == detail.explorer.workspace_pe_id)
        .ok_or_else(|| ApiError::NotFound("Workspace entry not found for project".into()))?;
    let path = canonical::uri_to_path(&ws_entry.folder.resource_canonical)
        .map_err(|e| ApiError::BadRequest(format!("Invalid workspace URI: {e}")))?;
    Ok(path)
}

/// `GET /api/projects/{project_id}/specs` — enumerate all specs in workspace.
async fn list_specs(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
) -> Result<Json<ApiResponse<Vec<WorkspaceSpecSummary>>>, ApiError> {
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let specs = SpecService::list_specs(&ws_path)?;
    Ok(Json(ApiResponse::ok(specs)))
}

/// `POST /api/projects/{project_id}/specs` — initialize a new spec.
async fn create_spec(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path(project_id): Path<String>,
    body: Result<Json<CreateSpecRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::create_spec(&ws_path, req)?;
    Ok(Json(ApiResponse::ok(spec)))
}

/// `GET /api/projects/{project_id}/specs/{spec_id}` — fetch full spec details.
async fn get_spec(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, spec_id)): Path<(String, String)>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::get_spec(&ws_path, &spec_id)?;
    Ok(Json(ApiResponse::ok(spec)))
}

/// `PUT /api/projects/{project_id}/specs/{spec_id}/files/{filename}` — update spec markdown file.
async fn update_spec_file(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, spec_id, filename)): Path<(String, String, String)>,
    body: Result<Json<UpdateSpecFileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::update_spec_file(&ws_path, &spec_id, &filename, &req.content, req.expected_mtime)?;
    Ok(Json(ApiResponse::ok(spec)))
}

/// `PATCH /api/projects/{project_id}/specs/{spec_id}/tasks/{task_id}` — toggle checklist item.
async fn patch_spec_task(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, spec_id, task_id)): Path<(String, String, String)>,
    body: Result<Json<PatchTaskRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::patch_task(&ws_path, &spec_id, &task_id, req.status, req.expected_mtime)?;
    Ok(Json(ApiResponse::ok(spec)))
}

/// `POST /api/projects/{project_id}/specs/{spec_id}/phase` — transition phase.
async fn transition_spec_phase(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, spec_id)): Path<(String, String)>,
    body: Result<Json<TransitionPhaseRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::transition_phase(&ws_path, &spec_id, req.phase)?;
    Ok(Json(ApiResponse::ok(spec)))
}

/// `POST /api/projects/{project_id}/specs/{spec_id}/link` — link conversation.
async fn link_spec_conversation(
    State(state): State<ProjectRouterState>,
    Extension(user): Extension<CurrentUser>,
    Path((project_id, spec_id)): Path<(String, String)>,
    body: Result<Json<LinkConversationRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<SpecDetail>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let ws_path = resolve_workspace_path(&state, &user.id, &project_id).await?;
    let spec = SpecService::link_conversation(&ws_path, &spec_id, req.conversation_id)?;
    Ok(Json(ApiResponse::ok(spec)))
}

impl From<SpecError> for ApiError {
    fn from(err: SpecError) -> Self {
        match err {
            SpecError::SpecNotFound { id } => ApiError::NotFound(format!("spec not found: {id}")),
            SpecError::TaskNotFound { id } => ApiError::NotFound(format!("task not found: {id}")),
            SpecError::SpecAlreadyExists { id } => {
                ApiError::coded(StatusCode::CONFLICT, "spec_already_exists", format!("spec already exists: {id}"), None)
            }
            SpecError::ConcurrentModification { file, disk_mtime, expected_mtime } => {
                ApiError::coded(
                    StatusCode::CONFLICT,
                    "concurrent_modification",
                    format!("concurrent modification on {file}: disk mtime {disk_mtime} differs from expected {expected_mtime}"),
                    Some(json!({ "file": file, "disk_mtime": disk_mtime, "expected_mtime": expected_mtime })),
                )
            }
            SpecError::ContainmentViolation { path, workspace } => {
                ApiError::Forbidden(format!("containment violation: path '{path:?}' escapes workspace '{workspace:?}'"))
            }
            SpecError::InvalidFileName { filename } => {
                ApiError::BadRequest(format!("invalid spec file name: {filename}"))
            }
            SpecError::Io { path, source } => {
                ApiError::Internal(format!("io error on {path:?}: {source}"))
            }
            SpecError::Serialization(msg) => {
                ApiError::Internal(format!("serialization error: {msg}"))
            }
        }
    }
}

#[cfg(test)]
#[path = "routes_test.rs"]
mod routes_test;
