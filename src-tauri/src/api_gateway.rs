use crate::models::{
    Account, AccountState, ApiGatewayCombo, ApiGatewayConfig,
    ApiGatewayServerState, ApiGatewayStatus, ApiRotationStrategy, ApiUsageReport, ApiUsageRow,
    ApiGatewayModelRegistry, TokenBreakdown, ToolId,
};
use crate::store::{Store, StoredState};
use crate::tools::default_config_dir;
use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::Path as FsPath;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub struct ApiServerHandle {
    pub(crate) shutdown: Option<oneshot::Sender<()>>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    pub status: ApiGatewayStatus,
}

impl ApiServerHandle {
    pub fn stopped(config: &ApiGatewayConfig) -> Self {
        Self {
            shutdown: None,
            thread: None,
            status: ApiGatewayStatus {
                state: ApiGatewayServerState::Stopped,
                base_url: base_url(config),
                error: None,
            },
        }
    }

    pub fn stop(&mut self, config: &ApiGatewayConfig) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.status = ApiGatewayStatus {
            state: ApiGatewayServerState::Stopped,
            base_url: base_url(config),
            error: None,
        };
    }
}

struct GatewayState {
    store: Store,
    client: reqwest::Client,
    runtime: Mutex<GatewayRuntime>,
}

#[derive(Default)]
struct GatewayRuntime {
    rr_index: HashMap<String, usize>,
    affinity: HashMap<String, String>,
    cooldowns: HashMap<String, Instant>,
}

/// A resolved routing target: a specific account serving a specific upstream model.
#[derive(Clone)]
struct SelectedMember {
    tool_id: ToolId,
    /// Upstream model name to send (the combo member).
    model: String,
    account: Account,
    /// Stable account identity used for cooldown/affinity (`tool:account_id`).
    key: String,
}

pub fn start_server(store: Store, config: ApiGatewayConfig) -> Result<ApiServerHandle> {
    let addr = SocketAddr::new(
        config
            .bind_host
            .parse::<IpAddr>()
            .context("Invalid bind address")?,
        config.port,
    );
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = GatewayState {
        store,
        client: reqwest::Client::new(),
        runtime: Mutex::new(GatewayRuntime::default()),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/messages", post(messages))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/backend-api/codex/:tail", post(codex_direct))
        .with_state(Arc::new(state));

    let listener = std::net::TcpListener::bind(addr).context("Couldn't bind API server port")?;
    listener
        .set_nonblocking(true)
        .context("Couldn't configure API server socket")?;
    let listener = tokio::net::TcpListener::from_std(listener)?;

    let thread = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };
        runtime.block_on(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
    });

    Ok(ApiServerHandle {
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
        status: ApiGatewayStatus {
            state: ApiGatewayServerState::Running,
            base_url: base_url(&config),
            error: None,
        },
    })
}

pub fn base_url(config: &ApiGatewayConfig) -> String {
    format!("http://{}:{}", config.bind_host, config.port)
}

async fn health(State(state): State<Arc<GatewayState>>) -> impl IntoResponse {
    match state.store.load() {
        Ok(data) => Json(json!({
            "ok": true,
            "combos": data.api_gateway.combos.len(),
            "keys": data.api_gateway.keys.iter().filter(|key| key.enabled).count()
        }))
        .into_response(),
        Err(err) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            err.to_string(),
        ),
    }
}

