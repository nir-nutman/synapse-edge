use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use axum::http::{HeaderMap, Response, StatusCode};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::triage::{triage, count_messages, extract_prompt_text};

pub const OPENAI_CHAT_PATH: &str = "/v1/chat/completions";

#[derive(Clone)]
pub struct AppState {
    pub client: reqwest::Client,
    pub local_url: String,
    pub cloud_url: String,
    pub cloud_api_key: Option<String>,
}

pub async fn chat_completions(
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<Value>,
) -> Response<Body> {
    let started = Instant::now();
    let prompt = extract_prompt_text(&payload);
    let message_count = count_messages(&payload);
    let decision = triage(&prompt, message_count);
    let prefer_local = decision.prefer_local;

    if prefer_local {
        info!(reason = %decision.reason, "Triage decision: LOCAL");
        match send_request(&state.client, &state.local_url, &payload, None).await {
            Ok(upstream) => {
                let latency_ms = started.elapsed().as_millis();
                let estimated_cost_saved_usd = 0.01_f64;
                info!(
                    decision = "local",
                    estimated_cost_saved_usd,
                    latency_ms,
                    "GreenOps telemetry"
                );
                return upstream_response(upstream);
            }
            Err(err) if err.is_connect() => {
                warn!("⚠️ Local node unreachable. Autonomous failover triggered.");
                match send_request(
                    &state.client,
                    &state.cloud_url,
                    &payload,
                    state.cloud_api_key.as_deref(),
                )
                .await
                {
                    Ok(upstream) => {
                        let latency_ms = started.elapsed().as_millis();
                        info!(
                            decision = "cloud_failover",
                            estimated_cost_saved_usd = 0.0_f64,
                            latency_ms,
                            "GreenOps telemetry"
                        );
                        return upstream_response(upstream);
                    }
                    Err(cloud_err) => {
                        error!(error = %cloud_err, "Cloud failover request failed");
                        return upstream_error_response(cloud_err);
                    }
                }
            }
            Err(err) => {
                error!(error = %err, "Local request failed");
                return upstream_error_response(err);
            }
        }
    }

    info!(reason = %decision.reason, "Triage decision: CLOUD");
    match send_request(
        &state.client,
        &state.cloud_url,
        &payload,
        state.cloud_api_key.as_deref(),
    )
    .await
    {
        Ok(upstream) => {
            let latency_ms = started.elapsed().as_millis();
            info!(
                decision = "cloud",
                estimated_cost_saved_usd = 0.0_f64,
                latency_ms,
                "GreenOps telemetry"
            );
            upstream_response(upstream)
        }
        Err(err) => {
            error!(error = %err, "Cloud request failed");
            upstream_error_response(err)
        }
    }
}

async fn send_request(
    client: &reqwest::Client,
    base_url: &str,
    payload: &Value,
    bearer_token: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = build_target_url(base_url);
    let mut request = client.post(url).json(payload);

    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }

    request.send().await
}

fn build_target_url(base_url: &str) -> String {
    if base_url.ends_with(OPENAI_CHAT_PATH) {
        return base_url.to_string();
    }

    format!("{}{}", base_url.trim_end_matches('/'), OPENAI_CHAT_PATH)
}

fn upstream_response(upstream: reqwest::Response) -> Response<Body> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream.bytes_stream();

    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(stream))
        .expect("response building should not fail");

    copy_headers(&headers, response.headers_mut());
    response
}

fn copy_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (name, value) in from {
        if name == HOST || name == CONTENT_LENGTH || name == CONNECTION || name == TRANSFER_ENCODING {
            continue;
        }
        to.insert(name, value.clone());
    }
}

fn upstream_error_response(err: reqwest::Error) -> Response<Body> {
    let status = if err.is_timeout() {
        StatusCode::GATEWAY_TIMEOUT
    } else if err.is_connect() {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::BAD_GATEWAY
    };

    let message = serde_json::json!({
        "error": {
            "message": format!("Upstream request failed: {err}"),
            "type": "upstream_error"
        }
    });

    let response = Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(message.to_string()))
        .expect("Failed to build error response");
    response
}
