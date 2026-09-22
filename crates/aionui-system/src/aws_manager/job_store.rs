use std::fs;
use std::path::{Path, PathBuf};
use aionui_api_types::{AwsCallerIdentity, AwsJobState, AwsLoginJobStatus};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Internal persisted job record stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsLoginJobRecord {
    pub job_id: String,
    pub profile_name: String,
    pub auth_type: String, // "sso" or "remote"
    pub pid: Option<u32>,
    pub state: AwsJobState,
    pub started_at: i64,
    pub expires_at: i64,
    pub verification_uri: Option<String>,
    pub user_code: Option<String>,
    pub sign_in_url: Option<String>,
    pub error_message: Option<String>,
    pub identity: Option<AwsCallerIdentity>,
}

impl From<&AwsLoginJobRecord> for AwsLoginJobStatus {
    fn from(record: &AwsLoginJobRecord) -> Self {
        Self {
            job_id: record.job_id.clone(),
            profile_name: record.profile_name.clone(),
            auth_type: record.auth_type.clone(),
            state: record.state,
            started_at: record.started_at,
            expires_at: record.expires_at,
            verification_uri: record.verification_uri.clone(),
            user_code: record.user_code.clone(),
            sign_in_url: record.sign_in_url.clone(),
            error_message: record.error_message.clone(),
            identity: record.identity.clone(),
        }
    }
}

/// Disk-backed store for interactive AWS login jobs.
#[derive(Clone)]
pub struct AwsJobStore {
    storage_dir: PathBuf,
}

impl AwsJobStore {
    pub fn new(work_dir: &Path) -> Self {
        let storage_dir = work_dir.join("aws_jobs");
        if let Err(e) = fs::create_dir_all(&storage_dir) {
            warn!(path = ?storage_dir, error = %e, "Failed to create aws_jobs storage directory");
        }
        Self { storage_dir }
    }

    fn job_path(&self, job_id: &str) -> PathBuf {
        self.storage_dir.join(format!("{job_id}.json"))
    }

    /// Save or update a job record atomically on disk.
    pub fn save_job(&self, job: &AwsLoginJobRecord) -> Result<(), String> {
        let path = self.job_path(&job.job_id);
        let content = serde_json::to_string_pretty(job)
            .map_err(|e| format!("Failed to serialize job record: {e}"))?;

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let temp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
        fs::write(&temp_path, content)
            .map_err(|e| format!("Failed to write temp job file: {e}"))?;
        fs::rename(&temp_path, &path)
            .map_err(|e| format!("Failed to atomically rename job file: {e}"))?;

        Ok(())
    }

    /// Load a specific job record by ID.
    pub fn load_job(&self, job_id: &str) -> Option<AwsLoginJobRecord> {
        let path = self.job_path(job_id);
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Load all persisted job records.
    pub fn load_all_jobs(&self) -> Vec<AwsLoginJobRecord> {
        let mut results = Vec::new();
        let entries = match fs::read_dir(&self.storage_dir) {
            Ok(e) => e,
            Err(_) => return results,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<AwsLoginJobRecord>(&content) {
                        results.push(record);
                    }
                }
            }
        }

        results.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        results
    }

    /// Startup reconciliation pass:
    /// Scans persisted jobs. Any job that was in-flight (`Starting`, `NeedsBrowser`,
    /// `NeedsUserCode`, `Pending`) when the service was restarted is examined:
    /// - If its PID is still running, it is killed with `kill_pid_tree` to avoid process leaks.
    /// - Marked as `Failed` with a clear explanation.
    /// - Old completed jobs (>24h) are pruned.
    pub async fn reconcile_startup_jobs(&self) -> usize {
        let all_jobs = self.load_all_jobs();
        let mut reconciled = 0;
        let now = chrono::Utc::now().timestamp();
        let day_ago = now - 86400;

        for mut job in all_jobs {
            // Prune jobs older than 24 hours
            if job.started_at < day_ago {
                let _ = fs::remove_file(self.job_path(&job.job_id));
                continue;
            }

            let is_in_flight = matches!(
                job.state,
                AwsJobState::Starting
                    | AwsJobState::NeedsBrowser
                    | AwsJobState::NeedsUserCode
                    | AwsJobState::Pending
            );

            if is_in_flight {
                if let Some(pid) = job.pid {
                    if aionui_runtime::is_pid_alive(pid) {
                        info!(
                            job_id = %job.job_id,
                            pid,
                            profile = %job.profile_name,
                            "Terminating orphaned AWS CLI login process after service restart"
                        );
                        let _ = aionui_runtime::kill_pid_tree(pid).await;
                        job.error_message = Some("Process was orphaned during service restart and was reaped.".to_string());
                    } else {
                        job.error_message = Some("Service was restarted while login was in progress.".to_string());
                    }
                } else {
                    job.error_message = Some("Service was restarted while login was in progress.".to_string());
                }

                job.state = AwsJobState::Failed;
                let _ = self.save_job(&job);
                reconciled += 1;
            }
        }

        if reconciled > 0 {
            info!(count = reconciled, "AWS CLI interactive login startup reconciliation complete");
        }
        reconciled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_save_and_load_job() {
        let dir = tempdir().unwrap();
        let store = AwsJobStore::new(dir.path());

        let record = AwsLoginJobRecord {
            job_id: "test-job-123".to_string(),
            profile_name: "dev-profile".to_string(),
            auth_type: "sso".to_string(),
            pid: Some(12345),
            state: AwsJobState::NeedsBrowser,
            started_at: 1000,
            expires_at: 1600,
            verification_uri: Some("https://device.sso.aws/".to_string()),
            user_code: Some("ABCD-1234".to_string()),
            sign_in_url: None,
            error_message: None,
            identity: None,
        };

        store.save_job(&record).unwrap();

        let loaded = store.load_job("test-job-123").unwrap();
        assert_eq!(loaded.profile_name, "dev-profile");
        assert_eq!(loaded.state, AwsJobState::NeedsBrowser);
        assert_eq!(loaded.user_code.as_deref(), Some("ABCD-1234"));

        // Test reconciliation
        let reconciled = store.reconcile_startup_jobs().await;
        assert_eq!(reconciled, 1);

        let after = store.load_job("test-job-123").unwrap();
        assert_eq!(after.state, AwsJobState::Failed);
    }
}