async fn models(State(state): State<Arc<GatewayState>>, headers: HeaderMap) -> Response {
    let data = match authorized_state(&state, &headers) {
        Ok(data) => data,
        Err(response) => return response,
    };
    let mut ids = Vec::<String>::new();
    for combo in &data.api_gateway.combos {
        if combo.enabled && !ids.contains(&combo.name) {
            ids.push(combo.name.clone());
        }
    }
    for registry in &data.api_gateway.model_registry {
        for model in &registry.models {
            if !ids.contains(model) {
                ids.push(model.clone());
            }
        }
    }
    Json(json!({
        "object": "list",
        "data": ids.into_iter().map(|id| json!({"id": id, "object": "model"})).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn messages(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    handle_ai_request(state, headers, body, ClientProtocol::Anthropic).await
}

async fn chat_completions(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    handle_ai_request(state, headers, body, ClientProtocol::OpenAiChat).await
}

async fn responses(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    handle_ai_request(state, headers, body, ClientProtocol::OpenAiResponses).await
}

async fn codex_direct(
    State(state): State<Arc<GatewayState>>,
    Path(tail): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let protocol = if tail == "responses" {
        ClientProtocol::OpenAiResponses
    } else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("Model '{tail}' not found"),
        );
    };
    handle_ai_request(state, headers, body, protocol).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientProtocol {
    Anthropic,
    OpenAiChat,
    OpenAiResponses,
}

async fn handle_ai_request(
    state: Arc<GatewayState>,
    headers: HeaderMap,
    body: Value,
    protocol: ClientProtocol,
) -> Response {
    let data = match authorized_state(&state, &headers) {
        Ok(data) => data,
        Err(response) => return response,
    };
    let key_id = auth_key_id(&data, &headers).unwrap_or_else(|| "unknown".to_string());
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(combo) = resolve_combo(&data, &model) else {
        return model_not_found(&model);
    };
    let session_id = session_id(&headers, &body);
    let max_attempts = usize::from(data.api_gateway.max_retries.max(1));
    let mut tried = HashSet::new();

    for _ in 0..max_attempts {
        let Some(selected) = select_member(&state, &data, &combo, &session_id, &tried) else {
            return exhausted_response(&model);
        };
        tried.insert(selected.key.clone());
        bind_session(&state, &session_id, &selected.key);

        let mut request_body = body.clone();
        request_body["model"] = Value::String(selected.model.clone());
        let provider = match selected.tool_id {
            ToolId::Claude => ClientProtocol::Anthropic,
            ToolId::Codex => ClientProtocol::OpenAiResponses,
            ToolId::Antigravity => unreachable!("Antigravity is not selectable for API combos"),
        };
        let request_body = match (protocol, provider) {
            (left, right) if left == right => request_body,
            (ClientProtocol::OpenAiChat, ClientProtocol::OpenAiResponses) => {
                openai_chat_to_responses(request_body)
            }
            (ClientProtocol::OpenAiChat, ClientProtocol::Anthropic) => {
                openai_chat_to_anthropic(request_body)
            }
            (ClientProtocol::OpenAiResponses, ClientProtocol::Anthropic) => {
                openai_responses_to_anthropic(request_body)
            }
            (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses) => {
                anthropic_to_responses(request_body)
            }
            _ => {
                return api_error(
                    StatusCode::NOT_IMPLEMENTED,
                    "unsupported_translation",
                    "Cross-provider translation is not implemented in this build",
                )
            }
        };

        let builder = match selected.tool_id {
            ToolId::Claude => build_claude_request(&state, &data, &selected.account, request_body),
            ToolId::Codex => build_codex_request(&state, &data, &selected.account, request_body),
            ToolId::Antigravity => unreachable!("guarded above"),
        };
        let builder = match builder {
            Ok(builder) => builder,
            Err(response) => {
                mark_member_error(&state, &selected.key, "Account token is missing or expired");
                if tried.len() >= max_attempts {
                    return response;
                }
                continue;
            }
        };
        let response = match builder.send().await {
            Ok(response) => response,
            Err(err) => {
                mark_cooldown(&state, &selected.key);
                if tried.len() >= max_attempts {
                    return api_error(
                        StatusCode::BAD_GATEWAY,
                        "upstream_error",
                        format!("Upstream request failed: {err}"),
                    );
                }
                continue;
            }
        };
        let credential_failed = matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        );
        if credential_failed {
            mark_member_error(
                &state,
                &selected.key,
                "Account token was rejected by upstream",
            );
            if tried.len() < max_attempts {
                continue;
            }
        }
        let retryable_failed = should_retry_upstream(response.status());
        if retryable_failed {
            mark_cooldown(&state, &selected.key);
            if tried.len() < max_attempts {
                continue;
            }
        }
        // Any other non-success (e.g. a 400 from a bad model on this member) → fall through to the
        // next combo member/account instead of returning a broken response, the way 9router does.
        // Only the last attempt surfaces the upstream error verbatim (handled in translate_response).
        let other_failure = !response.status().is_success() && !credential_failed && !retryable_failed;
        if other_failure && tried.len() < max_attempts {
            continue;
        }
        if response.status().is_success() {
            mark_member_available(&state, &selected.key);
        }
        return translate_response(
            &state.store,
            response,
            provider,
            protocol,
            &model,
            &key_id,
            &selected,
        )
        .await;
    }

    exhausted_response(&model)
}

/// Resolve the requested model into a combo: an ordered list of member model names plus a strategy.
/// A name that matches a combo uses that combo; otherwise, if some enabled account supports the
/// model directly, build a transient single-member combo so direct model ids still work.
fn resolve_combo(data: &StoredState, model: &str) -> Option<ApiGatewayCombo> {
    if let Some(combo) = data
        .api_gateway
        .combos
        .iter()
        .find(|combo| combo.enabled && combo.name == model)
    {
        return Some(combo.clone());
    }

    // Direct model: usable only if some enabled account of a provider supports it.
    if provider_for_model(data, model).is_some() {
        return Some(ApiGatewayCombo {
            id: format!("direct:{model}"),
            name: model.to_string(),
            members: vec![model.to_string()],
            strategy: None,
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        });
    }
    None
}

/// Whether the gateway can serve this model id: it names an enabled combo, or some enabled account
/// supports it as a direct model. Used to validate the model a virtual CLI account binds to.
pub fn model_is_servable(data: &StoredState, model: &str) -> bool {
    data.api_gateway
        .combos
        .iter()
        .any(|combo| combo.enabled && combo.name == model)
        || provider_for_model(data, model).is_some()
}

/// Which provider serves a given model: the provider whose enabled account's model registry lists
/// it. Falls back to a name heuristic (Claude models start with `claude`, GPT/Codex with `gpt`/`o`)
/// so freshly-typed models still route before a registry refresh.
fn provider_for_model(data: &StoredState, model: &str) -> Option<ToolId> {
    for tool_id in [ToolId::Claude, ToolId::Codex] {
        let supported = data
            .api_gateway
            .model_registry
            .iter()
            .filter(|registry| registry.tool_id == tool_id)
            .any(|registry| registry.models.iter().any(|candidate| candidate == model));
        if supported && has_enabled_account(data, &tool_id) {
            return Some(tool_id);
        }
    }
    let lower = model.to_ascii_lowercase();
    let guess = if lower.starts_with("claude") {
        ToolId::Claude
    } else if lower.starts_with("gpt") || lower.starts_with('o') || lower.contains("codex") {
        ToolId::Codex
    } else {
        return None;
    };
    has_enabled_account(data, &guess).then_some(guess)
}

/// True if at least one subscription account of this provider is enabled for the gateway (default
/// enabled when the participation list has no explicit entry yet).
fn has_enabled_account(data: &StoredState, tool_id: &ToolId) -> bool {
    data.accounts.iter().any(|account| {
        &account.tool_id == tool_id
            && account.api_provider.is_none()
            && !matches!(account.state, AccountState::NeedsLogin)
            && gateway_account_enabled(data, tool_id, &account.id)
    })
}

/// Whether an account participates in gateway rotation. Missing participation entry = enabled.
fn gateway_account_enabled(data: &StoredState, tool_id: &ToolId, account_id: &str) -> bool {
    data.api_gateway
        .accounts
        .iter()
        .find(|entry| &entry.tool_id == tool_id && entry.account_id == account_id)
        .is_none_or(|entry| entry.enabled)
}

fn authorized_state(
    state: &GatewayState,
    headers: &HeaderMap,
) -> std::result::Result<StoredState, Response> {
    let data = state.store.load().map_err(|err| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            err.to_string(),
        )
    })?;
    let Some(secret) = bearer_token(headers) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid API key",
        ));
    };
    let Some(key) = data
        .api_gateway
        .keys
        .iter()
        .find(|key| key.enabled && key.secret.as_deref() == Some(secret.as_str()))
    else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid API key",
        ));
    };
    if key
        .expires_at
        .as_deref()
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
        .is_some_and(|expires| expires < chrono::Utc::now())
    {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "API key expired",
        ));
    }
    Ok(data)
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    bearer.or_else(|| {
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn auth_key_id(data: &StoredState, headers: &HeaderMap) -> Option<String> {
    let secret = bearer_token(headers)?;
    data.api_gateway
        .keys
        .iter()
        .find(|key| key.secret.as_deref() == Some(secret.as_str()))
        .map(|key| key.id.clone())
}

fn session_id(headers: &HeaderMap, body: &Value) -> String {
    for name in ["x-session-id", "session-id", "openai-session-id"] {
        if let Some(value) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return value.to_string();
        }
    }
    if let Some(value) = body
        .pointer("/metadata/session_id")
        .or_else(|| body.pointer("/metadata/sessionId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return value.trim().to_string();
    }
    let seed = body
        .get("messages")
        .or_else(|| body.get("input"))
        .unwrap_or(body);
    let mut hasher = Sha256::new();
    hasher.update(seed.to_string().as_bytes());
    let digest = hasher.finalize();
    format!(
        "body:{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

fn select_member(
    state: &GatewayState,
    data: &StoredState,
    combo: &ApiGatewayCombo,
    session_id: &str,
    tried: &HashSet<String>,
) -> Option<SelectedMember> {
    let candidates = available_candidates(state, data, combo, tried);
    if candidates.is_empty() {
        return None;
    }

    let mut runtime = state.runtime.lock().ok()?;
    // Session affinity: stick to the bound account if it still has a live candidate.
    if let Some(bound_key) = runtime.affinity.get(session_id) {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| &candidate.key == bound_key)
        {
            return Some(candidate.clone());
        }
    }
    // Per-combo strategy overrides the gateway default. Fallback = always take the first live
    // candidate (member order = priority); round-robin = rotate the candidate list per combo.
    let strategy = combo
        .strategy
        .clone()
        .unwrap_or_else(|| data.api_gateway.rotation_strategy.clone());
    if strategy == ApiRotationStrategy::FillFirst {
        return candidates.first().cloned();
    }
    let index = runtime.rr_index.entry(combo.id.clone()).or_insert(0);
    let selected = candidates[*index % candidates.len()].clone();
    *index = index.saturating_add(1);
    Some(selected)
}

/// Build the ordered candidate list for a combo: for each member model (in priority order), every
/// enabled, non-cooling, under-quota account of the provider that serves that model. The first
/// account for a member is its primary; extra accounts give account-level fallback/rotation.
fn available_candidates(
    state: &GatewayState,
    data: &StoredState,
    combo: &ApiGatewayCombo,
    tried: &HashSet<String>,
) -> Vec<SelectedMember> {
    let now = Instant::now();
    let cooldowns = state
        .runtime
        .lock()
        .ok()
        .map(|runtime| runtime.cooldowns.clone())
        .unwrap_or_default();
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for model in &combo.members {
        let Some(tool_id) = provider_for_model(data, model) else {
            continue;
        };
        for account in data.accounts.iter().filter(|account| {
            account.tool_id == tool_id
                && account.api_provider.is_none()
                && !matches!(account.state, AccountState::NeedsLogin)
                && gateway_account_enabled(data, &tool_id, &account.id)
        }) {
            let key = account_key(&tool_id, &account.id);
            // De-dupe (account, model) pairs and skip tried/cooling/over-quota accounts.
            if !seen.insert((key.clone(), model.clone())) {
                continue;
            }
            if tried.contains(&key) || cooldowns.get(&key).is_some_and(|until| *until > now) {
                continue;
            }
            if quota_percent(account) >= data.api_gateway.quota_threshold {
                continue;
            }
            candidates.push(SelectedMember {
                tool_id: tool_id.clone(),
                model: model.clone(),
                account: account.clone(),
                key,
            });
        }
    }
    candidates
}

/// Upper bound on remembered session→account bindings. Hashed (`body:…`) session ids create a new
/// entry per distinct request body, so without a cap the affinity map would grow forever on a
/// long-running gateway. Affinity is best-effort: when we hit the cap we drop the whole map and
/// the next request simply re-picks an account.
const MAX_AFFINITY_ENTRIES: usize = 10_000;

fn bind_session(state: &GatewayState, session_id: &str, account_key: &str) {
    if let Ok(mut runtime) = state.runtime.lock() {
        // Drop expired cooldowns while we hold the lock — they are checked by timestamp so stale
        // entries are harmless, but they would otherwise accumulate indefinitely.
        let now = Instant::now();
        runtime.cooldowns.retain(|_, until| *until > now);
        if runtime.affinity.len() >= MAX_AFFINITY_ENTRIES
            && !runtime.affinity.contains_key(session_id)
        {
            runtime.affinity.clear();
        }
        runtime
            .affinity
            .insert(session_id.to_string(), account_key.to_string());
    }
}

fn mark_cooldown(state: &GatewayState, account_key: &str) {
    let until = chrono::Utc::now() + chrono::Duration::seconds(60);
    if let Ok(mut runtime) = state.runtime.lock() {
        runtime.cooldowns.insert(
            account_key.to_string(),
            Instant::now() + Duration::from_secs(60),
        );
    }
    persist_member_state(
        state,
        account_key,
        crate::models::ApiPoolAccountState::CoolingDown,
        Some(until.to_rfc3339()),
        None,
    );
}

fn mark_member_error(state: &GatewayState, account_key: &str, error: &str) {
    persist_member_state(
        state,
        account_key,
        crate::models::ApiPoolAccountState::Errored,
        None,
        Some(error.to_string()),
    );
}

fn mark_member_available(state: &GatewayState, account_key: &str) {
    persist_member_state(
        state,
        account_key,
        crate::models::ApiPoolAccountState::Available,
        None,
        None,
    );
}

fn persist_member_state(
    state: &GatewayState,
    account_key_value: &str,
    member_state: crate::models::ApiPoolAccountState,
    cooldown_until: Option<String>,
    error: Option<String>,
) {
    let Some((tool_id, account_id)) = parse_account_key(account_key_value) else {
        return;
    };
    let Ok(mut data) = state.store.load() else {
        return;
    };
    // Upsert the participation entry's runtime state (it may not exist if the account was never
    // toggled — default-enabled accounts have no row until something happens to them).
    if let Some(entry) = data
        .api_gateway
        .accounts
        .iter_mut()
        .find(|entry| entry.tool_id == tool_id && entry.account_id == account_id)
    {
        entry.state = member_state;
        entry.cooldown_until = cooldown_until;
        entry.error = error;
    } else {
        data.api_gateway.accounts.push(crate::models::ApiGatewayAccount {
            tool_id,
            account_id,
            enabled: true,
            state: member_state,
            cooldown_until,
            error,
        });
    }
    let _ = state.store.save(&data);
}

fn account_key(tool_id: &ToolId, account_id: &str) -> String {
    format!("{}:{}", tool_id.as_str(), account_id)
}

fn parse_account_key(key: &str) -> Option<(ToolId, String)> {
    let (tool, account_id) = key.split_once(':')?;
    let tool_id = match tool {
        "claude" => ToolId::Claude,
        "codex" => ToolId::Codex,
        "antigravity" => ToolId::Antigravity,
        _ => return None,
    };
    Some((tool_id, account_id.to_string()))
}

fn should_retry_upstream(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn openai_chat_to_responses(mut body: Value) -> Value {
    // Hoist any system messages into the required top-level `instructions`; the rest become `input`.
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if message.get("role").and_then(Value::as_str) == Some("system") {
            let text = message_text(&message);
            if !text.is_empty() {
                instructions.push(text);
            }
        } else {
            input.push(message);
        }
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("messages");
        object.insert("input".to_string(), Value::Array(input));
        object.insert(
            "instructions".to_string(),
            Value::String(if instructions.is_empty() {
                "You are a helpful coding assistant.".to_string()
            } else {
                instructions.join("\n\n")
            }),
        );
    }
    body
}

fn openai_chat_to_anthropic(body: Value) -> Value {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        if role == "system" {
            system.push(message_text(&message));
        } else if role == "tool" {
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.get("tool_call_id").and_then(Value::as_str).unwrap_or(""),
                    "content": message_text(&message)
                }]
            }));
        } else {
            let mut content = Vec::new();
            let text = message_text(&message);
            if !text.is_empty() {
                content.push(json!({"type": "text", "text": text}));
            }
            if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in tool_calls {
                    let function = call.get("function").unwrap_or(call);
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .and_then(|value| serde_json::from_str::<Value>(value).ok())
                        .unwrap_or_else(|| json!({}));
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                        "name": function.get("name").and_then(Value::as_str).unwrap_or(""),
                        "input": arguments
                    }));
                }
            }
            messages.push(json!({
                "role": if role == "assistant" { "assistant" } else { "user" },
                "content": content
            }));
        }
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::String(String::new())),
        "max_tokens": body.get("max_tokens").or_else(|| body.get("max_completion_tokens")).cloned().unwrap_or(Value::Number(4096.into())),
        "messages": messages
    });
    if !system.is_empty() {
        out["system"] = Value::String(system.join("\n\n"));
    }
    copy_if_present(&body, &mut out, "stream");
    copy_if_present(&body, &mut out, "temperature");
    copy_if_present(&body, &mut out, "top_p");
    if let Some(tools) = openai_tools_to_anthropic(body.get("tools")) {
        out["tools"] = tools;
    }
    if let Some(choice) = body.get("tool_choice") {
        if let Some(choice) = openai_tool_choice_to_anthropic(choice) {
            out["tool_choice"] = choice;
        }
    }
    out
}

