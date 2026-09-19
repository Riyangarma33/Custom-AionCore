//! Dynamic model context window resolver with in-memory caching and heuristic fallback.
//!
//! Resolves model context window capacity using:
//! 1. In-memory TTL cache (shared across turns and sessions).
//! 2. Remote provider `/models` discovery (e.g., 9router `context_length` / `capabilities.contextWindow`).
//! 3. Prefix-stripped heuristic fallback catalog rules (e.g., stripping `ag/`, `cx/`, `kr/` -> Gemini/Claude/GPT rules).

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Deserialize;
use tracing::{debug, info};

/// Cache TTL: 10 minutes.
const CACHE_TTL: Duration = Duration::from_secs(600);

/// HTTP timeout for remote `/models` endpoint resolution.
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct CacheEntry {
    context_window: usize,
    expires_at: Instant,
}

static CONTEXT_CACHE: LazyLock<DashMap<String, CacheEntry>> = LazyLock::new(DashMap::new);

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(RESOLUTION_TIMEOUT)
        .build()
        .unwrap_or_default()
});

fn cache_key(base_url: &str, model_id: &str) -> String {
    let normalized_url = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    format!("{normalized_url}:{}", model_id.trim().to_ascii_lowercase())
}

/// Strip router and provider prefixes from a model ID (e.g. `ag/gemini-3.8-flash-high` -> `gemini-3.8-flash-high`).
pub fn strip_model_prefix(model: &str) -> &str {
    if let Some((_prefix, rest)) = model.split_once('/') {
        rest
    } else {
        model
    }
}

/// Look up cached context window for a given base_url and model_id.
pub fn get_cached_context_window(base_url: &str, model_id: &str) -> Option<usize> {
    let now = Instant::now();
    let key = cache_key(base_url, model_id);

    if let Some(entry) = CONTEXT_CACHE.get(&key) {
        if entry.expires_at > now {
            return Some(entry.context_window);
        }
    }

    let stripped = strip_model_prefix(model_id);
    if stripped != model_id {
        let stripped_key = cache_key(base_url, stripped);
        if let Some(entry) = CONTEXT_CACHE.get(&stripped_key) {
            if entry.expires_at > now {
                return Some(entry.context_window);
            }
        }
    }

    None
}

/// Store a context window in cache.
pub fn insert_cached_context_window(base_url: &str, model_id: &str, context_window: usize) {
    let expires_at = Instant::now() + CACHE_TTL;
    let key = cache_key(base_url, model_id);
    CONTEXT_CACHE.insert(
        key,
        CacheEntry {
            context_window,
            expires_at,
        },
    );

    let stripped = strip_model_prefix(model_id);
    if stripped != model_id {
        let stripped_key = cache_key(base_url, stripped);
        CONTEXT_CACHE.insert(
            stripped_key,
            CacheEntry {
                context_window,
                expires_at,
            },
        );
    }
}

/// Clear cache (primarily for tests).
#[allow(dead_code)]
pub fn clear_context_cache() {
    CONTEXT_CACHE.clear();
}

#[derive(Deserialize)]
struct RemoteModelsResponse {
    #[serde(default)]
    data: Vec<RemoteModelEntry>,
}

#[derive(Deserialize)]
struct RemoteModelCapabilities {
    #[serde(rename = "contextWindow", default)]
    context_window: Option<usize>,
}

#[derive(Deserialize)]
struct RemoteModelEntry {
    id: String,
    #[serde(default)]
    context_length: Option<usize>,
    #[serde(default)]
    max_context_tokens: Option<usize>,
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    capabilities: Option<RemoteModelCapabilities>,
}

/// Normalize base_url to its models discovery endpoint (e.g. `http://host:port/v1/models`).
pub fn normalize_models_endpoint(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return None;
    }

    let stripped = if let Some(p) = trimmed.strip_suffix("/chat/completions") {
        p
    } else if let Some(p) = trimmed.strip_suffix("/responses") {
        p
    } else if let Some(p) = trimmed.strip_suffix("/v1/messages") {
        p
    } else {
        trimmed
    };

    if stripped.ends_with("/models") {
        Some(stripped.to_owned())
    } else {
        Some(format!("{stripped}/models"))
    }
}

