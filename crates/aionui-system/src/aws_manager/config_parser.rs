use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use aionui_api_types::{
    AwsProfileAuthMethod, AwsProfileSummary, AwsSaveProfileRequest, AwsSaveSsoSessionRequest,
    AwsSsoSessionSummary,
};
use tracing::{info, warn};

/// Default paths helper for AWS configuration.
pub fn default_aws_dir() -> PathBuf {
    dirs::home_dir().map(|h| h.join(".aws")).unwrap_or_else(|| PathBuf::from(".aws"))
}

pub fn default_config_path() -> PathBuf {
    std::env::var_os("AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_aws_dir().join("config"))
}

pub fn default_credentials_path() -> PathBuf {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_aws_dir().join("credentials"))
}

pub fn default_sso_cache_path() -> PathBuf {
    default_aws_dir().join("sso").join("cache")
}

pub fn default_login_cache_path() -> PathBuf {
    std::env::var_os("AWS_LOGIN_CACHE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_aws_dir().join("cli").join("cache"))
}

/// Represents a parsed INI section.
#[derive(Debug, Clone)]
pub struct IniSection {
    pub raw_header: String,
    pub name: String,
    pub section_type: SectionType,
    pub properties: Vec<(String, String)>,
    pub raw_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionType {
    Profile,
    SsoSession,
    LoginSession,
    Other,
}

/// Normalizes a section header to extract the type and clean name.
/// e.g. `profile 'AryaNoble - SFA'` -> (Profile, "AryaNoble - SFA")
/// e.g. `sso-session AryaNoble` -> (SsoSession, "AryaNoble")
/// e.g. `default` -> (Profile, "default")
pub fn parse_section_header(header_content: &str) -> (SectionType, String) {
    let trimmed = header_content.trim();
    if trimmed.eq_ignore_ascii_case("default") {
        return (SectionType::Profile, "default".to_string());
    }

    if let Some(rest) = trimmed.strip_prefix("profile ") {
        return (SectionType::Profile, clean_name(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("profile\t") {
        return (SectionType::Profile, clean_name(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("sso-session ") {
        return (SectionType::SsoSession, clean_name(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("sso-session\t") {
        return (SectionType::SsoSession, clean_name(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("login-session ") {
        return (SectionType::LoginSession, clean_name(rest));
    }
    if let Some(rest) = trimmed.strip_prefix("login-session\t") {
        return (SectionType::LoginSession, clean_name(rest));
    }

    (SectionType::Other, clean_name(trimmed))
}

fn clean_name(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
        || (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse an INI file into structured sections.
pub fn parse_ini_file(content: &str) -> Vec<IniSection> {
    let mut sections = Vec::new();
    let mut current_section: Option<IniSection> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            if let Some(sec) = current_section.take() {
                sections.push(sec);
            }
            let inner = &trimmed[1..trimmed.len() - 1];
            let (section_type, name) = parse_section_header(inner);
            current_section = Some(IniSection {
                raw_header: trimmed.to_string(),
                name,
                section_type,
                properties: Vec::new(),
                raw_lines: Vec::new(),
            });
            continue;
        }

        if let Some(sec) = current_section.as_mut() {
            sec.raw_lines.push(line.to_string());
            if !trimmed.starts_with('#') && !trimmed.starts_with(';') {
                if let Some((k, v)) = trimmed.split_once('=') {
                    sec.properties.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
        }
    }

    if let Some(sec) = current_section {
        sections.push(sec);
    }

    sections
}

/// Mask an AWS Access Key ID, e.g. `AKIAIOSFODNN7EXAMPLE` -> `AKIA...MPLE`.
pub fn mask_access_key_id(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.len() <= 8 {
        "****".to_string()
    } else {
        format!("{}...{}", &trimmed[..4], &trimmed[trimmed.len() - 4..])
    }
}

/// Load and classify all AWS profiles from config and credentials files.
pub fn load_all_profiles(config_path: &Path, creds_path: &Path) -> Vec<AwsProfileSummary> {
    let config_sections = if config_path.exists() {
        match fs::read_to_string(config_path) {
            Ok(c) => parse_ini_file(&c),
            Err(e) => {
                warn!(path = ?config_path, error = %e, "Failed to read AWS config file");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let creds_sections = if creds_path.exists() {
        match fs::read_to_string(creds_path) {
            Ok(c) => parse_ini_file(&c),
            Err(e) => {
                warn!(path = ?creds_path, error = %e, "Failed to read AWS credentials file");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Index sso-session definitions
    let mut sso_sessions: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    for sec in &config_sections {
        if sec.section_type == SectionType::SsoSession {
            let mut start_url = None;
            let mut region = None;
            for (k, v) in &sec.properties {
                if k.eq_ignore_ascii_case("sso_start_url") {
                    start_url = Some(v.clone());
                } else if k.eq_ignore_ascii_case("sso_region") {
                    region = Some(v.clone());
                }
            }
            sso_sessions.insert(sec.name.clone(), (start_url, region));
        }
    }

    // Index credentials by profile name
    let mut credentials_by_profile: HashMap<String, (Option<String>, bool)> = HashMap::new();
    for sec in &creds_sections {
        let name = sec.name.clone();
        let mut masked_key = None;
        let mut has_key = false;
        for (k, v) in &sec.properties {
            if k.eq_ignore_ascii_case("aws_access_key_id") && !v.is_empty() {
                has_key = true;
                masked_key = Some(mask_access_key_id(v));
            }
        }
        credentials_by_profile.insert(name, (masked_key, has_key));
    }

    // Discover all unique profile names from config and credentials
    let mut profile_names = Vec::new();
    let mut profile_config_map: HashMap<String, &IniSection> = HashMap::new();

    for sec in &config_sections {
        if sec.section_type == SectionType::Profile || (sec.section_type == SectionType::Other && sec.name == "default") {
            if !profile_config_map.contains_key(&sec.name) {
                profile_names.push(sec.name.clone());
                profile_config_map.insert(sec.name.clone(), sec);
            }
        }
    }

    for (name, _) in &credentials_by_profile {
        if !profile_config_map.contains_key(name) && !profile_names.contains(name) {
            profile_names.push(name.clone());
        }
    }

    // Sort profile names, with "default" pinned first
    profile_names.sort_by(|a, b| {
        if a == "default" {
            std::cmp::Ordering::Less
        } else if b == "default" {
            std::cmp::Ordering::Greater
        } else {
            a.to_lowercase().cmp(&b.to_lowercase())
        }
    });

    let mut profiles = Vec::new();

    for name in profile_names {
        let config_sec = profile_config_map.get(&name).copied();
        let (masked_key, has_key) = credentials_by_profile.get(&name).cloned().unwrap_or((None, false));

        let mut region = None;
        let mut output = None;
        let mut sso_session = None;
        let mut sso_start_url = None;
        let mut sso_region = None;
        let mut sso_account_id = None;
        let mut sso_role_name = None;
        let mut login_session = None;
        let mut role_arn = None;
        let mut source_profile = None;
        let mut credential_process = None;
        let mut config_has_key = false;
        let mut config_masked_key = None;

        if let Some(sec) = config_sec {
            for (k, v) in &sec.properties {
                match k.to_ascii_lowercase().as_str() {
                    "region" => region = Some(v.clone()),
                    "output" => output = Some(v.clone()),
                    "sso_session" => sso_session = Some(v.clone()),
                    "sso_start_url" => sso_start_url = Some(v.clone()),
                    "sso_region" => sso_region = Some(v.clone()),
                    "sso_account_id" => sso_account_id = Some(v.clone()),
                    "sso_role_name" => sso_role_name = Some(v.clone()),
                    "login_session" => login_session = Some(v.clone()),
                    "role_arn" => role_arn = Some(v.clone()),
                    "source_profile" => source_profile = Some(v.clone()),
                    "credential_process" => credential_process = Some(v.clone()),
                    "aws_access_key_id" => {
                        config_has_key = true;
                        config_masked_key = Some(mask_access_key_id(v));
                    }
                    _ => {}
                }
            }
        }

        // If profile links to an sso-session, resolve session-level start_url and region if missing
        if let Some(sess_name) = &sso_session {
            if let Some((sess_url, sess_reg)) = sso_sessions.get(sess_name) {
                if sso_start_url.is_none() {
                    sso_start_url = sess_url.clone();
                }
                if sso_region.is_none() {
                    sso_region = sess_reg.clone();
                }
            }
        }

        let effective_has_key = has_key || config_has_key;
        let effective_masked_key = masked_key.or(config_masked_key);
        let effective_login_session = match login_session.as_deref() {
            Some("pending") | Some("") => None,
            other => other.map(|s| s.to_string()),
        };

        let auth_method = if sso_session.is_some() || sso_start_url.is_some() || sso_account_id.is_some() {
            AwsProfileAuthMethod::Sso
        } else if role_arn.is_some() || source_profile.is_some() {
            AwsProfileAuthMethod::AssumeRole
        } else if credential_process.is_some() {
            AwsProfileAuthMethod::CredentialProcess
        } else if effective_has_key {
            AwsProfileAuthMethod::StaticKey
        } else {
            // Profile has no keys, no sso, no assume-role, no credential_process.
            // In AWS CLI, profiles with login_session or empty/region-only profiles are Console Login profiles.
            AwsProfileAuthMethod::ConsoleLogin
        };

        profiles.push(AwsProfileSummary {
            name,
            auth_method,
            region,
            output,
            sso_session,
            sso_start_url,
            sso_region,
            sso_account_id,
            sso_role_name,
            login_session: effective_login_session,
            role_arn,
            source_profile,
            has_access_key: effective_has_key,
            masked_access_key_id: effective_masked_key,
            identity: None,
            status: "untested".to_string(),
            last_checked: None,
            error_message: None,
        });
    }

    profiles
}

/// Creates a safe timestamped backup of the target file before any write operation.
pub fn create_backup_copy(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let backup_name = format!("{file_name}.backup.{timestamp}");
    let backup_path = path.with_file_name(backup_name);
    fs::copy(path, &backup_path)?;
    set_secure_permissions(&backup_path);
    info!(source = ?path, backup = ?backup_path, "Created backup copy of AWS config file");
    Ok(Some(backup_path))
}

/// Ensure `0600` permissions on Unix platforms.
pub fn set_secure_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(path, perms);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Atomic write to file ensuring parent directory exists and permissions are 0600.
pub fn write_file_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
    fs::write(&temp_path, content)?;
    set_secure_permissions(&temp_path);
    fs::rename(&temp_path, path)?;
    set_secure_permissions(path);
    Ok(())
}

/// Helper to identify all (start_line_idx, end_line_idx) ranges in an INI file matching a predicate.
/// Each range represents [header_line, next_header_or_eof).
/// Invariant: start < end for every returned range, and ranges are ordered in line index order.
fn find_section_ranges<F>(lines: &[String], mut is_target_section: F) -> Vec<(usize, usize)>
where
    F: FnMut(&str) -> bool,
{
    let mut ranges = Vec::new();
    let mut current_target_start = None;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
            if let Some(start) = current_target_start.take() {
                ranges.push((start, idx));
            }
            let inner = &trimmed[1..trimmed.len() - 1];
            if is_target_section(inner) {
                current_target_start = Some(idx);
            }
        }
    }

    if let Some(start) = current_target_start {
        ranges.push((start, lines.len()));
    }

    ranges
}

/// Replace or append a section in a list of lines.
/// If `ranges` is empty, appends `new_section` at the end (with a separating blank line if needed).
/// If `ranges` has 1 or more entries:
///   - Removes all subsequent duplicate ranges in reverse order (preserving line indices for earlier lines).
///   - Splices `new_section` into the first matching range.
fn replace_or_append_section(
    lines: &mut Vec<String>,
    ranges: &[(usize, usize)],
    new_section: Vec<String>,
) {
    if ranges.is_empty() {
        if !lines.is_empty() && !lines.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.extend(new_section);
    } else {
        // Drain any subsequent duplicate sections in reverse order so line indices remain valid
        for &(start, end) in ranges.iter().skip(1).rev() {
            if start < end && end <= lines.len() {
                lines.drain(start..end);
            }
        }
        // Replace the first matching section with new_section
        let (first_start, first_end) = ranges[0];
        if first_start <= first_end && first_end <= lines.len() {
            lines.splice(first_start..first_end, new_section);
        } else {
            lines.extend(new_section);
        }
    }
}

/// Delete all matching section ranges in reverse order so earlier line indices remain valid.
fn delete_section_ranges(lines: &mut Vec<String>, ranges: &[(usize, usize)]) {
    for &(start, end) in ranges.iter().rev() {
        if start < end && end <= lines.len() {
            lines.drain(start..end);
        }
    }
}

/// Save or update a profile in `config` (and optionally `credentials` if static keys are supplied).
pub fn save_profile(
    config_path: &Path,
    creds_path: &Path,
    req: &AwsSaveProfileRequest,
) -> Result<(), String> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }
    if name.contains('[') || name.contains(']') || name.contains('\n') || name.contains('\r') {
        return Err("Profile name contains invalid characters".to_string());
    }

    create_backup_copy(config_path).map_err(|e| format!("Failed to backup AWS config: {e}"))?;

    let existing_content = if config_path.exists() {
        fs::read_to_string(config_path).map_err(|e| format!("Failed to read config: {e}"))?
    } else {
        String::new()
    };

    // If original_name is provided and differs, handle rename
    let rename_from = req.original_name.as_deref().filter(|n| *n != name);
    let search_name = rename_from.unwrap_or(name);

    let mut lines: Vec<String> = existing_content.lines().map(|s| s.to_string()).collect();

    // Construct new section content
    let mut new_section = Vec::new();
    if name == "default" {
        new_section.push("[default]".to_string());
    } else if name.contains(' ') {
        new_section.push(format!("[profile '{name}']"));
    } else {
        new_section.push(format!("[profile {name}]"));
    }

    if let Some(raw) = &req.raw_config_section {
        for line in raw.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with('[') {
                new_section.push(line.to_string());
            }
        }
    } else {
        if let Some(reg) = &req.region {
            let trimmed = reg.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("region = {trimmed}"));
            }
        }
        if let Some(out) = &req.output {
            let trimmed = out.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("output = {trimmed}"));
            }
        }
        if let Some(sso_sess) = &req.sso_session {
            let trimmed = sso_sess.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("sso_session = {trimmed}"));
            }
        }
        if let Some(sso_url) = &req.sso_start_url {
            let trimmed = sso_url.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("sso_start_url = {trimmed}"));
            }
        }
        if let Some(sso_reg) = &req.sso_region {
            let trimmed = sso_reg.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("sso_region = {trimmed}"));
            }
        }
        if let Some(sso_acc) = &req.sso_account_id {
            let trimmed = sso_acc.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("sso_account_id = {trimmed}"));
            }
        }
        if let Some(sso_role) = &req.sso_role_name {
            let trimmed = sso_role.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("sso_role_name = {trimmed}"));
            }
        }
        if let Some(ls) = &req.login_session {
            let trimmed = ls.trim();
            if !trimmed.is_empty() && trimmed != "pending" {
                new_section.push(format!("login_session = {trimmed}"));
            }
        }
        if let Some(arn) = &req.role_arn {
            let trimmed = arn.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("role_arn = {trimmed}"));
            }
        }
        if let Some(src) = &req.source_profile {
            let trimmed = src.trim();
            if !trimmed.is_empty() {
                new_section.push(format!("source_profile = {trimmed}"));
            }
        }
    }

    let is_target = |inner: &str| {
        let (st, sec_name) = parse_section_header(inner);
        (st == SectionType::Profile || (st == SectionType::Other && sec_name == "default"))
            && sec_name == search_name
    };
    let ranges = find_section_ranges(&lines, is_target);
    replace_or_append_section(&mut lines, &ranges, new_section);

    let updated_config = lines.join("\n") + "\n";
    write_file_atomically(config_path, &updated_config)
        .map_err(|e| format!("Failed to write AWS config: {e}"))?;

    // Handle credentials if static key provided
    if let (Some(access_key), Some(secret_key)) = (&req.aws_access_key_id, &req.aws_secret_access_key) {
        let access_key = access_key.trim();
        let secret_key = secret_key.trim();
        if !access_key.is_empty() && !secret_key.is_empty() {
            create_backup_copy(creds_path)
                .map_err(|e| format!("Failed to backup AWS credentials: {e}"))?;
            let existing_creds = if creds_path.exists() {
                fs::read_to_string(creds_path).map_err(|e| format!("Failed to read credentials: {e}"))?
            } else {
                String::new()
            };

            let mut cred_lines: Vec<String> = existing_creds.lines().map(|s| s.to_string()).collect();
            let is_cred_target = |inner: &str| clean_name(inner) == search_name;
            let c_ranges = find_section_ranges(&cred_lines, is_cred_target);

            let new_cred_sec = vec![
                format!("[{name}]"),
                format!("aws_access_key_id = {access_key}"),
                format!("aws_secret_access_key = {secret_key}"),
            ];

            replace_or_append_section(&mut cred_lines, &c_ranges, new_cred_sec);
            let updated_creds = cred_lines.join("\n") + "\n";
            write_file_atomically(creds_path, &updated_creds)
                .map_err(|e| format!("Failed to write AWS credentials: {e}"))?;
        }
    } else if let Some(old_name) = rename_from {
        // If profile was renamed and had credentials under old_name, rename the credentials section
        if creds_path.exists() {
            let existing_creds = fs::read_to_string(creds_path).map_err(|e| format!("Failed to read credentials: {e}"))?;
            let mut cred_lines: Vec<String> = existing_creds.lines().map(|s| s.to_string()).collect();
            let is_cred_target = |inner: &str| clean_name(inner) == old_name;
            let c_ranges = find_section_ranges(&cred_lines, is_cred_target);
            if !c_ranges.is_empty() {
                create_backup_copy(creds_path).map_err(|e| format!("Failed to backup AWS credentials: {e}"))?;
                for &(start, end) in c_ranges.iter().skip(1).rev() {
                    if start < end && end <= cred_lines.len() {
                        cred_lines.drain(start..end);
                    }
                }
                let (first_start, _) = c_ranges[0];
                if first_start < cred_lines.len() {
                    cred_lines[first_start] = format!("[{name}]");
                }
                let updated_creds = cred_lines.join("\n") + "\n";
                write_file_atomically(creds_path, &updated_creds)
                    .map_err(|e| format!("Failed to update credentials after rename: {e}"))?;
            }
        }
    }

    Ok(())
}

