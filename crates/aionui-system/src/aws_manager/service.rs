use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use aionui_api_types::{
    AwsCallerIdentity, AwsJobState, AwsLoginJobStatus, AwsProfileSummary, AwsRuntimeInfo,
    AwsSaveProfileRequest, AwsSaveSsoSessionRequest, AwsSsoSessionSummary, AwsTestIdentityResponse,
};
use aionui_runtime::{Builder as CmdBuilder, kill_process_tree};
use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{error, info, warn};

use crate::error::SystemError;

use super::config_parser::{
    default_config_path, default_credentials_path, default_login_cache_path, default_sso_cache_path,
    delete_profile as parser_delete_profile, load_all_profiles, load_all_sso_sessions,
    save_profile as parser_save_profile, save_sso_session as parser_save_sso_session,
};
use super::job_store::{AwsJobStore, AwsLoginJobRecord};

struct ActiveJobHandle {
    #[allow(dead_code)]
    job_id: String,
    #[allow(dead_code)]
    profile_name: String,
    #[allow(dead_code)]
    pid: u32,
    stdin_tx: Option<mpsc::Sender<String>>,
    cancel_tx: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
pub struct AwsManagerService {
    config_path: PathBuf,
    credentials_path: PathBuf,
    sso_cache_path: PathBuf,
    login_cache_path: PathBuf,
    job_store: Arc<AwsJobStore>,
    active_profiles: Arc<Mutex<HashMap<String, String>>>, // profile_name -> job_id
    active_handles: Arc<Mutex<HashMap<String, ActiveJobHandle>>>, // job_id -> handle
}

impl AwsManagerService {
    pub fn new(work_dir: PathBuf) -> Self {
        let job_store = Arc::new(AwsJobStore::new(&work_dir));

        // Spawn startup reconciliation pass
        let store_clone = Arc::clone(&job_store);
        tokio::spawn(async move {
            let count = store_clone.reconcile_startup_jobs().await;
            if count > 0 {
                info!(reconciled_jobs = count, "Reconciled orphaned AWS CLI login jobs on startup");
            }
        });

        Self {
            config_path: default_config_path(),
            credentials_path: default_credentials_path(),
            sso_cache_path: default_sso_cache_path(),
            login_cache_path: default_login_cache_path(),
            job_store,
            active_profiles: Arc::new(Mutex::new(HashMap::new())),
            active_handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Retrieve runtime discovery details about AWS CLI on the host.
    pub async fn get_runtime_info(&self) -> Result<AwsRuntimeInfo, SystemError> {
        let mut cmd = CmdBuilder::clean_cli("aws");
        cmd.arg("--version");
        let probe = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await;

        let (installed, version) = match probe {
            Ok(Ok(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let full = if !stdout.is_empty() { stdout } else { stderr };
                (true, Some(full))
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                (false, Some(stderr))
            }
            _ => (false, None),
        };

        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "aionui".to_string());

        Ok(AwsRuntimeInfo {
            installed,
            version,
            user,
            config_path: self.config_path.to_string_lossy().to_string(),
            credentials_path: self.credentials_path.to_string_lossy().to_string(),
            sso_cache_path: self.sso_cache_path.to_string_lossy().to_string(),
            login_cache_path: self.login_cache_path.to_string_lossy().to_string(),
        })
    }

    /// List and classify all profiles from `~/.aws/config` and `~/.aws/credentials`.
    pub async fn list_profiles(&self) -> Result<Vec<AwsProfileSummary>, SystemError> {
        let profiles = load_all_profiles(&self.config_path, &self.credentials_path);
        Ok(profiles)
    }

    /// Execute safe on-demand identity check using `aws sts get-caller-identity --profile <profile>`.
    pub async fn test_identity(&self, profile: &str) -> Result<AwsTestIdentityResponse, SystemError> {
        let profile = profile.trim();
        if profile.is_empty() {
            return Err(SystemError::BadRequest("Profile name cannot be empty".to_string()));
        }

        let mut cmd = CmdBuilder::clean_cli("aws");
        cmd.args([
            "sts",
            "get-caller-identity",
            "--profile",
            profile,
            "--output",
            "json",
            "--cli-connect-timeout",
            "5",
            "--cli-read-timeout",
            "5",
        ]);

        let output_res = tokio::time::timeout(Duration::from_secs(8), cmd.output()).await;

        match output_res {
            Ok(Ok(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    let account = json.get("Account").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let arn = json.get("Arn").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let user_id = json.get("UserId").and_then(|v| v.as_str()).unwrap_or("").to_string();

                    Ok(AwsTestIdentityResponse {
                        profile: profile.to_string(),
                        status: "valid".to_string(),
                        identity: Some(AwsCallerIdentity { account, arn, user_id }),
                        error_message: None,
                    })
                } else {
                    Ok(AwsTestIdentityResponse {
                        profile: profile.to_string(),
                        status: "valid".to_string(),
                        identity: None,
                        error_message: None,
                    })
                }
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let lower = stderr.to_lowercase();
                let status = if lower.contains("expired") || lower.contains("token") || lower.contains("invalid") {
                    "expired"
                } else if lower.contains("could not be found") || lower.contains("no credentials") {
                    "missing_credentials"
                } else {
                    "error"
                };

                let clean_msg = stderr
                    .lines()
                    .filter(|l| !l.starts_with("getpass.py") && !l.starts_with("Warning:"))
                    .collect::<Vec<_>>()
                    .join(" ");

                Ok(AwsTestIdentityResponse {
                    profile: profile.to_string(),
                    status: status.to_string(),
                    identity: None,
                    error_message: Some(if clean_msg.is_empty() { stderr } else { clean_msg }),
                })
            }
            Ok(Err(e)) => Ok(AwsTestIdentityResponse {
                profile: profile.to_string(),
                status: "error".to_string(),
                identity: None,
                error_message: Some(format!("Failed to execute aws CLI: {e}")),
            }),
            Err(_) => Ok(AwsTestIdentityResponse {
                profile: profile.to_string(),
                status: "error".to_string(),
                identity: None,
                error_message: Some("Identity check timed out after 8 seconds".to_string()),
            }),
        }
    }

    /// Start an interactive login job (`aws sso login` or `aws login --remote`).
    pub async fn start_login(
        &self,
        profile: &str,
        requested_auth_type: Option<&str>,
    ) -> Result<AwsLoginJobStatus, SystemError> {
        let profile = profile.trim();
        if profile.is_empty() {
            return Err(SystemError::BadRequest("Profile name cannot be empty".to_string()));
        }

        // Check per-profile active lock
        {
            let active = self.active_profiles.lock().await;
            if let Some(existing_job_id) = active.get(profile) {
                if let Some(record) = self.job_store.load_job(existing_job_id) {
                    if matches!(
                        record.state,
                        AwsJobState::Starting
                            | AwsJobState::NeedsBrowser
                            | AwsJobState::NeedsUserCode
                            | AwsJobState::Pending
                    ) {
                        info!(
                            profile,
                            job_id = %existing_job_id,
                            "Attaching to existing in-flight AWS login job"
                        );
                        return Ok(AwsLoginJobStatus::from(&record));
                    }
                }
            }
        }

        // Determine auth_type
        let auth_type = if let Some(t) = requested_auth_type {
            if t == "remote" || t == "console_login" {
                "remote".to_string()
            } else {
                "sso".to_string()
            }
        } else {
            // Auto-detect based on profile config
            let profiles = load_all_profiles(&self.config_path, &self.credentials_path);
            let profile_match = profiles.iter().find(|p| p.name == profile);
            if let Some(p) = profile_match {
                if p.auth_method == aionui_api_types::AwsProfileAuthMethod::ConsoleLogin {
                    "remote".to_string()
                } else {
                    "sso".to_string()
                }
            } else {
                "sso".to_string()
            }
        };

        let job_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + 600; // 10 minutes hard timeout

        let mut cmd = CmdBuilder::new("aws");
        if auth_type == "remote" {
            cmd.args(["login", "--profile", profile, "--remote"]);
        } else {
            let profiles = load_all_profiles(&self.config_path, &self.credentials_path);
            let profile_match = profiles.iter().find(|p| p.name == profile);
            if let Some(p) = profile_match && let Some(sess) = &p.sso_session {
                cmd.args([
                    "sso",
                    "login",
                    "--sso-session",
                    sess.as_str(),
                    "--no-browser",
                    "--use-device-code",
                ]);
            } else {
                cmd.args([
                    "sso",
                    "login",
                    "--profile",
                    profile,
                    "--no-browser",
                    "--use-device-code",
                ]);
            }
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb");

        info!(
            job_id = %job_id,
            profile,
            auth_type = %auth_type,
            "Spawning interactive AWS login CLI job"
        );

        let mut child = cmd.spawn().map_err(|e| {
            SystemError::Internal(format!("Failed to spawn AWS login CLI process: {e}"))
        })?;

        let pid = child.id().ok_or_else(|| {
            SystemError::Internal("Failed to obtain PID from spawned AWS CLI process".to_string())
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            SystemError::Internal("Failed to capture stdout of AWS CLI process".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            SystemError::Internal("Failed to capture stderr of AWS CLI process".to_string())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            SystemError::Internal("Failed to capture stdin of AWS CLI process".to_string())
        })?;

        let initial_record = AwsLoginJobRecord {
            job_id: job_id.clone(),
            profile_name: profile.to_string(),
            auth_type: auth_type.clone(),
            pid: Some(pid),
            state: AwsJobState::Starting,
            started_at: now,
            expires_at,
            verification_uri: None,
            user_code: None,
            sign_in_url: None,
            error_message: None,
            identity: None,
        };

        self.job_store.save_job(&initial_record).map_err(SystemError::Internal)?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(4);
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

        // Register in active maps
        {
            let mut active = self.active_profiles.lock().await;
            active.insert(profile.to_string(), job_id.clone());

            let mut handles = self.active_handles.lock().await;
            handles.insert(
                job_id.clone(),
                ActiveJobHandle {
                    job_id: job_id.clone(),
                    profile_name: profile.to_string(),
                    pid,
                    stdin_tx: Some(stdin_tx),
                    cancel_tx: Some(cancel_tx),
                },
            );
        }

        let job_id_clone = job_id.clone();
        let profile_clone = profile.to_string();
        let auth_type_clone = auth_type.clone();
        let service_self = self.clone();

        // Stderr reader task (buffers last 4KB)
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_buf_clone = Arc::clone(&stderr_buf);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line).await {
                if n == 0 {
                    break;
                }
                let mut buf = stderr_buf_clone.lock().await;
                buf.push_str(&line);
                if buf.len() > 4096 {
                    let drain_len = buf.len() - 4096;
                    buf.drain(..drain_len);
                }
                line.clear();
            }
        });

        // Stdin writer task
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(code) = stdin_rx.recv().await {
                info!(job_id = %job_id_clone, "Writing authorization code to child stdin");
                let to_write = format!("{code}\n");
                if let Err(e) = stdin.write_all(to_write.as_bytes()).await {
                    warn!(error = %e, "Failed to write code to AWS CLI stdin");
                }
                let _ = stdin.flush().await;
            }
        });