/// Prefix-stripped heuristic context window catalog rules (Tier 3 fallback).
pub fn resolve_heuristic_context_window(model_id: &str) -> Option<usize> {
    let stripped = strip_model_prefix(model_id);
    let lower = stripped.to_ascii_lowercase();

    // 1. Gemini Family
    if lower.contains("gemini") {
        if lower.contains("1.5-pro") || lower.contains("2.0-pro") || lower.contains("2.5-pro") {
            return Some(2_000_000);
        }
        if lower.contains("3.8-flash") || lower.contains("3.8") {
            return Some(1_048_576);
        }
        if lower.contains("1.5-flash") || lower.contains("2.0-flash") || lower.contains("2.5-flash") {
            return Some(1_000_000);
        }
        return Some(1_000_000);
    }

    // 2. GPT-5.6 Family (9router custom)
    if lower.starts_with("gpt-5.6-sol") || lower.contains("5.6-sol") {
        return Some(372_000);
    }
    if lower.starts_with("gpt-5.6-terra") || lower.contains("5.6-terra") {
        return Some(272_000);
    }
    if lower.starts_with("gpt-5.6-luna") || lower.contains("5.6-luna") {
        return Some(128_000);
    }
    if lower.starts_with("gpt-5.6") {
        return Some(200_000);
    }

    // 3. Claude Family
    if lower.contains("claude") {
        if lower.contains("sonnet-5") || lower.contains("opus-5") || lower.contains("-5") {
            return Some(1_000_000);
        }
        return Some(200_000);
    }

    // 4. OpenAI GPT-4 / o-series
    if lower.contains("gpt-4.5") {
        return Some(128_000);
    }
    if lower.contains("gpt-4o") || lower.contains("gpt-4-turbo") {
        return Some(128_000);
    }
    if lower.contains("o1") || lower.contains("o3") || lower.contains("o3-mini") {
        return Some(200_000);
    }
    if lower.starts_with("gpt-4") {
        return Some(128_000);
    }

    // 5. DeepSeek Family
    if lower.contains("deepseek") {
        return Some(64_000);
    }

    // 6. Qwen Family
    if lower.contains("qwen") {
        return Some(128_000);
    }

    // 7. Llama Family
    if lower.contains("llama-3") || lower.contains("llama3") {
        return Some(128_000);
    }

    // 8. Mistral / Codestral
    if lower.contains("codestral") {
        return Some(256_000);
    }
    if lower.contains("mistral") {
        return Some(128_000);
    }

    None
}