fn openai_responses_to_anthropic(body: Value) -> Value {
    let mut messages = Vec::new();
    match body.get("input") {
        Some(Value::String(text)) => messages.push(json!({"role": "user", "content": text})),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => messages.push(json!({
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use",
                            "id": item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or(""),
                            "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                            "input": item.get("arguments").and_then(Value::as_str)
                                .and_then(|value| serde_json::from_str::<Value>(value).ok())
                                .unwrap_or_else(|| json!({}))
                        }]
                    })),
                    Some("function_call_output") => messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                            "content": item.get("output").cloned().unwrap_or(Value::String(String::new()))
                        }]
                    })),
                    _ => {
                        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                        messages.push(json!({
                            "role": if role == "assistant" { "assistant" } else { "user" },
                            "content": message_text(item)
                        }));
                    }
                }
            }
        }
        _ => {}
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::String(String::new())),
        "max_tokens": body.get("max_output_tokens").or_else(|| body.get("max_tokens")).cloned().unwrap_or(Value::Number(4096.into())),
        "messages": messages
    });
    copy_if_present(&body, &mut out, "stream");
    copy_if_present(&body, &mut out, "temperature");
    copy_if_present(&body, &mut out, "top_p");
    if let Some(tools) = openai_tools_to_anthropic(body.get("tools")) {
        out["tools"] = tools;
    }
    out
}

fn anthropic_to_responses(body: Value) -> Value {
    let mut input = Vec::new();
    // Codex's responses endpoint requires a top-level `instructions` string (the system prompt) —
    // a `{role:"system"}` item inside `input` is rejected with "Instructions are required".
    let instructions = anthropic_system_text(body.get("system"));
    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        match message.get("content") {
            Some(Value::Array(parts)) => {
                let mut text_parts = Vec::new();
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("tool_use") => input.push(json!({
                            "type": "function_call",
                            "call_id": part.get("id").and_then(Value::as_str).unwrap_or(""),
                            "name": part.get("name").and_then(Value::as_str).unwrap_or(""),
                            "arguments": part.get("input").cloned().unwrap_or_else(|| json!({})).to_string()
                        })),
                        Some("tool_result") => input.push(json!({
                            "type": "function_call_output",
                            "call_id": part.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                            "output": part.get("content").cloned().unwrap_or(Value::String(String::new()))
                        })),
                        _ => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                text_parts.push(text.to_string());
                            }
                        }
                    }
                }
                if !text_parts.is_empty() {
                    input.push(json!({"role": role, "content": text_parts.join("\n")}));
                }
            }
            _ => input.push(json!({"role": role, "content": message_text(&message)})),
        }
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::String(String::new())),
        "instructions": if instructions.is_empty() {
            "You are a helpful coding assistant.".to_string()
        } else {
            instructions
        },
        "input": input,
    });
    if let Some(max_tokens) = body.get("max_tokens") {
        out["max_output_tokens"] = max_tokens.clone();
    }
    copy_if_present(&body, &mut out, "stream");
    copy_if_present(&body, &mut out, "temperature");
    copy_if_present(&body, &mut out, "top_p");
    if let Some(tools) = anthropic_tools_to_openai(body.get("tools")) {
        out["tools"] = tools;
    }
    out
}

/// Anthropic `system` can be a plain string or an array of `{type:"text", text:...}` blocks.
/// Flatten either form to a single instructions string.
fn anthropic_system_text(system: Option<&Value>) -> String {
    match system {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn openai_tools_to_anthropic(tools: Option<&Value>) -> Option<Value> {
    let tools = tools?.as_array()?;
    Some(Value::Array(
        tools
            .iter()
            .map(|tool| {
                let function = tool.get("function").unwrap_or(tool);
                json!({
                    "name": function.get("name").cloned().unwrap_or(Value::String(String::new())),
                    "description": function.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}))
                })
            })
            .collect(),
    ))
}

fn anthropic_tools_to_openai(tools: Option<&Value>) -> Option<Value> {
    let tools = tools?.as_array()?;
    Some(Value::Array(
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.get("name").cloned().unwrap_or(Value::String(String::new())),
                    "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"}))
                })
            })
            .collect(),
    ))
}

fn openai_tool_choice_to_anthropic(choice: &Value) -> Option<Value> {
    match choice.as_str() {
        Some("none") => None,
        Some("required") => Some(json!({"type": "any"})),
        Some(_) => Some(json!({"type": "auto"})),
        None => choice
            .pointer("/function/name")
            .and_then(Value::as_str)
            .map(|name| json!({"type": "tool", "name": name}))
            .or_else(|| Some(json!({"type": "auto"}))),
    }
}

fn copy_if_present(from: &Value, to: &mut Value, key: &str) {
    if let Some(value) = from.get(key) {
        to[key] = value.clone();
    }
}

fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("input_text"))
                    .or_else(|| part.get("output_text"))
                    .or_else(|| part.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn build_claude_request(
    state: &GatewayState,
    data: &StoredState,
    account: &Account,
    mut body: Value,
) -> std::result::Result<reqwest::RequestBuilder, Response> {
    sanitize_anthropic_body(&mut body);
    let config_dir = account_config_dir(&state.store, data, account);
    let binary = data
        .tool_setups
        .get(ToolId::Claude.as_str())
        .and_then(|setup| setup.binary_path.as_deref());
    let Some(token) = crate::quota::claude_oauth_token_fresh(&config_dir, binary) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Account token is missing or expired",
        ));
    };
    let version = crate::quota::claude_version().unwrap_or_else(|| "2.0.0".to_string());
    Ok(state
        .client
        .request(Method::POST, "https://api.anthropic.com/v1/messages")
        .bearer_auth(token)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
        .header(
            header::USER_AGENT,
            format!("claude-cli/{version} (external, sdk-cli)"),
        )
        .json(&body))
}

/// Strip request fields that newer Claude Code clients send but the Anthropic OAuth Messages
/// endpoint rejects (e.g. `context_management` → "Extra inputs are not permitted"). We only forward
/// the documented Messages API fields, so an evolving client can't 400 the upstream.
fn sanitize_anthropic_body(body: &mut Value) {
    const ALLOWED: &[&str] = &[
        "model",
        "messages",
        "system",
        "max_tokens",
        "metadata",
        "stop_sequences",
        "stream",
        "temperature",
        "top_k",
        "top_p",
        "tools",
        "tool_choice",
        "thinking",
    ];
    if let Some(object) = body.as_object_mut() {
        object.retain(|key, _| ALLOWED.contains(&key.as_str()));
    }
}