        // Background supervisor task: stdout parser, wait, timeout, cancel
        let job_id_sup = job_id.clone();
        let store_sup = Arc::clone(&self.job_store);
        let active_profiles_sup = Arc::clone(&self.active_profiles);
        let active_handles_sup = Arc::clone(&self.active_handles);

        tokio::spawn(async move {
            let url_regex = Regex::new(r"https?://\S+").unwrap();
            let code_regex = Regex::new(r"\b([A-Z0-9]{4}-[A-Z0-9]{4})\b").unwrap();

            let mut stdout_buf = Vec::new();
            let mut reader = BufReader::new(stdout);
            let mut chunk = [0u8; 1024];

            let hard_timeout = tokio::time::sleep(Duration::from_secs(600));
            tokio::pin!(hard_timeout);
            tokio::pin!(cancel_rx);

            let mut stdout_done = false;
            let mut child_exited: Option<std::process::ExitStatus> = None;

            loop {
                tokio::select! {
                    res = reader.read(&mut chunk), if !stdout_done => {
                        match res {
                            Ok(0) => {
                                stdout_done = true;
                            }
                            Ok(n) => {
                                stdout_buf.extend_from_slice(&chunk[..n]);
                                let text = String::from_utf8_lossy(&stdout_buf);

                                if let Some(mut current) = store_sup.load_job(&job_id_sup) {
                                    let mut changed = false;

                                    if auth_type_clone == "sso" {
                                        let autofill_regex = Regex::new(r"https?://\S+\?user_code=[A-Z0-9-]+").unwrap();

                                        if let Some(caps) = code_regex.captures(&text) {
                                            if let Some(m) = caps.get(1) {
                                                if current.user_code.as_deref() != Some(m.as_str()) {
                                                    current.user_code = Some(m.as_str().to_string());
                                                    changed = true;
                                                }
                                            }
                                        }

                                        if let Some(m) = autofill_regex.find(&text) {
                                            if current.verification_uri.as_deref() != Some(m.as_str()) {
                                                current.verification_uri = Some(m.as_str().to_string());
                                                changed = true;
                                            }
                                        } else if current.verification_uri.is_none() {
                                            if let Some(m) = url_regex.find(&text) {
                                                current.verification_uri = Some(m.as_str().to_string());
                                                changed = true;
                                            }
                                        }

                                        if current.verification_uri.is_some() && current.state == AwsJobState::Starting {
                                            current.state = AwsJobState::NeedsBrowser;
                                            changed = true;
                                        }
                                    } else {
                                        // auth_type == "remote"
                                        if current.sign_in_url.is_none() {
                                            if let Some(m) = url_regex.find(&text) {
                                                current.sign_in_url = Some(m.as_str().to_string());
                                                changed = true;
                                            }
                                        }
                                        if current.sign_in_url.is_some() && current.state == AwsJobState::Starting {
                                            current.state = AwsJobState::NeedsUserCode;
                                            changed = true;
                                        }
                                    }

                                    if changed {
                                        let _ = store_sup.save_job(&current);
                                    }
                                }
                            }
                            Err(_) => {
                                stdout_done = true;
                            }
                        }
                    }

                    exit_res = child.wait(), if child_exited.is_none() => {
                        match exit_res {
                            Ok(status) => {
                                child_exited = Some(status);
                                break;
                            }
                            Err(e) => {
                                error!(pid, error = %e, "Error waiting on AWS CLI child process");
                                break;
                            }
                        }
                    }

                    _ = &mut hard_timeout => {
                        warn!(pid, job_id = %job_id_sup, "AWS CLI interactive login timed out after 10m");
                        let _ = kill_process_tree(&mut child).await;
                        if let Some(mut current) = store_sup.load_job(&job_id_sup) {
                            current.state = AwsJobState::Expired;
                            current.error_message = Some("Interactive login timed out after 10 minutes".to_string());
                            let _ = store_sup.save_job(&current);
                        }
                        break;
                    }

                    _ = &mut cancel_rx => {
                        info!(pid, job_id = %job_id_sup, "AWS CLI interactive login cancelled by user");
                        let _ = kill_process_tree(&mut child).await;
                        if let Some(mut current) = store_sup.load_job(&job_id_sup) {
                            current.state = AwsJobState::Cancelled;
                            current.error_message = Some("Login cancelled by user".to_string());
                            let _ = store_sup.save_job(&current);
                        }
                        break;
                    }
                }
            }

            // Handle final process exit
            if let Some(status) = child_exited {
                if let Some(mut current) = store_sup.load_job(&job_id_sup) {
                    if status.success() {
                        info!(profile = %profile_clone, "AWS login process exited successfully");
                        // Run test identity to verify
                        let test_res = service_self.test_identity(&profile_clone).await;
                        current.state = AwsJobState::Success;
                        if let Ok(res) = test_res {
                            current.identity = res.identity;
                        }
                    } else {
                        let err_text = stderr_buf.lock().await.clone();
                        warn!(
                            profile = %profile_clone,
                            code = ?status.code(),
                            stderr = %err_text,
                            "AWS login process exited with failure"
                        );
                        current.state = AwsJobState::Failed;
                        current.error_message = Some(if err_text.trim().is_empty() {
                            format!("Login failed with exit status {:?}", status.code())
                        } else {
                            err_text.trim().to_string()
                        });
                    }
                    let _ = store_sup.save_job(&current);
                }
            }

            // Cleanup active locks
            {
                let mut active = active_profiles_sup.lock().await;
                active.remove(&profile_clone);

                let mut handles = active_handles_sup.lock().await;
                handles.remove(&job_id_sup);
            }
        });

