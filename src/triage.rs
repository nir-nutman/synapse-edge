use serde_json::Value;

/// Represents a decision the triage proxy makes
/// Contains a reason attribute to explain why it
/// chose the local model or API model
pub struct TriageDecision {
    pub prefer_local: bool,
    pub reason: String,
}

/// Keywords to determine if a prompt is complex
/// and should be routed to the API model
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

/// Takes a prompt (string) and message count (integer) as input
/// Returns a TriageDecision: whether the local model is preferred
/// and the reason for choosing the model it chose (local or API)
/// Chooses local model if word count <= 30 and message count <= 2
/// and if there are no complex keywords (from HARD_SIGNALS array)
pub fn triage(prompt: &str, message_count: usize) -> TriageDecision {
    let word_count = prompt.split_whitespace().count();
    let lowered = prompt.to_lowercase();

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

/// Reads messages from request payload, returns # of messages
/// Triage logic needs this number in it's calculation (<= 2 messages)
pub fn count_messages(payload: &Value) -> usize {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| messages.len())
        .unwrap_or(0)
}

/// Pulls out prompt from the payload
/// We only need the user's messages because
/// that determines whether the local model or API is used
pub fn extract_prompt_text(payload: &Value) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triage_accuracy_on_test_set() {
        let data = std::fs::read_to_string("testdata/triage_prompts.jsonl").unwrap();
        let mut correct = 0;
        let mut total = 0;

        for line in data.lines() {
            let case: serde_json::Value = serde_json::from_str(line).unwrap();

            let prompt = extract_prompt_text(&case);
            let message_count = count_messages(&case);
            let decision = triage(&prompt, message_count);

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

        println!(
            "{correct}/{total} correct ({:.1}%)",
            100.0 * correct as f64 / total as f64
        );
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
                total_actual_cost +=
                    tokens * INPUT_PRICE_PER_TOKEN + tokens * OUTPUT_PRICE_PER_TOKEN;
            }
        }
        let savings = (total_baseline_cost - total_actual_cost) / total_baseline_cost * 100.0;
        println!(
            "Baseline cost: {:.6}\nActual cost: {:.6}\nSavings: {:.1}%",
            total_baseline_cost, total_actual_cost, savings
        );
    }
}
