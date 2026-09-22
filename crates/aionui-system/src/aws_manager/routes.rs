#![allow(clippy::disallowed_types)]

use std::sync::Arc;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Path, State};
use axum::routing::{delete, get, post};

use aionui_api_types::{
    ApiResponse, AwsLoginJobStatus, AwsLoginRequest, AwsProfileSummary, AwsRuntimeInfo,
    AwsSaveProfileRequest, AwsSaveSsoSessionRequest, AwsSsoSessionSummary, AwsSubmitCodeRequest,
    AwsTestIdentityRequest, AwsTestIdentityResponse,
};
use aionui_auth::CurrentUser;
use aionui_common::ApiError;

use super::service::AwsManagerService;

/// Router state for AWS CLI manager routes.
#[derive(Clone)]
pub struct AwsRouterState {
    pub service: Arc<AwsManagerService>,
}

/// Build the AWS CLI manager router.
///
/// All routes require authentication (applied by the caller via auth_middleware).
pub fn aws_routes(state: AwsRouterState) -> Router {
    Router::new()
        .route("/api/aws/runtime", get(get_runtime_info))
        .route("/api/aws/profiles", get(list_profiles).post(save_profile))
        .route("/api/aws/profiles/test-identity", post(test_identity))
        .route("/api/aws/profiles/{name}", delete(delete_profile))
        .route("/api/aws/sso-sessions", get(list_sso_sessions).post(save_sso_session))
        .route("/api/aws/login", post(start_login))
        .route("/api/aws/jobs/{id}", get(get_job_status))
        .route("/api/aws/jobs/{id}/submit-code", post(submit_code))
        .route("/api/aws/jobs/{id}/cancel", post(cancel_job))
        .with_state(state)
}

/// GET /api/aws/runtime — runtime discovery (version, runtime user, config paths)
async fn get_runtime_info(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<AwsRuntimeInfo>>, ApiError> {
    let info = state.service.get_runtime_info().await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(info)))
}

/// GET /api/aws/profiles — list all classified AWS profiles
async fn list_profiles(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AwsProfileSummary>>>, ApiError> {
    let profiles = state.service.list_profiles().await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(profiles)))
}

/// POST /api/aws/profiles/test-identity — on-demand caller identity test
async fn test_identity(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AwsTestIdentityRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AwsTestIdentityResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let res = state
        .service
        .test_identity(&req.profile)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(res)))
}

/// POST /api/aws/profiles — create or update AWS profile configuration
async fn save_profile(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AwsSaveProfileRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.service.save_profile(req).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::message("Profile saved successfully")))
}

/// DELETE /api/aws/profiles/{name} — delete an AWS profile configuration
async fn delete_profile(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(name): Path<String>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    state.service.delete_profile(&name).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::message("Profile deleted successfully")))
}

/// GET /api/aws/sso-sessions — list all configured SSO sessions
async fn list_sso_sessions(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
) -> Result<Json<ApiResponse<Vec<AwsSsoSessionSummary>>>, ApiError> {
    let sessions = state.service.list_sso_sessions().await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(sessions)))
}

/// POST /api/aws/sso-sessions — create or update an SSO session configuration
async fn save_sso_session(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AwsSaveSsoSessionRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    state.service.save_sso_session(req).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::message("SSO session saved successfully")))
}

/// POST /api/aws/login — start an interactive login job
async fn start_login(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<AwsLoginRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AwsLoginJobStatus>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .start_login(&req.profile, req.auth_type.as_deref())
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(status)))
}

/// GET /api/aws/jobs/{id} — get status of an interactive login job
async fn get_job_status(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AwsLoginJobStatus>>, ApiError> {
    let status = state.service.get_job_status(&id).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(status)))
}

/// POST /api/aws/jobs/{id}/submit-code — submit authorization code for console login flow
async fn submit_code(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
    body: Result<Json<AwsSubmitCodeRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AwsLoginJobStatus>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let status = state
        .service
        .submit_code(&id, &req.code)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(status)))
}

/// POST /api/aws/jobs/{id}/cancel — cancel a running login job
async fn cancel_job(
    State(state): State<AwsRouterState>,
    Extension(_user): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<AwsLoginJobStatus>>, ApiError> {
    let status = state.service.cancel_job(&id).await.map_err(ApiError::from)?;
    Ok(Json(ApiResponse::ok(status)))
}