fn build_codex_request(
    state: &GatewayState,
    data: &StoredState,
    account: &Account,
    mut body: Value,
) -> std::result::Result<reqwest::RequestBuilder, Response> {
    sanitize_codex_body(&mut body);
    let config_dir = account_config_dir(&state.store, data, account);
    let Some(token) = crate::quota::codex_access_token_fresh(&config_dir) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Account token is missing or expired",
        ));
    };
    let mut request = state
        .client
        .request(
            Method::POST,
            "https://chatgpt.com/backend-api/codex/responses",
        )
        .bearer_auth(token)
        .header(header::ACCEPT, "text/event-stream")
        .header(header::USER_AGENT, "codex_cli_rs/0.0.0")
        .json(&body);
    if let Some(account_id) = crate::quota::codex_account_id(&config_dir) {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    Ok(request)
}

/// Enforce the fields the Codex backend `/responses` endpoint requires (and that the real Codex CLI
/// always sends), so requests translated from another protocol — or sent by clients that omit them
/// — aren't rejected (e.g. "Store must be set to false"). Also drops Chat/Anthropic-only fields the
/// Responses API doesn't accept.
fn sanitize_codex_body(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        // The backend rejects server-side storage outright.
        object.insert("store".to_string(), Value::Bool(false));
        // Codex streams by default; keep the caller's choice but default to streaming when absent.
        object
            .entry("stream".to_string())
            .or_insert(Value::Bool(true));
        // `instructions` is required; provide a default if a caller didn't set one.
        if !object
            .get("instructions")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        {
            object.insert(
                "instructions".to_string(),
                Value::String("You are a helpful coding assistant.".to_string()),
            );
        }
        // Chat-style / sampling fields the Responses endpoint doesn't accept.
        for key in ["messages", "max_tokens", "max_completion_tokens", "top_p", "temperature"] {
            object.remove(key);
        }
    }
}

pub fn discover_account_models(
    store: &Store,
    data: &StoredState,
    account: &Account,
    binary: Option<&FsPath>,
) -> ApiGatewayModelRegistry {
    let config_dir = account_config_dir(store, data, account);
    let result = match account.tool_id {
        ToolId::Claude => discover_claude_models(&config_dir, binary),
        ToolId::Codex => discover_codex_models(&config_dir, binary),
        ToolId::Antigravity => Err(anyhow::anyhow!(
            "Antigravity is not supported by the API gateway"
        )),
    };
    match result {
        Ok(mut models) => {
            models.sort();
            models.dedup();
            ApiGatewayModelRegistry {
                tool_id: account.tool_id.clone(),
                account_id: account.id.clone(),
                models,
                updated_at: chrono::Utc::now().to_rfc3339(),
                error: None,
            }
        }
        Err(error) => ApiGatewayModelRegistry {
            tool_id: account.tool_id.clone(),
            account_id: account.id.clone(),
            models: Vec::new(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            error: Some(format!("{error:#}")),
        },
    }
}

fn discover_claude_models(config_dir: &FsPath, binary: Option<&FsPath>) -> Result<Vec<String>> {
    let token = crate::quota::claude_oauth_token_fresh(config_dir, binary)
        .context("Claude OAuth token is missing or expired")?;
    let version = crate::quota::claude_version().unwrap_or_else(|| "2.0.0".to_string());
    let value = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("Couldn't build HTTP client")?
        .get("https://api.anthropic.com/v1/models?limit=1000")
        .bearer_auth(token)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
        .header(
            reqwest::header::USER_AGENT,
            format!("claude-cli/{version} (external, sdk-cli)"),
        )
        .send()
        .context("Couldn't fetch Claude models")?
        .error_for_status()
        .context("Claude model registry request failed")?
        .json::<Value>()
        .context("Claude model registry response is invalid")?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect())
}

fn discover_codex_models(config_dir: &FsPath, binary: Option<&FsPath>) -> Result<Vec<String>> {
    let binary = binary.unwrap_or_else(|| FsPath::new("codex"));
    let mut child = Command::new(binary)
        .args(["app-server", "--stdio"])
        .env("CODEX_HOME", config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Couldn't start Codex model registry")?;
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().context("Codex app-server has no stdin")?;
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "ai-switcher",
                        "title": "AI Account Switcher",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {}
                }
            })
        )?;
        std::thread::sleep(Duration::from_millis(250));
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": 2, "method": "model/list", "params": {"limit": 100}})
        )?;
        std::thread::sleep(Duration::from_secs(2));
    }
    drop(child.stdin.take());
    // `codex app-server` may keep running after we close stdin, so never block on it: wait up to a
    // short deadline, then kill it. We read whatever it already wrote either way.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error).context("Couldn't poll Codex model registry"),
        }
    }
    let mut stdout = Vec::new();
    if let Some(mut handle) = child.stdout.take() {
        use std::io::Read;
        let _ = handle.read_to_end(&mut stdout);
    }
    let response = String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message.get("id").and_then(Value::as_i64) == Some(2))
        .context("Codex model registry returned no response")?;
    parse_codex_model_response(&response)
}

fn parse_codex_model_response(response: &Value) -> Result<Vec<String>> {
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        anyhow::bail!("Codex model registry failed: {error}");
    }
    Ok(response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| !model.get("hidden").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|model| {
            model
                .get("id")
                .or_else(|| model.get("model"))
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
        .collect())
}

async fn translate_response(
    store: &Store,
    response: reqwest::Response,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
    key_id: &str,
    member: &SelectedMember,
) -> Response {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Never run a non-2xx upstream body through success translation — that fabricated an empty
    // assistant message (content:[]) with the upstream's error status, confusing the client.
    // Pass the real error body + status straight back so the client sees what actually failed.
    if !status.is_success() {
        let body = response.bytes().await.unwrap_or_default();
        let content_type = if content_type.is_empty() {
            "application/json".to_string()
        } else {
            content_type
        };
        return (status, [(header::CONTENT_TYPE, content_type)], body).into_response();
    }
    if content_type.contains("text/event-stream") {
        let recorder = UsageRecorder::new(store, model, key_id, member, provider);
        // Same protocol → pass bytes through untouched; otherwise translate each event. Either
        // way we sniff the upstream events for usage so the API report reflects real tokens
        // (recorded when the stream ends via the recorder's Drop).
        let translate = provider != client;
        let mut sniffer = StreamUsageSniffer::new(provider, client, model.to_string(), recorder);
        let stream = response.bytes_stream().map_ok(move |chunk| {
            if translate {
                Bytes::from(sniffer.push_translate(&chunk))
            } else {
                sniffer.push_passthrough(&chunk);
                chunk
            }
        });
        let header_content_type = if translate {
            HeaderValue::from_static("text/event-stream")
        } else {
            HeaderValue::from_str(&content_type)
                .unwrap_or_else(|_| HeaderValue::from_static("text/event-stream"))
        };
        return (
            status,
            [(header::CONTENT_TYPE, header_content_type)],
            Body::from_stream(
                stream.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err)),
            ),
        )
            .into_response();
    }
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(err) => {
            return api_error(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                format!("Upstream response failed: {err}"),
            )
        }
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            record_api_usage(store, model, key_id, member, TokenBreakdown::default());
            return (status, [(header::CONTENT_TYPE, content_type)], body).into_response();
        }
    };
    record_api_usage(
        store,
        model,
        key_id,
        member,
        tokens_from_usage(provider, &value),
    );
    let translated = translate_json_response(value, provider, client, model);
    (status, Json(translated)).into_response()
}

pub fn usage_report(store: &Store) -> ApiUsageReport {
    let mut report = read_api_usage(store);
    report.generated_at = chrono::Utc::now().to_rfc3339();
    report.total_requests = report.rows.iter().map(|row| row.requests).sum();
    report.total = report
        .rows
        .iter()
        .fold(TokenBreakdown::default(), |mut total, row| {
            total.add(&row.tokens);
            total
        });
    report
}

fn record_api_usage(
    store: &Store,
    combo_name: &str,
    key_id: &str,
    member: &SelectedMember,
    tokens: TokenBreakdown,
) {
    let account_id = member.account.id.clone();
    let mut report = read_api_usage(store);
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(row) = report.rows.iter_mut().find(|row| {
        row.combo_name == combo_name
            && row.key_id == key_id
            && row.account_id == account_id
            && row.tool_id == member.tool_id
    }) {
        row.requests = row.requests.saturating_add(1);
        row.tokens.add(&tokens);
        row.last_used_at = now;
    } else {
        report.rows.push(ApiUsageRow {
            combo_name: combo_name.to_string(),
            key_id: key_id.to_string(),
            account_id,
            tool_id: member.tool_id.clone(),
            requests: 1,
            tokens,
            last_used_at: now,
        });
    }
    report.total_requests = report.rows.iter().map(|row| row.requests).sum();
    report.total = report
        .rows
        .iter()
        .fold(TokenBreakdown::default(), |mut total, row| {
            total.add(&row.tokens);
            total
        });
    let _ = std::fs::write(
        store.api_usage_path(),
        serde_json::to_vec_pretty(&report).unwrap_or_default(),
    );
}

