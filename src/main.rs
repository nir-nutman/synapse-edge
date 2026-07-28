use std::env;
use std::net::SocketAddr;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use dotenvy::dotenv;
use reqwest::Client;
use serde_json::Value;
use tokio::net::TcpListener;
use tracing::Level;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const OPENAI_CHAT_PATH: &str = "/v1/chat/completions";

#[derive(Clone)]
struct AppState {
    client: Client,
    local_url: String,
    cloud_url: String,
    cloud_api_key: Option<String>,
}

#[tokio::main]
async fn main() {
    let _ = dotenv();

    // Write logs to ./logs/synapse-edge.log (rotated daily) AND to the terminal.
    // The `_log_guard` must stay alive for the whole program — dropping it early
    // would stop the background thread that flushes logs to disk.
    let file_appender = tracing_appender::rolling::daily("logs", "synapse-edge.log");
    let (file_writer, _log_guard) = tracing_appender::non_blocking(file_appender);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(false) // no color codes in the log file
        .with_writer(file_writer);

    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::from_level(Level::INFO))
        .with(stdout_layer)
        .with(file_layer)
        .init();

    let local_url = env::var("LOCAL_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let cloud_url = env::var("CLOUD_URL").unwrap_or_else(|_| "https://api.openai.com".to_string());
    let cloud_api_key = env::var("CLOUD_API_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    let state = AppState {
        client: Client::new(),
        local_url,
        cloud_url,
        cloud_api_key,
    };

    let app = Router::new()
        .route(OPENAI_CHAT_PATH, post(chat_completions))
        .with_state(state);

    let addr: SocketAddr = bind_addr
        .parse()
        .unwrap_or_else(|_| "0.0.0.0:3000".parse().expect("valid default bind address"));

    info!(%addr, "Synapse-Edge Phase 1 proxy started");

    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");
    axum::serve(listener, app)
        .await
        .expect("server exited unexpectedly");
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Response<Body> {
    // 2026 systemic directive: triage by lightweight local-first heuristics to reduce inference cost.
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
                // 2026 reliability directive: autonomous self-healing failover on local connectivity faults.
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
    client: &Client,
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

/// The result of triage: where to send the request, and *why*.
///
/// Carrying the `reason` alongside the boolean is what powers the SPEC's
/// "Decision Provenance" goal — every routing choice can explain itself.
struct TriageDecision {
    prefer_local: bool,
    reason: String,
}

/// Phrases that usually signal a hard / expensive request the cloud handles better.
/// `&'static str` means these string slices live for the whole program (they're baked
/// into the binary), so we never allocate or free them.
const HARD_SIGNALS: [&str; 8] = [
    "explain",
    "write code",
    "debug",
    "step by step",
    "analyze",
    "prove",
    "refactor",
    "in detail",
];

/// Decide whether a prompt is "easy" enough to answer on the local model.
///
/// Heuristic (Phase 1): a prompt is easy if it is short, part of a short
/// conversation, and contains none of the `HARD_SIGNALS` keywords.
fn triage(prompt: &str, message_count: usize) -> TriageDecision {
    let word_count = prompt.split_whitespace().count();
    let lowered = prompt.to_lowercase();

    // `.iter().any(..)` returns true if the closure is true for ANY keyword.
    let hard_signal = HARD_SIGNALS.iter().find(|kw| lowered.contains(*kw));

    let easy = word_count <= 30 && message_count <= 2 && hard_signal.is_none();

    if easy {
        TriageDecision {
            prefer_local: true,
            reason: format!("easy: {word_count} words, no complex keywords"),
        }
    } else if let Some(kw) = hard_signal {
        TriageDecision {
            prefer_local: false,
            reason: format!("complex: detected keyword \"{kw}\""),
        }
    } else {
        TriageDecision {
            prefer_local: false,
            reason: format!("complex: {word_count} words across {message_count} messages"),
        }
    }
}

/// Count how many chat messages the request carries (proxy for conversation depth).
fn count_messages(payload: &Value) -> usize {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.len())
        .unwrap_or(0)
}

fn extract_prompt_text(payload: &Value) -> String {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|msg| {
                    msg.get("role")
                        .and_then(Value::as_str)
                        .map(|role| role.eq_ignore_ascii_case("user"))
                        .unwrap_or(false)
                })
                .filter_map(|msg| flatten_content(msg.get("content")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn flatten_content(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            // OpenAI compatibility: handle multimodal content blocks where text appears in typed fragments.
            let text = parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        part.get("text").and_then(Value::as_str).map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");

            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

#[cfg(test)] mod tests {
    use super::*; // brings triage(), extract_prompt_text(), count_messages() into scope

    #[test]
    fn triage_accuracy_on_test_set() {
        let data = std::fs::read_to_string("testdata/triage_prompts.jsonl").unwrap();
        let mut correct = 0;
        let mut total = 0;

        for line in data.lines() {
            let case: serde_json::Value = serde_json::from_str(line).unwrap();

            // TODO: pull `messages` back out of `case` and build a payload
            // shaped like { "messages": case["messages"] }, since that's
            // what extract_prompt_text/count_messages expect.
            
            // TODO: call extract_prompt_text(&payload), count_messages(&payload),
            // then triage(&prompt, message_count)
            let prompt = extract_prompt_text(&case);
            let message_count = count_messages(&case);
            let decision = triage(&prompt, message_count);

            // TODO: compare decision.prefer_local to case["expected_local"].as_bool().unwrap()
            // increment `correct` if they match, print a mismatch line if not
            let expected = case["expected_local"].as_bool().unwrap();
            if decision.prefer_local == expected {
                correct += 1;
            } else {
                println!(
                    "MISMATCH id={} note={:?} expected={} got={} reason={:?}",
                    case["id"], case["note"], expected, decision.prefer_local, decision.reason
                );
            }
            total += 1;
        }

        println!("{correct}/{total} correct ({:.1}%)", 100.0 * correct as f64 / total as f64);
    }

    #[test]
    fn triage_matches_human_expectation() {
        let data = std::fs::read_to_string("testdata/triage_prompts.jsonl").unwrap();
        let mut correct = 0;
        let mut total = 0;
        let mut divergences = 0;

        for line in data.lines() {
            let case: serde_json::Value = serde_json::from_str(line).unwrap();

            let prompt = extract_prompt_text(&case);
            let message_count = count_messages(&case);
            let decision = triage(&prompt, message_count);

            let expected_local = case["expected_local"].as_bool().unwrap();
            let human_local = case["human_local"].as_bool().unwrap();
            if expected_local != human_local {
                divergences += 1;
            }

            if decision.prefer_local == human_local {
                correct += 1;
            } else {
                println!(
                    "MISMATCH id={} note={:?} human_local={} got={} reason={:?}",
                    case["id"], case["note"], human_local, decision.prefer_local, decision.reason
                );
            }
            total += 1;
        }
        println!(
            "{correct}/{total} agree with human judgment ({:.1}%) — {divergences} designed divergences",
            100.0 * correct as f64 / total as f64
        );
    }

    #[test]
    fn triage_cost_savings() {
        let data = std::fs::read_to_string("testdata/triage_prompts.jsonl").unwrap(); 
        const INPUT_PRICE_PER_TOKEN: f64 = 0.00000015;
        const OUTPUT_PRICE_PER_TOKEN: f64 = 0.0000006;
        let mut total_baseline_cost = 0.0;
        let mut total_actual_cost = 0.0;
        
        for line in data.lines() {
            let case: serde_json::Value = serde_json::from_str(line).unwrap();

            let prompt = extract_prompt_text(&case);
            let message_count = count_messages(&case);
            let decision = triage(&prompt, message_count);

            let word_count = prompt.split_whitespace().count();
            let tokens = word_count as f64 * 1.3;
            total_baseline_cost += tokens * INPUT_PRICE_PER_TOKEN + tokens * OUTPUT_PRICE_PER_TOKEN;
           
            if decision.prefer_local {
                total_actual_cost += 0.0;
            } else {
                total_actual_cost += tokens * INPUT_PRICE_PER_TOKEN + tokens * OUTPUT_PRICE_PER_TOKEN;
            }
        }
        let savings = (total_baseline_cost - total_actual_cost) / total_baseline_cost * 100.0;
        println!(
            "Baseline cost: {:.6}\nActual cost: {:.6}\nSavings: {:.1}%",
            total_baseline_cost, total_actual_cost, savings
        );
    }
}
