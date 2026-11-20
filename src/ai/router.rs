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
            Ok((text, _tin, _tout)) => {
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
async fn call_deepseek(
    api_key: &str,
    system: &str,
    user: &str,
) -> Result<(String, i32, i32), String> {
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

    // Tokens as the provider counted them. Absent counts record as zero rather than failing the call:
    // a missing number is not worth losing a customer's reply over.
    let tin = body["usage"]["prompt_tokens"].as_i64().unwrap_or(0) as i32;
    let tout = body["usage"]["completion_tokens"].as_i64().unwrap_or(0) as i32;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| (s.to_string(), tin, tout))
        .ok_or_else(|| "DeepSeek returned no content".to_string())
}

/// Call OpenAI Chat Completions API
async fn call_openai(
    api_key: &str,
    system: &str,
    user: &str,
) -> Result<(String, i32, i32), String> {
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

    let tin = body["usage"]["prompt_tokens"].as_i64().unwrap_or(0) as i32;
    let tout = body["usage"]["completion_tokens"].as_i64().unwrap_or(0) as i32;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| (s.to_string(), tin, tout))
        .ok_or_else(|| "OpenAI returned no content".to_string())
}

/// Call Anthropic Messages API
async fn call_anthropic(
    api_key: &str,
    system: &str,
    user: &str,
) -> Result<(String, i32, i32), String> {
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

    // Anthropic reports its counts as input_tokens/output_tokens.
    let tin = body["usage"]["input_tokens"].as_i64().unwrap_or(0) as i32;
    let tout = body["usage"]["output_tokens"].as_i64().unwrap_or(0) as i32;

    body["content"][0]["text"]
        .as_str()
        .map(|s| (s.to_string(), tin, tout))
        .ok_or_else(|| "Anthropic returned no content".to_string())
}

/// Route a prompt, and record who paid for it.
///
/// This is the entry point every feature should use: the same provider chain as `route_to_llm`, but each
/// attempt writes a row to `ai_usage` — including the failures, because a provider refusing OUR credential
/// is precisely what an operator needs to see. `platform_key` on the row says whether the account's own
/// credential or the platform's paid, which is the difference between a customer's cost and ours.
pub async fn route_to_llm_tracked(
    db: &sqlx::PgPool,
    tenant_id: uuid::Uuid,
    feature: &str,
    primary: &str,
    mk: &crate::provider_keys::ModelKeys,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, String> {
    // THE ASKED-FOR PROVIDER GOES FIRST, and the rest follow in the app's usual order. The parameter used
    // to be accepted and ignored — every call went DeepSeek first no matter what was requested — so a
    // feature that asked for another provider silently got the default one.
    let mut order: Vec<&str> = Vec::with_capacity(3);
    order.push(primary);
    for provider in ["deepseek", "openai", "anthropic"] {
        if provider != primary {
            order.push(provider);
        }
    }
    let mut last_err = String::from("no model provider is configured for this account");

    for provider in order {
        let Some(api_key) = mk.keys.get(provider).cloned() else {
            continue;
        };
        let on_platform = mk.on_platform_key(provider);
        let model = match provider {
            "deepseek" => "deepseek-chat",
            "openai" => "gpt-4o-mini",
            "anthropic" => "claude-sonnet-4-20250514",
            _ => "",
        };

        let result = match provider {
            "deepseek" => call_deepseek(&api_key, system_prompt, user_prompt).await,
            "openai" => call_openai(&api_key, system_prompt, user_prompt).await,
            "anthropic" => call_anthropic(&api_key, system_prompt, user_prompt).await,
            _ => continue,
        };

        match result {
            Ok((text, tin, tout)) => {
                record_usage(
                    db,
                    tenant_id,
                    feature,
                    provider,
                    model,
                    tin,
                    tout,
                    on_platform,
                    true,
                    None,
                )
                .await;
                tracing::info!(provider, feature, on_platform, "LLM call succeeded");
                return Ok(text);
            }
            Err(e) => {
                record_usage(
                    db,
                    tenant_id,
                    feature,
                    provider,
                    model,
                    0,
                    0,
                    on_platform,
                    false,
                    Some(&e),
                )
                .await;
                tracing::warn!(provider, feature, error = %e, "LLM provider failed, trying next");
                last_err = e;
            }
        }
    }

    Err(last_err)
}

/// Write one usage row. Never fails the caller: a customer's reply must not be lost because a tally could
/// not be written.
#[allow(clippy::too_many_arguments)]
async fn record_usage(
    db: &sqlx::PgPool,
    tenant_id: uuid::Uuid,
    feature: &str,
    provider: &str,
    model: &str,
    tokens_in: i32,
    tokens_out: i32,
    platform_key: bool,
    ok: bool,
    detail: Option<&str>,
) {
    if let Err(e) = sqlx::query(
        "INSERT INTO ai_usage (tenant_id, feature, provider, model, tokens_in, tokens_out,
                               platform_key, ok, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(tenant_id)
    .bind(feature)
    .bind(provider)
    .bind(model)
    .bind(tokens_in)
    .bind(tokens_out)
    .bind(platform_key)
    .bind(ok)
    .bind(detail)
    .execute(db)
    .await
    {
        tracing::warn!(error = %e, "an AI usage row could not be written");
    }
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