fn read_api_usage(store: &Store) -> ApiUsageReport {
    std::fs::read(store.api_usage_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| ApiUsageReport {
            generated_at: chrono::Utc::now().to_rfc3339(),
            total_requests: 0,
            total: TokenBreakdown::default(),
            rows: Vec::new(),
        })
}

fn tokens_from_usage(provider: ClientProtocol, value: &Value) -> TokenBreakdown {
    tokens_from_usage_obj(provider, value.get("usage"))
}

/// Extract a token breakdown from an already-located `usage` object (the shape differs
/// between non-streaming bodies, where it sits under `usage`, and streaming events, where
/// it is nested inside `message_start`/`message_delta`/`response.completed`).
fn tokens_from_usage_obj(provider: ClientProtocol, usage: Option<&Value>) -> TokenBreakdown {
    match provider {
        ClientProtocol::Anthropic => TokenBreakdown {
            input: usage
                .and_then(|usage| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read: usage
                .and_then(|usage| usage.get("cache_read_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_creation: usage
                .and_then(|usage| usage.get("cache_creation_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
        _ => TokenBreakdown {
            input: usage
                .and_then(|usage| {
                    usage
                        .get("input_tokens")
                        .or_else(|| usage.get("prompt_tokens"))
                })
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .and_then(|usage| {
                    usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens"))
                })
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read: usage
                .and_then(|usage| usage.get("cached_input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_creation: 0,
        },
    }
}

/// Locate the `usage` object inside a single streaming event, regardless of provider shape:
/// Anthropic `message_start` nests it under `/message/usage`; Anthropic `message_delta` and the
/// OpenAI Responses `response.completed` event nest it under `/response/usage` or top-level
/// `usage`. Returns `None` when the event carries no usage.
fn stream_event_usage(event: &Value) -> Option<&Value> {
    event
        .pointer("/message/usage")
        .or_else(|| event.pointer("/response/usage"))
        .or_else(|| event.get("usage"))
        .filter(|usage| usage.is_object())
}

/// Merge usage seen across multiple streaming events. Anthropic splits the numbers across
/// `message_start` (input + cache) and `message_delta` (final output), so we keep the max of
/// each field rather than summing — summing would double-count fields that repeat.
fn merge_stream_usage(acc: &mut TokenBreakdown, next: TokenBreakdown) {
    acc.input = acc.input.max(next.input);
    acc.output = acc.output.max(next.output);
    acc.cache_read = acc.cache_read.max(next.cache_read);
    acc.cache_creation = acc.cache_creation.max(next.cache_creation);
}

/// Records gateway usage exactly once for a single request. Holds the recording context so the
/// streaming code path can persist the tokens it sniffed when the stream finishes (or the client
/// disconnects), instead of recording a hardcoded zero up front.
struct UsageRecorder {
    store: Store,
    combo_name: String,
    key_id: String,
    member: SelectedMember,
    provider: ClientProtocol,
    tokens: TokenBreakdown,
    recorded: bool,
}

impl UsageRecorder {
    fn new(
        store: &Store,
        combo_name: &str,
        key_id: &str,
        member: &SelectedMember,
        provider: ClientProtocol,
    ) -> Self {
        Self {
            store: store.clone(),
            combo_name: combo_name.to_string(),
            key_id: key_id.to_string(),
            member: member.clone(),
            provider,
            tokens: TokenBreakdown::default(),
            recorded: false,
        }
    }

    /// Sniff a parsed upstream streaming event for usage and fold it into the accumulator.
    fn observe_event(&mut self, event: &Value) {
        if let Some(usage) = stream_event_usage(event) {
            merge_stream_usage(
                &mut self.tokens,
                tokens_from_usage_obj(self.provider, Some(usage)),
            );
        }
    }

    fn record_now(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        record_api_usage(
            &self.store,
            &self.combo_name,
            &self.key_id,
            &self.member,
            self.tokens,
        );
    }
}

impl Drop for UsageRecorder {
    fn drop(&mut self) {
        self.record_now();
    }
}

fn translate_json_response(
    value: Value,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
) -> Value {
    match (provider, client) {
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiChat) => {
            anthropic_json_to_chat(value, model)
        }
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses) => {
            anthropic_json_to_responses(value, model)
        }
        (ClientProtocol::OpenAiResponses, ClientProtocol::Anthropic) => {
            responses_json_to_anthropic(value, model)
        }
        (ClientProtocol::OpenAiResponses, ClientProtocol::OpenAiChat) => {
            responses_json_to_chat(value, model)
        }
        _ => value,
    }
}

fn anthropic_json_to_chat(value: Value, model: &str) -> Value {
    let text = anthropic_text(&value);
    let tool_calls = anthropic_tool_calls(&value);
    let mut message = json!({"role": "assistant", "content": text});
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::String(format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()))),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason(&value)}],
        "usage": anthropic_usage_to_openai(value.get("usage"))
    })
}

fn anthropic_json_to_responses(value: Value, model: &str) -> Value {
    let text = anthropic_text(&value);
    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
    }
    output.extend(anthropic_function_calls(&value));
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::String(format!("resp_{}", chrono::Utc::now().timestamp_millis()))),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "output": output,
        "usage": anthropic_usage_to_openai(value.get("usage"))
    })
}

fn responses_json_to_anthropic(value: Value, model: &str) -> Value {
    let text = responses_text(&value);
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    content.extend(responses_tool_uses(&value));
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::String(format!("msg_{}", chrono::Utc::now().timestamp_millis()))),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": if responses_has_tool_calls(&value) { "tool_use" } else { "end_turn" },
        "usage": openai_usage_to_anthropic(value.get("usage"))
    })
}

fn responses_json_to_chat(value: Value, model: &str) -> Value {
    let text = responses_text(&value);
    let tool_calls = responses_chat_tool_calls(&value);
    let mut message = json!({"role": "assistant", "content": text});
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": value.get("id").cloned().unwrap_or(Value::String(format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()))),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": if responses_has_tool_calls(&value) { "tool_calls" } else { "stop" }}],
        "usage": value.get("usage").cloned().unwrap_or(Value::Null)
    })
}

fn anthropic_tool_calls(value: &Value) -> Vec<Value> {
    value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|item| {
            json!({
                "id": item.get("id").cloned().unwrap_or(Value::String(String::new())),
                "type": "function",
                "function": {
                    "name": item.get("name").cloned().unwrap_or(Value::String(String::new())),
                    "arguments": item.get("input").cloned().unwrap_or_else(|| json!({})).to_string()
                }
            })
        })
        .collect()
}

fn anthropic_function_calls(value: &Value) -> Vec<Value> {
    value
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|item| {
            json!({
                "type": "function_call",
                "call_id": item.get("id").cloned().unwrap_or(Value::String(String::new())),
                "name": item.get("name").cloned().unwrap_or(Value::String(String::new())),
                "arguments": item.get("input").cloned().unwrap_or_else(|| json!({})).to_string()
            })
        })
        .collect()
}

fn response_output_items(value: &Value) -> impl Iterator<Item = &Value> {
    value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn responses_has_tool_calls(value: &Value) -> bool {
    response_output_items(value)
        .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
}

fn responses_tool_uses(value: &Value) -> Vec<Value> {
    response_output_items(value)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            json!({
                "type": "tool_use",
                "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::String(String::new())),
                "name": item.get("name").cloned().unwrap_or(Value::String(String::new())),
                "input": item.get("arguments").and_then(Value::as_str)
                    .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                    .unwrap_or_else(|| json!({}))
            })
        })
        .collect()
}

fn responses_chat_tool_calls(value: &Value) -> Vec<Value> {
    response_output_items(value)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(|item| {
            json!({
                "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(Value::String(String::new())),
                "type": "function",
                "function": {
                    "name": item.get("name").cloned().unwrap_or(Value::String(String::new())),
                    "arguments": item.get("arguments").cloned().unwrap_or(Value::String("{}".to_string()))
                }
            })
        })
        .collect()
}

fn anthropic_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn responses_text(value: &Value) -> String {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return text.to_string();
    }
    value
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|part| {
                    part.get("text")
                        .or_else(|| part.get("output_text"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn finish_reason(value: &Value) -> Value {
    match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Value::String("length".to_string()),
        Some(_) => Value::String("stop".to_string()),
        None => Value::Null,
    }
}

fn anthropic_usage_to_openai(usage: Option<&Value>) -> Value {
    let input = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output})
}

fn openai_usage_to_anthropic(usage: Option<&Value>) -> Value {
    json!({
        "input_tokens": usage.and_then(|usage| usage.get("input_tokens").or_else(|| usage.get("prompt_tokens"))).and_then(Value::as_u64).unwrap_or(0),
        "output_tokens": usage.and_then(|usage| usage.get("output_tokens").or_else(|| usage.get("completion_tokens"))).and_then(Value::as_u64).unwrap_or(0)
    })
}

/// Buffers an upstream SSE byte stream into complete lines, sniffs each `data:` event for usage
/// (folding it into the `UsageRecorder`), and — when client and provider protocols differ —
/// rewrites each event into the client's protocol. Usage is recorded when this struct is dropped,
/// which covers both normal completion and a client disconnecting mid-stream.
struct StreamUsageSniffer {
    pending: Vec<u8>,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: String,
    recorder: UsageRecorder,
}