/// Delete a profile section from `config` and `credentials` with backups.
pub fn delete_profile(config_path: &Path, creds_path: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }

    if config_path.exists() {
        create_backup_copy(config_path).map_err(|e| format!("Failed to backup AWS config: {e}"))?;
        let content = fs::read_to_string(config_path).map_err(|e| format!("Failed to read config: {e}"))?;
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        let is_target = |inner: &str| {
            let (st, sec_name) = parse_section_header(inner);
            (st == SectionType::Profile || (st == SectionType::Other && sec_name == "default"))
                && sec_name == name
        };
        let ranges = find_section_ranges(&lines, is_target);
        if !ranges.is_empty() {
            delete_section_ranges(&mut lines, &ranges);
            let updated = lines.join("\n") + "\n";
            write_file_atomically(config_path, &updated)
                .map_err(|e| format!("Failed to update config after delete: {e}"))?;
        }
    }

    if creds_path.exists() {
        create_backup_copy(creds_path).map_err(|e| format!("Failed to backup AWS credentials: {e}"))?;
        let content = fs::read_to_string(creds_path).map_err(|e| format!("Failed to read credentials: {e}"))?;
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        let is_cred_target = |inner: &str| clean_name(inner) == name;
        let ranges = find_section_ranges(&lines, is_cred_target);
        if !ranges.is_empty() {
            delete_section_ranges(&mut lines, &ranges);
            let updated = lines.join("\n") + "\n";
            write_file_atomically(creds_path, &updated)
                .map_err(|e| format!("Failed to update credentials after delete: {e}"))?;
        }
    }

    Ok(())
}