/// Dynamically resolve context window for a model against the remote provider API or heuristic catalog.
pub async fn resolve_dynamic_context_window(
    platform: &str,
    base_url: &str,
    api_key: &str,
    model_id: &str,
) -> Option<usize> {
    // 1. In-memory cache check (instant)
    if let Some(cached) = get_cached_context_window(base_url, model_id) {
        debug!(model = %model_id, context_window = cached, "Context window cache hit");
        return Some(cached);
    }

    // 2. Query remote provider endpoint if OpenAI-compatible
    if platform != "bedrock" && platform != "vertex-ai" {
        if let Some(endpoint_url) = normalize_models_endpoint(base_url) {
            let first_key = api_key.lines().next().unwrap_or(api_key).trim();
            let mut req = HTTP_CLIENT.get(&endpoint_url);
            if !first_key.is_empty() {
                if platform == "anthropic" || platform == "claude" {
                    req = req
                        .header("x-api-key", first_key)
                        .header("anthropic-version", "2023-06-01");
                } else {
                    req = req.header("Authorization", format!("Bearer {first_key}"));
                }
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<RemoteModelsResponse>().await {
                        Ok(body) => {
                            let mut found_for_target = None;
                            let target_stripped = strip_model_prefix(model_id);

                            for m in body.data {
                                let window = m
                                    .context_length
                                    .or(m.context_window)
                                    .or(m.max_context_tokens)
                                    .or_else(|| m.capabilities.as_ref().and_then(|c| c.context_window));

                                if let Some(w) = window {
                                    insert_cached_context_window(base_url, &m.id, w);

                                    let entry_stripped = strip_model_prefix(&m.id);
                                    if m.id.eq_ignore_ascii_case(model_id)
                                        || entry_stripped.eq_ignore_ascii_case(model_id)
                                        || m.id.eq_ignore_ascii_case(target_stripped)
                                        || entry_stripped.eq_ignore_ascii_case(target_stripped)
                                    {
                                        found_for_target = Some(w);
                                    }
                                }
                            }

                            if let Some(w) = found_for_target {
                                info!(
                                    model = %model_id,
                                    context_window = w,
                                    "Resolved dynamic context window from provider API"
                                );
                                return Some(w);
                            }
                        }
                        Err(e) => {
                            debug!(
                                error = %e,
                                endpoint = %endpoint_url,
                                "Failed to parse provider /models response"
                            );
                        }
                    }
                }
                Ok(resp) => {
                    debug!(
                        status = %resp.status(),
                        endpoint = %endpoint_url,
                        "Provider /models endpoint returned non-success"
                    );
                }
                Err(e) => {
                    debug!(
                        error = %e,
                        endpoint = %endpoint_url,
                        "Provider /models endpoint unreachable"
                    );
                }
            }
        }
    }

    // 3. Heuristic fallback rules (Tier 3)
    if let Some(heuristic) = resolve_heuristic_context_window(model_id) {
        info!(
            model = %model_id,
            context_window = heuristic,
            "Resolved context window from heuristic catalog rules"
        );
        insert_cached_context_window(base_url, model_id, heuristic);
        return Some(heuristic);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_model_prefix() {
        assert_eq!(strip_model_prefix("ag/gemini-3.8-flash-high"), "gemini-3.8-flash-high");
        assert_eq!(strip_model_prefix("cx/gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(strip_model_prefix("kr/claude-sonnet-5"), "claude-sonnet-5");
        assert_eq!(strip_model_prefix("bare-model"), "bare-model");
    }

    #[test]
    fn test_normalize_models_endpoint() {
        assert_eq!(
            normalize_models_endpoint("http://127.0.0.1:20128/v1"),
            Some("http://127.0.0.1:20128/v1/models".into())
        );
        assert_eq!(
            normalize_models_endpoint("http://127.0.0.1:20128/v1/"),
            Some("http://127.0.0.1:20128/v1/models".into())
        );
        assert_eq!(
            normalize_models_endpoint("http://127.0.0.1:20128/v1/chat/completions"),
            Some("http://127.0.0.1:20128/v1/models".into())
        );
        assert_eq!(
            normalize_models_endpoint("http://127.0.0.1:20128/v1/responses"),
            Some("http://127.0.0.1:20128/v1/models".into())
        );
        assert_eq!(
            normalize_models_endpoint("http://127.0.0.1:20128/v1/models"),
            Some("http://127.0.0.1:20128/v1/models".into())
        );
        assert_eq!(normalize_models_endpoint("invalid-url"), None);
    }

    #[test]
    fn test_resolve_heuristic_context_window() {
        // Gemini
        assert_eq!(resolve_heuristic_context_window("ag/gemini-3.8-flash-high"), Some(1_048_576));
        assert_eq!(resolve_heuristic_context_window("gemini-2.5-pro"), Some(2_000_000));
        assert_eq!(resolve_heuristic_context_window("gemini-2.5-flash"), Some(1_000_000));

        // GPT-5.6 (9router)
        assert_eq!(resolve_heuristic_context_window("cx/gpt-5.6-sol"), Some(372_000));
        assert_eq!(resolve_heuristic_context_window("cx/gpt-5.6-terra"), Some(272_000));
        assert_eq!(resolve_heuristic_context_window("cx/gpt-5.6-luna"), Some(128_000));

        // Claude
        assert_eq!(resolve_heuristic_context_window("kr/claude-sonnet-5"), Some(1_000_000));
        assert_eq!(resolve_heuristic_context_window("claude-3-7-sonnet"), Some(200_000));

        // OpenAI GPT-4 / o-series
        assert_eq!(resolve_heuristic_context_window("gpt-4o"), Some(128_000));
        assert_eq!(resolve_heuristic_context_window("o3-mini"), Some(200_000));

        // Unknown
        assert_eq!(resolve_heuristic_context_window("completely-unknown-model"), None);
    }

    #[test]
    fn test_context_window_cache_operations() {
        clear_context_cache();

        let base_url = "http://127.0.0.1:20128/v1";
        let model_id = "ag/gemini-3.8-flash-high";

        assert_eq!(get_cached_context_window(base_url, model_id), None);

        insert_cached_context_window(base_url, model_id, 1_048_576);

        assert_eq!(get_cached_context_window(base_url, model_id), Some(1_048_576));
        // Prefix-stripped lookup should also hit cache
        assert_eq!(
            get_cached_context_window(base_url, "gemini-3.8-flash-high"),
            Some(1_048_576)
        );

        clear_context_cache();
        assert_eq!(get_cached_context_window(base_url, model_id), None);
    }
}