impl StreamUsageSniffer {
    fn new(
        provider: ClientProtocol,
        client: ClientProtocol,
        model: String,
        recorder: UsageRecorder,
    ) -> Self {
        Self {
            pending: Vec::new(),
            provider,
            client,
            model,
            recorder,
        }
    }

    /// Iterate over the complete lines currently buffered, invoking `on_line` for each. Leaves any
    /// trailing partial line in the buffer for the next chunk.
    fn for_each_line(&mut self, chunk: &[u8], mut on_line: impl FnMut(&str)) {
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            on_line(&String::from_utf8_lossy(&line));
        }
    }

    /// Passthrough mode (same protocol): only sniff usage, bytes are forwarded unchanged.
    fn push_passthrough(&mut self, chunk: &[u8]) {
        let mut events = Vec::new();
        self.for_each_line(chunk, |line| {
            if let Some(value) = parse_sse_data_line(line) {
                events.push(value);
            }
        });
        for event in &events {
            self.recorder.observe_event(event);
        }
    }

    /// Translate mode (cross-protocol): sniff usage and rewrite each event for the client.
    fn push_translate(&mut self, chunk: &[u8]) -> String {
        let provider = self.provider;
        let client = self.client;
        let model = self.model.clone();
        let mut out = String::new();
        let mut events = Vec::new();
        self.for_each_line(chunk, |line| {
            out.push_str(&translate_sse_line(line, provider, client, &model, &mut events));
        });
        for event in &events {
            self.recorder.observe_event(event);
        }
        out
    }
}

/// Parse a single SSE line into its `data:` JSON payload, if any. Returns `None` for `event:`
/// lines, comments, blank lines, `[DONE]`, and malformed JSON.
fn parse_sse_data_line(line: &str) -> Option<Value> {
    let data = line.strip_prefix("data:")?.trim();
    if data == "[DONE]" {
        return None;
    }
    serde_json::from_str::<Value>(data).ok()
}

fn translate_sse_line(
    line: &str,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
    seen_events: &mut Vec<Value>,
) -> String {
    let mut out = String::new();
    {
        if !line.starts_with("data:") {
            if !line.starts_with("event:") {
                out.push_str(line);
                out.push('\n');
            }
            return out;
        }
        let data = line.trim_start_matches("data:").trim();
        if data == "[DONE]" {
            out.push_str("data: [DONE]\n\n");
            return out;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return out;
        };
        // Keep only events that actually carry usage, so the usage sniffer can fold them in
        // without us cloning every text-delta event.
        if stream_event_usage(&value).is_some() {
            seen_events.push(value.clone());
        }
        let translated = translate_stream_event(value, provider, client, model);
        if !translated.is_null() {
            out.push_str("data: ");
            out.push_str(&translated.to_string());
            out.push_str("\n\n");
        }
    }
    out
}

fn translate_stream_event(
    value: Value,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
) -> Value {
    if let Some(event) = translate_tool_stream_event(&value, provider, client, model) {
        return event;
    }
    if let Some(event) = translate_terminal_stream_event(&value, provider, client, model) {
        return event;
    }
    let text = stream_delta_text(&value);
    if text.is_empty() {
        return Value::Null;
    }
    match (provider, client) {
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiChat)
        | (ClientProtocol::OpenAiResponses, ClientProtocol::OpenAiChat) => json!({
            "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        }),
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses) => json!({
            "type": "response.output_text.delta",
            "delta": text
        }),
        (ClientProtocol::OpenAiResponses, ClientProtocol::Anthropic) => json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
        _ => value,
    }
}

fn translate_tool_stream_event(
    value: &Value,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
) -> Option<Value> {
    let event_type = value.get("type").and_then(Value::as_str)?;
    match (provider, client, event_type) {
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiChat, "content_block_start")
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") =>
        {
            Some(json!({
                "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": value.get("index").and_then(Value::as_u64).unwrap_or(0),
                        "id": value.pointer("/content_block/id").cloned().unwrap_or(Value::String(String::new())),
                        "type": "function",
                        "function": {
                            "name": value.pointer("/content_block/name").cloned().unwrap_or(Value::String(String::new())),
                            "arguments": ""
                        }
                    }]},
                    "finish_reason": null
                }]
            }))
        }
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiChat, "content_block_delta")
            if value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta") =>
        {
            Some(json!({
                "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": value.get("index").and_then(Value::as_u64).unwrap_or(0),
                        "function": {
                            "arguments": value.pointer("/delta/partial_json").cloned().unwrap_or(Value::String(String::new()))
                        }
                    }]},
                    "finish_reason": null
                }]
            }))
        }
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses, "content_block_start")
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") =>
        {
            Some(json!({
                "type": "response.output_item.added",
                "output_index": value.get("index").and_then(Value::as_u64).unwrap_or(0),
                "item": {
                    "type": "function_call",
                    "call_id": value.pointer("/content_block/id").cloned().unwrap_or(Value::String(String::new())),
                    "name": value.pointer("/content_block/name").cloned().unwrap_or(Value::String(String::new())),
                    "arguments": ""
                }
            }))
        }
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses, "content_block_delta")
            if value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta") =>
        {
            Some(json!({
                "type": "response.function_call_arguments.delta",
                "output_index": value.get("index").and_then(Value::as_u64).unwrap_or(0),
                "delta": value.pointer("/delta/partial_json").cloned().unwrap_or(Value::String(String::new()))
            }))
        }
        (
            ClientProtocol::OpenAiResponses,
            ClientProtocol::Anthropic,
            "response.output_item.added",
        ) if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") => {
            Some(json!({
                "type": "content_block_start",
                "index": value.get("output_index").and_then(Value::as_u64).unwrap_or(0),
                "content_block": {
                    "type": "tool_use",
                    "id": value.pointer("/item/call_id").or_else(|| value.pointer("/item/id")).cloned().unwrap_or(Value::String(String::new())),
                    "name": value.pointer("/item/name").cloned().unwrap_or(Value::String(String::new())),
                    "input": {}
                }
            }))
        }
        (
            ClientProtocol::OpenAiResponses,
            ClientProtocol::Anthropic,
            "response.function_call_arguments.delta",
        ) => Some(json!({
            "type": "content_block_delta",
            "index": value.get("output_index").and_then(Value::as_u64).unwrap_or(0),
            "delta": {
                "type": "input_json_delta",
                "partial_json": value.get("delta").cloned().unwrap_or(Value::String(String::new()))
            }
        })),
        (
            ClientProtocol::OpenAiResponses,
            ClientProtocol::OpenAiChat,
            "response.output_item.added",
        ) if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") => {
            Some(json!({
                "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": value.get("output_index").and_then(Value::as_u64).unwrap_or(0),
                        "id": value.pointer("/item/call_id").or_else(|| value.pointer("/item/id")).cloned().unwrap_or(Value::String(String::new())),
                        "type": "function",
                        "function": {
                            "name": value.pointer("/item/name").cloned().unwrap_or(Value::String(String::new())),
                            "arguments": ""
                        }
                    }]},
                    "finish_reason": null
                }]
            }))
        }
        (
            ClientProtocol::OpenAiResponses,
            ClientProtocol::OpenAiChat,
            "response.function_call_arguments.delta",
        ) => Some(json!({
            "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": value.get("output_index").and_then(Value::as_u64).unwrap_or(0),
                    "function": {
                        "arguments": value.get("delta").cloned().unwrap_or(Value::String(String::new()))
                    }
                }]},
                "finish_reason": null
            }]
        })),
        _ => None,
    }
}

