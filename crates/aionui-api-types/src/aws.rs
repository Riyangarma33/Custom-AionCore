use serde::{Deserialize, Serialize};

/// Runtime environment details for the AWS CLI installation on the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsRuntimeInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub user: String,
    pub config_path: String,
    pub credentials_path: String,
    pub sso_cache_path: String,
    pub login_cache_path: String,
}

/// Authentication mechanism configured for an AWS CLI profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsProfileAuthMethod {
    Sso,
    ConsoleLogin,
    AssumeRole,
    StaticKey,
    CredentialProcess,
    Unknown,
}

/// Verified AWS caller identity returned from `aws sts get-caller-identity`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AwsCallerIdentity {
    pub account: String,
    pub arn: String,
    pub user_id: String,
}

/// Summary of an AWS CLI profile parsed from `~/.aws/config` and `~/.aws/credentials`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsProfileSummary {
    pub name: String,
    pub auth_method: AwsProfileAuthMethod,
    pub region: Option<String>,
    pub output: Option<String>,
    pub sso_session: Option<String>,
    pub sso_start_url: Option<String>,
    pub sso_region: Option<String>,
    pub sso_account_id: Option<String>,
    pub sso_role_name: Option<String>,
    pub login_session: Option<String>,
    pub role_arn: Option<String>,
    pub source_profile: Option<String>,
    pub has_access_key: bool,
    pub masked_access_key_id: Option<String>,
    pub identity: Option<AwsCallerIdentity>,
    pub status: String, // "valid", "expired", "missing_credentials", "untested", "error"
    pub last_checked: Option<String>,
    pub error_message: Option<String>,
}

/// Request body for `POST /api/aws/profiles/test-identity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsTestIdentityRequest {
    pub profile: String,
}

/// Response body for `POST /api/aws/profiles/test-identity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsTestIdentityResponse {
    pub profile: String,
    pub status: String, // "valid", "expired", "missing_credentials", "error"
    pub identity: Option<AwsCallerIdentity>,
    pub error_message: Option<String>,
}

/// Request body for `POST /api/aws/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsLoginRequest {
    pub profile: String,
    #[serde(default)]
    pub auth_type: Option<String>, // "sso", "remote", or None for auto
}

/// Request body for `POST /api/aws/jobs/{id}/submit-code`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsSubmitCodeRequest {
    pub code: String,
}

/// Execution lifecycle states of an interactive AWS login job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsJobState {
    Starting,
    NeedsBrowser,
    NeedsUserCode,
    Pending,
    Success,
    Failed,
    Expired,
    Cancelled,
}

/// Status payload for `GET /api/aws/jobs/{id}` and login endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsLoginJobStatus {
    pub job_id: String,
    pub profile_name: String,
    pub auth_type: String, // "sso" or "remote"
    pub state: AwsJobState,
    pub started_at: i64,
    pub expires_at: i64,
    pub verification_uri: Option<String>,
    pub user_code: Option<String>,
    pub sign_in_url: Option<String>,
    pub error_message: Option<String>,
    pub identity: Option<AwsCallerIdentity>,
}

/// Request body for `POST /api/aws/profiles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsSaveProfileRequest {
    pub name: String,
    pub original_name: Option<String>,
    pub region: Option<String>,
    pub output: Option<String>,
    pub auth_method: Option<String>,
    pub sso_session: Option<String>,
    pub sso_start_url: Option<String>,
    pub sso_region: Option<String>,
    pub sso_account_id: Option<String>,
    pub sso_role_name: Option<String>,
    pub login_session: Option<String>,
    pub role_arn: Option<String>,
    pub source_profile: Option<String>,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub raw_config_section: Option<String>,
}

/// Request parameter for `DELETE /api/aws/profiles/{name}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsDeleteProfileRequest {
    pub name: String,
}

/// Request body for `POST /api/aws/sso-sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsSaveSsoSessionRequest {
    pub name: String,
    pub sso_start_url: String,
    pub sso_region: String,
    #[serde(default)]
    pub sso_registration_scopes: Option<String>,
}

/// Information about an AWS IAM Identity Center SSO session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsSsoSessionSummary {
    pub name: String,
    pub sso_start_url: String,
    pub sso_region: String,
    pub sso_registration_scopes: Option<String>,
}