/// Load all SSO session definitions from ~/.aws/config
pub fn load_all_sso_sessions(config_path: &Path) -> Vec<AwsSsoSessionSummary> {
    let config_sections = if config_path.exists() {
        match fs::read_to_string(config_path) {
            Ok(c) => parse_ini_file(&c),
            Err(e) => {
                warn!(path = ?config_path, error = %e, "Failed to read AWS config file");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let mut sessions = Vec::new();
    for sec in &config_sections {
        if sec.section_type == SectionType::SsoSession {
            let mut start_url = String::new();
            let mut region = String::new();
            let mut scopes = None;
            for (k, v) in &sec.properties {
                if k.eq_ignore_ascii_case("sso_start_url") {
                    start_url = v.clone();
                } else if k.eq_ignore_ascii_case("sso_region") {
                    region = v.clone();
                } else if k.eq_ignore_ascii_case("sso_registration_scopes") {
                    scopes = Some(v.clone());
                }
            }
            sessions.push(AwsSsoSessionSummary {
                name: sec.name.clone(),
                sso_start_url: start_url,
                sso_region: region,
                sso_registration_scopes: scopes,
            });
        }
    }
    sessions.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    sessions
}

/// Save or update an [sso-session <name>] section in ~/.aws/config with backup
pub fn save_sso_session(
    config_path: &Path,
    req: &AwsSaveSsoSessionRequest,
) -> Result<(), String> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err("SSO session name cannot be empty".to_string());
    }
    if name.contains('[') || name.contains(']') || name.contains('\n') || name.contains('\r') {
        return Err("SSO session name contains invalid characters".to_string());
    }
    let start_url = req.sso_start_url.trim();
    if start_url.is_empty() {
        return Err("SSO start URL cannot be empty".to_string());
    }
    let region = req.sso_region.trim();
    if region.is_empty() {
        return Err("SSO region cannot be empty".to_string());
    }

    create_backup_copy(config_path).map_err(|e| format!("Failed to backup AWS config: {e}"))?;

    let existing_content = if config_path.exists() {
        fs::read_to_string(config_path).map_err(|e| format!("Failed to read config: {e}"))?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = existing_content.lines().map(|s| s.to_string()).collect();

    let mut new_section = Vec::new();
    if name.contains(' ') {
        new_section.push(format!("[sso-session '{name}']"));
    } else {
        new_section.push(format!("[sso-session {name}]"));
    }
    new_section.push(format!("sso_start_url = {start_url}"));
    new_section.push(format!("sso_region = {region}"));
    let scopes = req
        .sso_registration_scopes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("sso:account:access");
    new_section.push(format!("sso_registration_scopes = {scopes}"));

    let is_target = |inner: &str| {
        let (st, sec_name) = parse_section_header(inner);
        st == SectionType::SsoSession && sec_name == name
    };
    let ranges = find_section_ranges(&lines, is_target);
    replace_or_append_section(&mut lines, &ranges, new_section);

    let updated_config = lines.join("\n") + "\n";
    write_file_atomically(config_path, &updated_config)
        .map_err(|e| format!("Failed to write AWS config: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_section_header() {
        assert_eq!(parse_section_header("default"), (SectionType::Profile, "default".to_string()));
        assert_eq!(
            parse_section_header("profile 'Sadewa Federation'"),
            (SectionType::Profile, "Sadewa Federation".to_string())
        );
        assert_eq!(
            parse_section_header("profile \"My Profile\""),
            (SectionType::Profile, "My Profile".to_string())
        );
        assert_eq!(
            parse_section_header("profile sandbox-2024"),
            (SectionType::Profile, "sandbox-2024".to_string())
        );
        assert_eq!(
            parse_section_header("sso-session AryaNoble"),
            (SectionType::SsoSession, "AryaNoble".to_string())
        );
        assert_eq!(
            parse_section_header("login-session 'session-1'"),
            (SectionType::LoginSession, "session-1".to_string())
        );
    }

    #[test]
    fn test_mask_access_key() {
        assert_eq!(mask_access_key_id("AKIAIOSFODNN7EXAMPLE"), "AKIA...MPLE");
        assert_eq!(mask_access_key_id("SHORT"), "****");
    }

    #[test]
    fn test_load_all_profiles_and_classification() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config");
        let creds_path = dir.path().join("credentials");

        let config_content = r#"
[default]
region = ap-southeast-1
output = json

[profile 'AryaNoble - SFA']
sso_session = AryaNoble
sso_account_id = 546158667544
sso_role_name = ics-ms-rw
region = ap-southeast-3

[sso-session AryaNoble]
sso_start_url = https://aryanoble-sso.awsapps.com/start/#/
sso_region = ap-southeast-3

[profile Fresnel]
login_session = arn:aws:sts::466650104955:assumed-role/ics-awsc-msw/garma.rianto
region = ap-southeast-3

[profile sandbox3-2024]
source_profile = _sandbox3-2024-base
role_arn = arn:aws:iam::339712808680:role/garma.rianto-Role

[profile static-account]
region = us-east-1
"#;

        let creds_content = r#"
[static-account]
aws_access_key_id = AKIA1234567890EXAMPLE
aws_secret_access_key = SECRET_ACCESS_KEY_THAT_MUST_NEVER_LEAK
"#;

        fs::write(&config_path, config_content).unwrap();
        fs::write(&creds_path, creds_content).unwrap();

        let profiles = load_all_profiles(&config_path, &creds_path);
        assert_eq!(profiles.len(), 5);

        // default is first
        assert_eq!(profiles[0].name, "default");
        assert_eq!(profiles[0].auth_method, AwsProfileAuthMethod::ConsoleLogin);

        let sfa = profiles.iter().find(|p| p.name == "AryaNoble - SFA").unwrap();
        assert_eq!(sfa.auth_method, AwsProfileAuthMethod::Sso);
        assert_eq!(sfa.sso_start_url.as_deref(), Some("https://aryanoble-sso.awsapps.com/start/#/"));
        assert_eq!(sfa.sso_region.as_deref(), Some("ap-southeast-3"));

        let fresnel = profiles.iter().find(|p| p.name == "Fresnel").unwrap();
        assert_eq!(fresnel.auth_method, AwsProfileAuthMethod::ConsoleLogin);

        let sandbox = profiles.iter().find(|p| p.name == "sandbox3-2024").unwrap();
        assert_eq!(sandbox.auth_method, AwsProfileAuthMethod::AssumeRole);

        let static_p = profiles.iter().find(|p| p.name == "static-account").unwrap();
        assert_eq!(static_p.auth_method, AwsProfileAuthMethod::StaticKey);
        assert!(static_p.has_access_key);
        assert_eq!(static_p.masked_access_key_id.as_deref(), Some("AKIA...MPLE"));
    }

    #[test]
    fn test_save_and_delete_profile_with_backup() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config");
        let creds_path = dir.path().join("credentials");

        fs::write(&config_path, "[profile initial]\nregion = us-west-2\n").unwrap();

        let req = AwsSaveProfileRequest {
            name: "test-new-profile".to_string(),
            original_name: None,
            region: Some("ap-southeast-1".to_string()),
            output: Some("json".to_string()),
            auth_method: Some("sso".to_string()),
            sso_session: Some("my-session".to_string()),
            sso_start_url: Some("https://test.awsapps.com/start".to_string()),
            sso_region: Some("ap-southeast-1".to_string()),
            sso_account_id: Some("123456789012".to_string()),
            sso_role_name: Some("AdminRole".to_string()),
            login_session: None,
            role_arn: None,
            source_profile: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            raw_config_section: None,
        };

        save_profile(&config_path, &creds_path, &req).unwrap();

        let profiles = load_all_profiles(&config_path, &creds_path);
        let created = profiles.iter().find(|p| p.name == "test-new-profile").unwrap();
        assert_eq!(created.auth_method, AwsProfileAuthMethod::Sso);
        assert_eq!(created.region.as_deref(), Some("ap-southeast-1"));

        // Delete profile
        delete_profile(&config_path, &creds_path, "test-new-profile").unwrap();
        let profiles_after = load_all_profiles(&config_path, &creds_path);
        assert!(profiles_after.iter().find(|p| p.name == "test-new-profile").is_none());
        assert!(profiles_after.iter().find(|p| p.name == "initial").is_some());
    }

    #[test]
    fn test_save_console_login_profile_without_login_session() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config");
        let creds_path = dir.path().join("credentials");

        fs::write(&config_path, "").unwrap();

        let req = AwsSaveProfileRequest {
            name: "dev-console".to_string(),
            original_name: None,
            region: Some("ap-southeast-1".to_string()),
            output: Some("json".to_string()),
            auth_method: Some("console_login".to_string()),
            sso_session: None,
            sso_start_url: None,
            sso_region: None,
            sso_account_id: None,
            sso_role_name: None,
            login_session: None,
            role_arn: None,
            source_profile: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            raw_config_section: None,
        };

        save_profile(&config_path, &creds_path, &req).unwrap();

        // Verify config content does NOT contain "login_session = pending" or any login_session
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(!content.contains("login_session"));

        // Verify profile loaded is classified as ConsoleLogin with None login_session
        let profiles = load_all_profiles(&config_path, &creds_path);
        let p = profiles.iter().find(|p| p.name == "dev-console").unwrap();
        assert_eq!(p.auth_method, AwsProfileAuthMethod::ConsoleLogin);
        assert_eq!(p.login_session, None);
    }

    #[test]
    fn test_save_and_rename_profile_with_duplicate_sections() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config");
        let creds_path = dir.path().join("credentials");

        let initial_config = r#"
[profile 'Arista - Marketing']
login_session = arn:aws:iam::717271747993:user/arista-webmarketing-admin
region = ap-southeast-1

[profile 'other-profile']
region = us-west-2

[profile Arista - Marketing]
login_session = arn:aws:iam::717271747993:user/arista-webmarketing-admin
"#;
        fs::write(&config_path, initial_config).unwrap();

        // 1. Rename 'Arista - Marketing' to 'Arista - WebMarketing'
        let req = AwsSaveProfileRequest {
            name: "Arista - WebMarketing".to_string(),
            original_name: Some("Arista - Marketing".to_string()),
            region: Some("ap-southeast-1".to_string()),
            output: Some("json".to_string()),
            auth_method: Some("console_login".to_string()),
            sso_session: None,
            sso_start_url: None,
            sso_region: None,
            sso_account_id: None,
            sso_role_name: None,
            login_session: Some("arn:aws:iam::717271747993:user/arista-webmarketing-admin".to_string()),
            role_arn: None,
            source_profile: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            raw_config_section: None,
        };

        // This must not panic and must succeed
        save_profile(&config_path, &creds_path, &req).unwrap();

        let profiles = load_all_profiles(&config_path, &creds_path);
        // Original name must be gone
        assert!(profiles.iter().find(|p| p.name == "Arista - Marketing").is_none());
        // New name must exist and only once
        let matching: Vec<_> = profiles.iter().filter(|p| p.name == "Arista - WebMarketing").collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].region.as_deref(), Some("ap-southeast-1"));
        assert_eq!(matching[0].auth_method, AwsProfileAuthMethod::ConsoleLogin);
        // Other profile must remain intact
        assert!(profiles.iter().find(|p| p.name == "other-profile").is_some());

        // Raw config check: no duplicate section headers
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(!content.contains("Arista - Marketing']"));
        assert!(!content.contains("Arista - Marketing]"));
        assert!(content.contains("[profile 'Arista - WebMarketing']"));
    }

    #[test]
    fn test_delete_profile_with_duplicate_sections() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config");
        let creds_path = dir.path().join("credentials");

        let initial_config = r#"
[profile 'Arista - Marketing']
region = ap-southeast-1

[profile other]
region = us-east-1

[profile Arista - Marketing]
region = ap-southeast-1
"#;
        fs::write(&config_path, initial_config).unwrap();

        delete_profile(&config_path, &creds_path, "Arista - Marketing").unwrap();

        let profiles = load_all_profiles(&config_path, &creds_path);
        assert!(profiles.iter().find(|p| p.name == "Arista - Marketing").is_none());
        assert!(profiles.iter().find(|p| p.name == "other").is_some());

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(!content.contains("Arista - Marketing"));
        assert!(content.contains("[profile other]"));
    }
}