fn translate_terminal_stream_event(
    value: &Value,
    provider: ClientProtocol,
    client: ClientProtocol,
    model: &str,
) -> Option<Value> {
    let event_type = value.get("type").and_then(Value::as_str)?;
    match (provider, client, event_type) {
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiChat, "message_stop") => Some(json!({
            "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })),
        (ClientProtocol::Anthropic, ClientProtocol::OpenAiResponses, "message_stop") => {
            Some(json!({"type": "response.completed"}))
        }
        (ClientProtocol::OpenAiResponses, ClientProtocol::Anthropic, "response.completed") => {
            Some(json!({"type": "message_stop"}))
        }
        (ClientProtocol::OpenAiResponses, ClientProtocol::OpenAiChat, "response.completed") => {
            Some(json!({
                "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            }))
        }
        _ => None,
    }
}

fn stream_delta_text(value: &Value) -> String {
    value
        .pointer("/delta/text")
        .or_else(|| value.pointer("/delta/content"))
        .or_else(|| value.pointer("/choices/0/delta/content"))
        .or_else(|| value.get("delta"))
        .and_then(Value::as_str)
        .or_else(|| value.get("text").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn exhausted_response(model: &str) -> Response {
    api_error(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limit_error",
        format!(
            "All accounts in combo '{model}' are exhausted or cooling down. Retry after quota reset."
        ),
    )
}

fn quota_percent(account: &Account) -> f64 {
    account.quota.as_ref().map_or(0.0, |quota| {
        if quota.error.is_some() {
            return 0.0;
        }
        [quota.five_hour.percent_used, quota.weekly.percent_used]
            .into_iter()
            .flatten()
            .fold(0.0_f64, f64::max)
    })
}

fn account_config_dir(store: &Store, data: &StoredState, account: &Account) -> std::path::PathBuf {
    if account.fingerprint.starts_with("profile:") {
        store.account_dir(&account.tool_id, &account.id)
    } else {
        data.tool_setups
            .get(account.tool_id.as_str())
            .and_then(|setup| setup.default_config_dir.clone())
            .unwrap_or_else(|| default_config_dir(&account.tool_id))
    }
}

fn model_not_found(model: &str) -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "not_found_error",
        format!("Model '{model}' not found"),
    )
}

fn api_error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({
            "error": {
                "type": kind,
                "message": message.into()
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ApiGatewayKey, QuotaInfo, QuotaWindow};

    fn quota(five: f64, weekly: f64) -> QuotaInfo {
        QuotaInfo {
            five_hour: QuotaWindow {
                label: "5-hour limit".to_string(),
                percent_used: Some(five),
                reset_at: None,
            },
            weekly: QuotaWindow {
                label: "Weekly limit".to_string(),
                percent_used: Some(weekly),
                reset_at: None,
            },
            models: None,
            plan: None,
            updated_at: None,
            error: None,
        }
    }

    fn account(tool_id: ToolId, id: &str, percent: f64) -> Account {
        Account {
            id: id.to_string(),
            tool_id,
            name: id.to_string(),
            state: AccountState::Idle,
            fingerprint: format!("profile:{id}"),
            created_at: "2026-06-14T00:00:00Z".to_string(),
            updated_at: "2026-06-14T00:00:00Z".to_string(),
            last_used_at: None,
            quota: Some(quota(percent, percent)),
            launcher_command: None,
            is_default: false,
            avatar_url: None,
            api_provider: None,
        }
    }

    fn member(tool_id: ToolId, account_id: &str, model: &str) -> SelectedMember {
        SelectedMember {
            tool_id: tool_id.clone(),
            model: model.to_string(),
            account: account(tool_id.clone(), account_id, 0.0),
            key: account_key(&tool_id, account_id),
        }
    }

    fn combo(name: &str, members: &[&str]) -> ApiGatewayCombo {
        ApiGatewayCombo {
            id: format!("combo-{name}"),
            name: name.to_string(),
            members: members.iter().map(|model| model.to_string()).collect(),
            strategy: None,
            enabled: true,
            created_at: "2026-06-14T00:00:00Z".to_string(),
            updated_at: "2026-06-14T00:00:00Z".to_string(),
        }
    }

    /// Register a provider's supported models so `provider_for_model` can route them in tests.
    fn registry(tool_id: ToolId, account_id: &str, models: &[&str]) -> ApiGatewayModelRegistry {
        ApiGatewayModelRegistry {
            tool_id,
            account_id: account_id.to_string(),
            models: models.iter().map(|model| model.to_string()).collect(),
            updated_at: "2026-06-14T00:00:00Z".to_string(),
            error: None,
        }
    }

    #[test]
    fn openai_chat_to_anthropic_preserves_system_and_text_parts() {
        let translated = openai_chat_to_anthropic(json!({
            "model": "pool-model",
            "messages": [
                {"role": "system", "content": "Use terse answers."},
                {"role": "user", "content": [
                    {"type": "input_text", "input_text": "hello"},
                    {"type": "text", "text": "world"}
                ]},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"id\":42}"}
                }]}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Find an item",
                    "parameters": {"type": "object", "properties": {"id": {"type": "number"}}}
                }
            }],
            "max_completion_tokens": 123
        }));

        assert_eq!(translated["system"], "Use terse answers.");
        assert_eq!(translated["max_tokens"], 123);
        assert_eq!(translated["messages"][0]["role"], "user");
        assert_eq!(
            translated["messages"][0]["content"][0]["text"],
            "hello\nworld"
        );
        assert_eq!(translated["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(translated["messages"][1]["content"][0]["input"]["id"], 42);
        assert_eq!(translated["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn responses_json_to_chat_reads_output_text_parts() {
        let translated = responses_json_to_chat(
            json!({
                "id": "resp_1",
                "output": [
                    {
                        "type": "message",
                        "content": [
                            {"type": "output_text", "text": "Hello "},
                            {"type": "output_text", "output_text": "there"}
                        ]
                    },
                    {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "lookup",
                        "arguments": "{\"id\":42}"
                    }
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
            "pool-model",
        );

        assert_eq!(
            translated["choices"][0]["message"]["content"],
            "Hello there"
        );
        assert_eq!(translated["model"], "pool-model");
        assert_eq!(
            translated["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "lookup"
        );
        assert_eq!(translated["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn anthropic_tool_use_translates_to_openai_responses() {
        let translated = anthropic_json_to_responses(
            json!({
                "id": "msg_1",
                "content": [{
                    "type": "tool_use",
                    "id": "tool_1",
                    "name": "search",
                    "input": {"query": "rust"}
                }],
                "usage": {"input_tokens": 3, "output_tokens": 2}
            }),
            "pool-model",
        );
        assert_eq!(translated["output"][0]["type"], "function_call");
        assert_eq!(translated["output"][0]["name"], "search");
        assert_eq!(translated["output"][0]["arguments"], r#"{"query":"rust"}"#);
    }

    #[test]
    fn stream_delta_text_supports_openai_chat_chunks() {
        let text = stream_delta_text(&json!({
            "choices": [{"delta": {"content": "token"}}]
        }));
        assert_eq!(text, "token");
    }

    #[test]
    fn accepts_bearer_and_anthropic_api_key_headers() {
        let mut bearer_headers = HeaderMap::new();
        bearer_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-bearer"),
        );
        assert_eq!(bearer_token(&bearer_headers).as_deref(), Some("sk-bearer"));

        let mut anthropic_headers = HeaderMap::new();
        anthropic_headers.insert("x-api-key", HeaderValue::from_static("sk-anthropic"));
        assert_eq!(
            bearer_token(&anthropic_headers).as_deref(),
            Some("sk-anthropic")
        );
    }

    fn test_sniffer(
        provider: ClientProtocol,
        client: ClientProtocol,
    ) -> (StreamUsageSniffer, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("aisw-api-sniffer-{}", uuid::Uuid::new_v4()));
        let store = Store::for_test(root.clone()).unwrap();
        let recorder = UsageRecorder::new(
            &store,
            "pool-model",
            "key-1",
            &member(ToolId::Claude, "a1", "claude-real"),
            provider,
        );
        (
            StreamUsageSniffer::new(provider, client, "pool-model".to_string(), recorder),
            root,
        )
    }

    #[test]
    fn sse_translator_buffers_split_json_lines() {
        let (mut sniffer, root) =
            test_sniffer(ClientProtocol::Anthropic, ClientProtocol::OpenAiChat);
        let first = sniffer.push_translate(
            br#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","te"#,
        );
        assert!(first.is_empty());

        let second = sniffer.push_translate(
            br#"xt":"hello"}}

"#,
        );
        assert!(second.contains(r#""content":"hello""#));
        assert!(second.contains(r#""object":"chat.completion.chunk""#));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn streaming_records_usage_from_anthropic_events() {
        // Anthropic splits usage: input + cache in message_start, output in message_delta.
        let (mut sniffer, root) =
            test_sniffer(ClientProtocol::Anthropic, ClientProtocol::OpenAiChat);
        sniffer.push_translate(
            br#"data: {"type":"message_start","message":{"usage":{"input_tokens":120,"cache_read_input_tokens":40,"output_tokens":1}}}

data: {"type":"message_delta","usage":{"output_tokens":77}}

data: {"type":"message_stop"}

"#,
        );
        let store = sniffer.recorder.store.clone();
        drop(sniffer); // records on drop
        let report = read_api_usage(&store);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].tokens.input, 120);
        assert_eq!(report.rows[0].tokens.cache_read, 40);
        assert_eq!(report.rows[0].tokens.output, 77);
        assert_eq!(report.rows[0].requests, 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn streaming_passthrough_records_responses_usage() {
        // Native Codex (OpenAI Responses) passthrough: usage rides in response.completed.
        let (mut sniffer, root) = test_sniffer(
            ClientProtocol::OpenAiResponses,
            ClientProtocol::OpenAiResponses,
        );
        sniffer.push_passthrough(
            br#"data: {"type":"response.output_text.delta","delta":"hi"}

data: {"type":"response.completed","response":{"usage":{"input_tokens":13,"output_tokens":17,"cached_input_tokens":4}}}

"#,
        );
        let store = sniffer.recorder.store.clone();
        drop(sniffer);
        let report = read_api_usage(&store);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].tokens.input, 13);
        assert_eq!(report.rows[0].tokens.output, 17);
        assert_eq!(report.rows[0].tokens.cache_read, 4);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn streaming_tool_call_translates_both_directions() {
        let chat_start = translate_stream_event(
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "tool_1",
                    "name": "search",
                    "input": {}
                }
            }),
            ClientProtocol::Anthropic,
            ClientProtocol::OpenAiChat,
            "pool-model",
        );
        assert_eq!(
            chat_start["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "search"
        );

        let anthropic_delta = translate_stream_event(
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "{\"query\":\"rust\"}"
            }),
            ClientProtocol::OpenAiResponses,
            ClientProtocol::Anthropic,
            "pool-model",
        );
        assert_eq!(
            anthropic_delta["delta"]["partial_json"],
            "{\"query\":\"rust\"}"
        );
    }

    #[test]
    fn token_extraction_supports_anthropic_and_openai_usage() {
        let anthropic = tokens_from_usage(
            ClientProtocol::Anthropic,
            &json!({
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 7,
                    "cache_read_input_tokens": 5,
                    "cache_creation_input_tokens": 3
                }
            }),
        );
        assert_eq!(anthropic.input, 11);
        assert_eq!(anthropic.output, 7);
        assert_eq!(anthropic.cache_read, 5);
        assert_eq!(anthropic.cache_creation, 3);

        let openai = tokens_from_usage(
            ClientProtocol::OpenAiResponses,
            &json!({
                "usage": {
                    "prompt_tokens": 13,
                    "completion_tokens": 17,
                    "cached_input_tokens": 19
                }
            }),
        );
        assert_eq!(openai.input, 13);
        assert_eq!(openai.output, 17);
        assert_eq!(openai.cache_read, 19);
    }

    #[test]
    fn selects_round_robin_with_affinity_and_skips_unavailable_accounts() {
        let root = std::env::temp_dir().join(format!("aisw-api-selector-{}", uuid::Uuid::new_v4()));
        let store = Store::for_test(root.clone()).unwrap();
        let state = GatewayState {
            store,
            client: reqwest::Client::new(),
            runtime: Mutex::new(GatewayRuntime::default()),
        };
        let mut data = StoredState::default();
        data.api_gateway.quota_threshold = 95.0;
        data.accounts = vec![
            account(ToolId::Claude, "a1", 20.0),
            account(ToolId::Claude, "a2", 30.0),
            account(ToolId::Claude, "a3", 99.0),
        ];
        // One combo, one member model that all three Claude accounts serve → round-robin rotates
        // across the accounts. The over-quota account (a3) is skipped.
        data.api_gateway.model_registry = vec![
            registry(ToolId::Claude, "a1", &["claude-1"]),
            registry(ToolId::Claude, "a2", &["claude-1"]),
            registry(ToolId::Claude, "a3", &["claude-1"]),
        ];
        let combo = combo("local", &["claude-1"]);
        data.api_gateway.combos.push(combo.clone());
        state.store.save(&data).unwrap();
        let tried = HashSet::new();

        let first = select_member(&state, &data, &combo, "s1", &tried).unwrap();
        let second = select_member(&state, &data, &combo, "s2", &tried).unwrap();
        assert_eq!(first.account.id, "a1");
        assert_eq!(second.account.id, "a2");

        let a2_key = account_key(&ToolId::Claude, "a2");
        bind_session(&state, "sticky", &a2_key);
        let sticky = select_member(&state, &data, &combo, "sticky", &tried).unwrap();
        assert_eq!(sticky.account.id, "a2");

        mark_cooldown(&state, &a2_key);
        let persisted = state.store.load().unwrap();
        let a2_entry = persisted
            .api_gateway
            .accounts
            .iter()
            .find(|entry| entry.tool_id == ToolId::Claude && entry.account_id == "a2")
            .unwrap();
        assert_eq!(a2_entry.state, crate::models::ApiPoolAccountState::CoolingDown);
        let after_cooldown = select_member(&state, &data, &combo, "sticky", &tried).unwrap();
        assert_eq!(after_cooldown.account.id, "a1");

        state.runtime.lock().unwrap().cooldowns.clear();
        state.runtime.lock().unwrap().affinity.clear();
        data.api_gateway.rotation_strategy = ApiRotationStrategy::FillFirst;
        let fill_first = select_member(&state, &data, &combo, "fill", &tried).unwrap();
        assert_eq!(fill_first.account.id, "a1");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn anthropic_to_responses_sets_top_level_instructions() {
        // Codex's responses endpoint requires `instructions`; the Anthropic system prompt must be
        // hoisted there, not left as a system item inside `input`.
        let out = anthropic_to_responses(json!({
            "model": "gpt-5-codex",
            "system": [{"type": "text", "text": "Be terse."}],
            "messages": [{"role": "user", "content": "hi"}]
        }));
        assert_eq!(out["instructions"], "Be terse.");
        // No system role should leak into input.
        let has_system = out["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.get("role").and_then(Value::as_str) == Some("system"));
        assert!(!has_system);

        // Missing system → a non-empty default instruction (the endpoint rejects an empty one).
        let out2 = anthropic_to_responses(json!({
            "model": "gpt-5-codex",
            "messages": [{"role": "user", "content": "hi"}]
        }));
        assert!(out2["instructions"].as_str().is_some_and(|s| !s.is_empty()));

        // OpenAI-chat → responses also hoists the system message into instructions.
        let out3 = openai_chat_to_responses(json!({
            "model": "gpt-5-codex",
            "messages": [
                {"role": "system", "content": "Rules here."},
                {"role": "user", "content": "hi"}
            ]
        }));
        assert_eq!(out3["instructions"], "Rules here.");
        assert_eq!(out3["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sanitize_codex_body_sets_required_responses_fields() {
        let mut body = json!({
            "model": "gpt-5-codex",
            "input": [{"role": "user", "content": "hi"}],
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100,
            "temperature": 0.5
        });
        sanitize_codex_body(&mut body);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body["instructions"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(body.get("messages").is_none());
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("temperature").is_none());

        // A caller-supplied stream choice is preserved.
        let mut streamed = json!({"input": [], "stream": false, "instructions": "x"});
        sanitize_codex_body(&mut streamed);
        assert_eq!(streamed["stream"], false);
        assert_eq!(streamed["instructions"], "x");
    }

    #[test]
    fn sanitize_anthropic_body_drops_unknown_client_fields() {
        let mut body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100,
            "stream": true,
            "context_management": {"edits": []},
            "anthropic_beta": ["x"],
            "betas": ["y"]
        });
        sanitize_anthropic_body(&mut body);
        assert!(body.get("context_management").is_none());
        assert!(body.get("anthropic_beta").is_none());
        assert!(body.get("betas").is_none());
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["stream"], true);
        assert!(body.get("messages").is_some());
    }

    #[test]
    fn resolves_combo_name_before_direct_model() {
        let mut data = StoredState::default();
        data.accounts = vec![
            account(ToolId::Claude, "a1", 0.0),
            account(ToolId::Codex, "a2", 0.0),
        ];
        data.api_gateway.model_registry = vec![
            registry(ToolId::Claude, "a1", &["claude-real"]),
            registry(ToolId::Codex, "a2", &["gpt-real"]),
        ];
        data.api_gateway.combos = vec![combo("smart-combo", &["claude-real", "gpt-real"])];

        let by_name = resolve_combo(&data, "smart-combo").unwrap();
        assert_eq!(by_name.id, "combo-smart-combo");
        assert_eq!(by_name.members.len(), 2);

        let direct = resolve_combo(&data, "gpt-real").unwrap();
        assert_eq!(direct.id, "direct:gpt-real");
        assert_eq!(direct.members, vec!["gpt-real".to_string()]);

        // Unknown model with no provider support → no combo.
        assert!(resolve_combo(&data, "mystery-model").is_none());
    }

    #[tokio::test]
    async fn serves_health_and_authenticated_models() {
        let root = std::env::temp_dir().join(format!("aisw-api-gateway-{}", uuid::Uuid::new_v4()));
        let store = Store::for_test(root.clone()).unwrap();
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut data = StoredState::default();
        data.api_gateway.port = port;
        data.api_gateway.keys.push(ApiGatewayKey {
            id: "key-1".to_string(),
            name: "test".to_string(),
            secret: Some("sk-test".to_string()),
            prefix: "sk-...test".to_string(),
            enabled: true,
            expires_at: None,
            created_at: "2026-06-14T00:00:00Z".to_string(),
        });
        data.api_gateway.combos.push(combo("smart-combo", &["claude-real"]));
        data.api_gateway.model_registry.push(ApiGatewayModelRegistry {
            tool_id: ToolId::Codex,
            account_id: "x1".to_string(),
            models: vec!["gpt-registry".to_string(), "claude-real".to_string()],
            updated_at: "2026-06-14T00:00:00Z".to_string(),
            error: None,
        });
        store.save(&data).unwrap();

        let mut server = start_server(store, data.api_gateway.clone()).unwrap();
        let client = reqwest::Client::new();
        let root_url = format!("http://127.0.0.1:{port}");

        let health = client
            .get(format!("{root_url}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::OK);

        let unauthorized = client
            .get(format!("{root_url}/v1/models"))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let models = client
            .get(format!("{root_url}/v1/models"))
            .header("x-api-key", "sk-test")
            .send()
            .await
            .unwrap();
        assert_eq!(models.status(), reqwest::StatusCode::OK);
        let models: Value = models.json().await.unwrap();
        let ids = models["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["smart-combo", "gpt-registry", "claude-real"]);

        server.stop(&data.api_gateway);
        let mut restarted = start_server(
            Store::for_test(root.clone()).unwrap(),
            data.api_gateway.clone(),
        )
        .unwrap();
        let health_after_restart = reqwest::Client::new()
            .get(format!("{root_url}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(health_after_restart.status(), reqwest::StatusCode::OK);
        restarted.stop(&data.api_gateway);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parses_visible_codex_models() {
        let response = json!({
            "id": 2,
            "result": {
                "data": [
                    {"id": "gpt-5.5", "hidden": false},
                    {"model": "gpt-5.4-mini"},
                    {"id": "internal", "hidden": true}
                ]
            }
        });
        assert_eq!(
            parse_codex_model_response(&response).unwrap(),
            vec!["gpt-5.5", "gpt-5.4-mini"]
        );
    }
}