        Ok(AwsLoginJobStatus::from(&initial_record))
    }

    /// Retrieve status of a login job.
    pub async fn get_job_status(&self, job_id: &str) -> Result<AwsLoginJobStatus, SystemError> {
        let record = self
            .job_store
            .load_job(job_id)
            .ok_or_else(|| SystemError::NotFound(format!("Login job '{job_id}' not found")))?;
        Ok(AwsLoginJobStatus::from(&record))
    }

    /// Submit authorization code for remote login flow (`aws login --remote`).
    pub async fn submit_code(&self, job_id: &str, code: &str) -> Result<AwsLoginJobStatus, SystemError> {
        let code = code.trim();
        if code.is_empty() {
            return Err(SystemError::BadRequest("Authorization code cannot be empty".to_string()));
        }

        let mut job = self
            .job_store
            .load_job(job_id)
            .ok_or_else(|| SystemError::NotFound(format!("Login job '{job_id}' not found")))?;

        if job.state != AwsJobState::NeedsUserCode && job.state != AwsJobState::Starting {
            return Err(SystemError::BadRequest(format!(
                "Job is in state '{:?}', cannot accept authorization code",
                job.state
            )));
        }

        let handles = self.active_handles.lock().await;
        if let Some(handle) = handles.get(job_id) {
            if let Some(tx) = &handle.stdin_tx {
                tx.send(code.to_string())
                    .await
                    .map_err(|e| SystemError::Internal(format!("Failed to send code to process: {e}")))?;
            }
        } else {
            return Err(SystemError::Conflict("Job process is no longer active".to_string()));
        }

        job.state = AwsJobState::Pending;
        self.job_store.save_job(&job).map_err(SystemError::Internal)?;

        Ok(AwsLoginJobStatus::from(&job))
    }

