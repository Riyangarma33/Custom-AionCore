use std::time::Duration;

use aionui_api_types::ModelInfo;
use serde::Deserialize;
use tracing::warn;

use crate::error::SystemError;

use super::FetchConfig;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Dispatch to the appropriate platform-specific fetcher.
pub(crate) async fn fetch_for_platform(
    client: &reqwest::Client,
    config: &FetchConfig,
) -> Result<Vec<ModelInfo>, SystemError> {
    match config.platform.as_str() {
        "anthropic" | "claude" => fetch_anthropic(client, &config.base_url, &config.api_key).await,
        "gemini" => fetch_gemini(client, &config.base_url, &config.api_key).await,
        "bedrock" => fetch_bedrock(config).await,
        "vertex-ai" => Ok(vertex_ai_models()),
        "new-api" => fetch_new_api(client, &config.base_url, &config.api_key).await,
        "minimax" => Ok(minimax_models()),
        "dashscope-coding" => fetch_dashscope_coding(client, &config.base_url, &config.api_key).await,
        _ => fetch_openai_compatible(client, &config.base_url, &config.api_key).await,
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible (default)
// ---------------------------------------------------------------------------

/// Response shape for OpenAI `/models` endpoint.
#[derive(Deserialize)]
struct OpenAiModelsResponse {
    #[serde(default)]
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModelCapabilities {
    #[serde(rename = "contextWindow", default)]
    context_window: Option<usize>,
    #[serde(rename = "maxOutput", default)]
    max_output: Option<usize>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    max_context_tokens: Option<usize>,
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    max_completion_tokens: Option<usize>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    capabilities: Option<OpenAiModelCapabilities>,
}

/// Fetch models from an OpenAI-compatible `/models` endpoint.
pub(super) async fn fetch_openai_compatible(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<ModelInfo>, SystemError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let first_key = api_key.lines().next().unwrap_or(api_key).trim();
    let mut req = client.get(&url).timeout(REQUEST_TIMEOUT);
    if !first_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {first_key}"));
    }
    let resp = req.send().await.map_err(|e| remote_error(&e))?;

    check_response_status(&resp)?;

    let body: OpenAiModelsResponse = resp
        .json()
        .await
        .map_err(|e| SystemError::BadGateway(format!("Failed to parse models response: {e}")))?;

    Ok(body
        .data
        .into_iter()
        .map(|m| {
            let context_length = m
                .context_length
                .or(m.context_window)
                .or(m.max_context_tokens)
                .or_else(|| m.capabilities.as_ref().and_then(|c| c.context_window));
            let max_completion_tokens = m
                .max_completion_tokens
                .or(m.max_output_tokens)
                .or_else(|| m.capabilities.as_ref().and_then(|c| c.max_output));

            if context_length.is_some() || max_completion_tokens.is_some() || m.name.is_some() {
                ModelInfo::Named {
                    id: m.id,
                    name: m.name,
                    context_length,
                    max_completion_tokens,
                }
            } else {
                ModelInfo::Id(m.id)
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Anthropic
// ---------------------------------------------------------------------------

/// Response shape for Anthropic `/v1/models`.
#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModel>,
}

#[derive(Deserialize)]
struct AnthropicModel {
    id: String,
}

const ANTHROPIC_FALLBACK_MODELS: &[&str] = &[
    "claude-sonnet-4-20250514",
    "claude-opus-4-20250514",
    "claude-3-7-sonnet-20250219",
];

async fn fetch_anthropic(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<ModelInfo>, SystemError> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let result = client
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let body: AnthropicModelsResponse = resp
                .json()
                .await
                .map_err(|e| SystemError::BadGateway(format!("Failed to parse Anthropic response: {e}")))?;
            Ok(body.data.into_iter().map(|m| ModelInfo::Id(m.id)).collect())
        }
        Ok(resp) => {
            warn!(
                status = %resp.status(),
                "Anthropic models API failed, using fallback list"
            );
            Ok(fallback_models(ANTHROPIC_FALLBACK_MODELS))
        }
        Err(e) => {
            warn!(error = %e, "Anthropic models API unreachable, using fallback list");
            Ok(fallback_models(ANTHROPIC_FALLBACK_MODELS))
        }
    }
}

// ---------------------------------------------------------------------------
// Gemini
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GeminiModelsResponse {
    models: Vec<GeminiModel>,
}

#[derive(Deserialize)]
struct GeminiModel {
    name: String,
}

const GEMINI_FALLBACK_MODELS: &[&str] = &["gemini-2.5-pro", "gemini-2.5-flash"];

async fn fetch_gemini(client: &reqwest::Client, base_url: &str, api_key: &str) -> Result<Vec<ModelInfo>, SystemError> {
    let url = format!("{}/v1beta/models?key={api_key}", base_url.trim_end_matches('/'));
    let result = client.get(&url).timeout(REQUEST_TIMEOUT).send().await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            let body: GeminiModelsResponse = resp
                .json()
                .await
                .map_err(|e| SystemError::BadGateway(format!("Failed to parse Gemini response: {e}")))?;
            let models = body
                .models
                .into_iter()
                .map(|m| {
                    // Strip "models/" prefix: "models/gemini-2.5-pro" -> "gemini-2.5-pro"
                    let id = m.name.strip_prefix("models/").unwrap_or(&m.name).to_owned();
                    ModelInfo::Id(id)
                })
                .collect();
            Ok(models)
        }
        Ok(resp) => {
            warn!(
                status = %resp.status(),
                "Gemini models API failed, using fallback list"
            );
            Ok(fallback_models(GEMINI_FALLBACK_MODELS))
        }
        Err(e) => {
            warn!(error = %e, "Gemini models API unreachable, using fallback list");
            Ok(fallback_models(GEMINI_FALLBACK_MODELS))
        }
    }
}

// ---------------------------------------------------------------------------
// Bedrock (AWS SDK)
// ---------------------------------------------------------------------------

async fn fetch_bedrock(config: &FetchConfig) -> Result<Vec<ModelInfo>, SystemError> {
    let bedrock_cfg = config
        .bedrock_config
        .as_ref()
        .ok_or_else(|| SystemError::BadRequest("Bedrock requires bedrockConfig".into()))?;

    let region = aws_sdk_bedrock::config::Region::new(bedrock_cfg.region.clone());

    let sdk_config = match bedrock_cfg.auth_method {
        aionui_api_types::BedrockAuthMethod::AccessKey => {
            let key_id = bedrock_cfg
                .access_key_id
                .as_deref()
                .ok_or_else(|| SystemError::BadRequest("accessKeyId is required".into()))?;
            let secret = bedrock_cfg
                .secret_access_key
                .as_deref()
                .ok_or_else(|| SystemError::BadRequest("secretAccessKey is required".into()))?;

            let creds = aws_sdk_bedrock::config::Credentials::new(
                key_id, secret, None, // session token
                None, // expiry
                "aionui",
            );
            aws_sdk_bedrock::Config::builder()
                .region(region)
                .credentials_provider(creds)
                .build()
        }
        aionui_api_types::BedrockAuthMethod::Profile => {
            let profile = bedrock_cfg.profile.as_deref().unwrap_or("default");
            let aws_cfg = aws_config::from_env()
                .profile_name(profile)
                .region(aws_config::Region::new(bedrock_cfg.region.clone()))
                .load()
                .await;
            aws_sdk_bedrock::Config::new(&aws_cfg)
        }
    };

    let client = aws_sdk_bedrock::Client::from_conf(sdk_config);
    let resp = client
        .list_inference_profiles()
        .send()
        .await
        .map_err(|e| SystemError::BadGateway(format!("Bedrock API error: {e}")))?;

    let profiles = resp.inference_profile_summaries();
    // Filter to only anthropic.claude models per API Spec
    let models: Vec<ModelInfo> = profiles
        .iter()
        .filter(|p| p.inference_profile_id().starts_with("anthropic.claude"))
        .map(|p| ModelInfo::Id(p.inference_profile_id().to_string()))
        .collect();

    Ok(models)
}

// ---------------------------------------------------------------------------
// Hardcoded platforms
// ---------------------------------------------------------------------------

fn vertex_ai_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo::Id("gemini-2.5-pro".into()),
        ModelInfo::Id("gemini-2.5-flash".into()),
    ]
}

