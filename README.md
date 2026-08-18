# Synapse-Edge: Cost-Aware LLM Prompt Router

A Rust proxy that routes chat requests between a local LLM (Ollama) and a cloud API (Groq/OpenAI-compatible) based on prompt complexity, reducing inference costs while maintaining response quality. Routes simple prompts locally for near-instant inference; escalates complex queries to the cloud.

## How It Works

The proxy runs as a server accepting OpenAI-compatible chat completion requests on `BIND_ADDR` (default `0.0.0.0:3000`). For each incoming request, it:

1. **Triages the prompt** using a lightweight heuristic:
   - Routes to local Ollama if: `word_count ≤ 30` AND `message_count ≤ 2` AND no hard-signal keywords
   - Hard-signal keywords: "explain", "write code", "debug", "step by step", "analyze", "prove", "refactor", "in detail"
   - Otherwise routes to cloud

2. **Executes the request** against the chosen endpoint and returns the response

3. **Fails over autonomously**: If the local node is unreachable (TCP connection error), automatically retries against the cloud API

## Setup

**Requirements:**
- Rust 1.70+ (via `rustup`)
- Ollama installed and running locally (default: `http://127.0.0.1:11434`)
- Cloud API key for Groq or OpenAI (optional, needed for cloud requests)

**Configuration:**

Copy `.env.example` to `.env` and fill in your values:
```bash
cp .env.example .env
```

Then edit `.env` with:
- `LOCAL_URL`: Your local Ollama endpoint
- `CLOUD_URL`: Your cloud API endpoint (Groq, OpenAI, etc. — must be OpenAI-compatible)
- `CLOUD_API_KEY`: Your cloud API key
- `BIND_ADDR`: What address/port the proxy listens on

**Build & run:**
```bash
cargo run --bin synapse-edge
```

The proxy will bind to `BIND_ADDR` and log all decisions and latencies to both stdout and `logs/synapse-edge.log` (daily rotation).

## Evaluation Results

A 60-prompt test set (`testdata/triage_prompts.jsonl`) validates the routing logic:

- **Heuristic conformance**: 60/60 correct (100%) — the code matches its own rule
- **Human-judgment agreement**: 50/60 (83.3%) — actual prompt complexity evaluation
  - 10 designed divergences reveal failure modes:
    - **Keyword substring matching**: "I appreciate you explaining that" flagged as complex (contains "explain" substring, but is a thank-you, not a request)
    - **Message-count over-triggering**: Trivial back-and-forth conversations (3+ turns) routed to cloud even if all messages are simple
    - **Missed requests**: "Give me a Python one-liner..." routes locally despite being code generation (doesn't contain "write code")
- **Estimated cost savings**: 37.8% reduction in API costs across the test set (based on word-count token estimation vs. published OpenAI pricing)

## Known Limitations

1. **Keyword matching is brittle**: Substring matching catches false positives ("explaining" → "explain") and false negatives (code requests without the phrase "write code")
2. **No semantic understanding**: Complexity classification is purely lexical (word/message count + keywords), not semantic
3. **Failover only on connection errors**: Timeouts or model-level errors on Ollama don't trigger fallback to cloud
4. **Hardcoded thresholds**: 30-word limit and 2-message limit are fixed; no dynamic tuning

## Tech Stack

- **Framework**: Axum (async web server)
- **Runtime**: Tokio (async executor)
- **HTTP client**: reqwest
- **Serialization**: serde + serde_json
- **Logging**: tracing + tracing-appender (structured, rotating logs)
- **Environment**: dotenvy