    /// Cancel a running login job.
    pub async fn cancel_job(&self, job_id: &str) -> Result<AwsLoginJobStatus, SystemError> {
        let mut job = self
            .job_store
            .load_job(job_id)
            .ok_or_else(|| SystemError::NotFound(format!("Login job '{job_id}' not found")))?;

        let mut handles = self.active_handles.lock().await;
        if let Some(mut handle) = handles.remove(job_id) {
            if let Some(cancel_tx) = handle.cancel_tx.take() {
                let _ = cancel_tx.send(());
            }
        }

        // Release profile lock if held
        {
            let mut active = self.active_profiles.lock().await;
            active.retain(|_, id| id != job_id);
        }

        job.state = AwsJobState::Cancelled;
        job.error_message = Some("Cancelled by user".to_string());
        let _ = self.job_store.save_job(&job);

        Ok(AwsLoginJobStatus::from(&job))
    }

    /// Save or update an AWS profile configuration.
    pub async fn save_profile(&self, req: AwsSaveProfileRequest) -> Result<(), SystemError> {
        parser_save_profile(&self.config_path, &self.credentials_path, &req)
            .map_err(SystemError::BadRequest)
    }

    /// Delete an AWS profile configuration.
    pub async fn delete_profile(&self, name: &str) -> Result<(), SystemError> {
        parser_delete_profile(&self.config_path, &self.credentials_path, name)
            .map_err(SystemError::BadRequest)
    }

    /// List all configured SSO sessions from ~/.aws/config.
    pub async fn list_sso_sessions(&self) -> Result<Vec<AwsSsoSessionSummary>, SystemError> {
        Ok(load_all_sso_sessions(&self.config_path))
    }

    /// Save or update an SSO session configuration.
    pub async fn save_sso_session(&self, req: AwsSaveSsoSessionRequest) -> Result<(), SystemError> {
        parser_save_sso_session(&self.config_path, &req).map_err(SystemError::BadRequest)
    }
}