fn minimax_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo::Id("MiniMax-Text-01".into()),
        ModelInfo::Id("abab6.5s-chat".into()),
        ModelInfo::Id("abab6.5-chat".into()),
    ]
}

// ---------------------------------------------------------------------------
// new-api (OpenAI-compatible with /v1 enforcement)
// ---------------------------------------------------------------------------

async fn fetch_new_api(client: &reqwest::Client, base_url: &str, api_key: &str) -> Result<Vec<ModelInfo>, SystemError> {
    let normalized = ensure_v1_path(base_url);
    fetch_openai_compatible(client, &normalized, api_key).await
}

/// Ensure the URL path ends with `/v1`.
fn ensure_v1_path(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

// ---------------------------------------------------------------------------
// dashscope-coding (hardcoded + key validation)
// ---------------------------------------------------------------------------

const DASHSCOPE_MODELS: &[&str] = &["qwen-coder-plus", "qwen-coder-turbo"];

async fn fetch_dashscope_coding(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<ModelInfo>, SystemError> {
    // Validate key by sending a minimal chat completion request
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": DASHSCOPE_MODELS[0],
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| remote_error(&e))?;

    if resp.status().is_client_error() {
        return Err(SystemError::BadGateway(format!(
            "Dashscope API key validation failed: {}",
            resp.status()
        )));
    }

    Ok(fallback_models(DASHSCOPE_MODELS))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn fallback_models(ids: &[&str]) -> Vec<ModelInfo> {
    ids.iter().map(|id| ModelInfo::Id((*id).to_string())).collect()
}

fn check_response_status(resp: &reqwest::Response) -> Result<(), SystemError> {
    if resp.status().is_success() {
        return Ok(());
    }
    Err(SystemError::BadGateway(format!(
        "Remote API returned {}",
        resp.status()
    )))
}

fn remote_error(e: &reqwest::Error) -> SystemError {
    if e.is_timeout() {
        SystemError::Timeout("Remote API request timed out".into())
    } else {
        SystemError::BadGateway(format!("Remote API request failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_v1_path_already_present() {
        assert_eq!(
            ensure_v1_path("https://api.example.com/v1"),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn ensure_v1_path_missing() {
        assert_eq!(ensure_v1_path("https://api.example.com"), "https://api.example.com/v1");
    }

    #[test]
    fn ensure_v1_path_trailing_slash() {
        assert_eq!(ensure_v1_path("https://api.example.com/"), "https://api.example.com/v1");
    }

    #[test]
    fn ensure_v1_path_with_v1_and_trailing_slash() {
        assert_eq!(
            ensure_v1_path("https://api.example.com/v1/"),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn vertex_ai_returns_expected_models() {
        let models = vertex_ai_models();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0], ModelInfo::Id("gemini-2.5-pro".into()));
        assert_eq!(models[1], ModelInfo::Id("gemini-2.5-flash".into()));
    }

    #[test]
    fn minimax_returns_expected_models() {
        let models = minimax_models();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0], ModelInfo::Id("MiniMax-Text-01".into()));
    }

    #[test]
    fn fallback_models_builds_model_info_list() {
        let models = fallback_models(&["a", "b", "c"]);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0], ModelInfo::Id("a".into()));
    }

    #[test]
    fn openai_models_response_deserializes_context_length_and_capabilities() {
        let raw = serde_json::json!({
            "data": [
                {
                    "id": "ag/gemini-3.8-flash-high",
                    "context_length": 1048576,
                    "max_completion_tokens": 65536
                },
                {
                    "id": "cx/gpt-5.6-sol",
                    "capabilities": {
                        "contextWindow": 372000,
                        "maxOutput": 32768
                    }
                },
                {
                    "id": "simple-model"
                }
            ]
        });

        let parsed: OpenAiModelsResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.data.len(), 3);

        let m0 = &parsed.data[0];
        assert_eq!(m0.id, "ag/gemini-3.8-flash-high");
        assert_eq!(m0.context_length, Some(1048576));
        assert_eq!(m0.max_completion_tokens, Some(65536));

        let m1 = &parsed.data[1];
        assert_eq!(m1.id, "cx/gpt-5.6-sol");
        assert_eq!(
            m1.capabilities.as_ref().and_then(|c| c.context_window),
            Some(372000)
        );
        assert_eq!(
            m1.capabilities.as_ref().and_then(|c| c.max_output),
            Some(32768)
        );

        let m2 = &parsed.data[2];
        assert_eq!(m2.id, "simple-model");
        assert_eq!(m2.context_length, None);
    }
}
