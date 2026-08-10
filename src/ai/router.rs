//! AI Provider Router — routes AI requests to external LLM providers.
//!
//! Multi-provider with automatic fallback chain.
//! Primary: DeepSeek | Backups: OpenAI → Anthropic
//!
//! If one provider fails, the next in the chain is tried.
//! If all fail, the template engine takes over.

use serde_json::json;

/// Route a prompt to the configured LLM provider with fallback chain.
/// Tries primary first, then falls through to next providers on failure.
pub async fn route_to_llm(
    _primary: &str,
    api_keys: &std::collections::HashMap<String, String>,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, String> {
    // Define fallback chain: primary -> openai -> anthropic
    let providers = ["deepseek", "openai", "anthropic"];

    // Try providers in priority order
    for provider in &providers {
        // Skip if this provider wasn't requested and we already tried primary
        let api_key = match api_keys.get(*provider) {
            Some(key) => key.clone(),
            None => continue,
        };

        let result = match *provider {
            "deepseek" => call_deepseek(&api_key, system_prompt, user_prompt).await,
            "openai" => call_openai(&api_key, system_prompt, user_prompt).await,
            "anthropic" => call_anthropic(&api_key, system_prompt, user_prompt).await,
            _ => continue,
        };

        match result {
            Ok(text) => {
                tracing::info!(provider = %provider, "LLM call succeeded");
                return Ok(text);
            }
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e, "LLM provider failed, trying next");
                // Continue to next provider in chain
            }
        }
    }

    Err("All LLM providers failed".to_string())
}

/// Call DeepSeek Chat Completions API
/// Docs: https://api-docs.deepseek.com/
async fn call_deepseek(api_key: &str, system: &str, user: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let payload = json!({
        "model": "deepseek-chat",
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "max_tokens": 500,
        "temperature": 0.7,
        "stream": false
    });

    let resp = client
        .post("https://api.deepseek.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("DeepSeek request failed: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("DeepSeek returned {}", status));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("DeepSeek response parse failed: {}", e))?;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "DeepSeek returned no content".to_string())
}

/// Call OpenAI Chat Completions API
async fn call_openai(api_key: &str, system: &str, user: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let payload = json!({
        "model": "gpt-4o-mini",
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "max_tokens": 500,
        "temperature": 0.7,
    });

    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("OpenAI request failed: {}", e))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("OpenAI response parse failed: {}", e))?;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "OpenAI returned no content".to_string())
}

/// Call Anthropic Messages API
async fn call_anthropic(api_key: &str, system: &str, user: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let payload = json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 500,
        "system": system,
        "messages": [
            {"role": "user", "content": user}
        ]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Anthropic request failed: {}", e))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Anthropic response parse failed: {}", e))?;

    body["content"][0]["text"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Anthropic returned no content".to_string())
}

/// Use AI to compose a personalized follow-up message with fallback chain.
/// Tries DeepSeek first → OpenAI → Anthropic → template fallback.
pub async fn ai_compose_follow_up(
    api_keys: &std::collections::HashMap<String, String>,
    contact_name: &str,
    business_name: &str,
    context: &str,
    health_score: i32,
    num_signals: i32,
) -> String {
    let system = "You are a B2B customer success AI. Your job is to write short, personal follow-up emails that re-engage business owners without sounding robotic or salesy. Keep it under 100 words. Use their business name naturally. Never use generic phrases like 'just checking in' or 'touching base.'";

    let user = format!(
        "Write a follow-up email for:
        Contact: {contact_name}
        Business: {business_name}
        Context: {context}
        Health score: {health_score}/100
        Events recorded: {num_signals}

        The goal is to get them to take the next action in their onboarding checklist."
    );

    // Try primary DeepSeek first, fall through OpenAI → Anthropic
    match route_to_llm("deepseek", api_keys, system, &user).await {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(error = %e, "All LLM providers failed, falling back to template");
            format!(
                "Hi {contact_name},\n\nQuick note about your {business_name} setup — \
                I noticed you're at step {} of onboarding. Want a hand finishing up? \
                Reply here and I'll help personally.\n\nBest,\nThe CRM Swift Team",
                match context {
                    "checklist_stage_2" => "2",
                    "checklist_stage_3" => "3",
                    _ => "1",
                }
            )
        }
    }
}
