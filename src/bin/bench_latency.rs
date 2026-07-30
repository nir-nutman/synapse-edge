use std::time::Instant;
use serde_json::json;

#[tokio::main]
async fn main() {
    let client = reqwest::Client::new();
    let proxy_url = "http://127.0.0.1:3000/v1/chat/completions"; // match your BIND_ADDR

    // A handful of prompts you already know route local vs cloud, from testdata/.
    let local_prompt = "What is 2+2?";
    let cloud_prompt = "Explain how photosynthesis works in detail.";

    let mut local_latencies: Vec<u128> = Vec::new();
    let mut cloud_latencies: Vec<u128> = Vec::new();

    for _ in 0..20 {
        let body = json!({ "messages": 
            [{"role": "user", "content": local_prompt}] 
        });
        let start = Instant::now();
        let _response = client.post(proxy_url).json(&body).send().await.unwrap();
        let elapsed_ms = start.elapsed().as_millis();

        local_latencies.push(elapsed_ms);
    }

    for _ in 0..20 {
        let body = json!({"messages": 
            [{"role": "user", "content": cloud_prompt}]
        });
        let start = Instant::now();
        let _response = client.post(proxy_url).json(&body).send().await.unwrap();
        let elapsed_ms = start.elapsed().as_millis();

        cloud_latencies.push(elapsed_ms);
    }

    local_latencies.sort();
    let p50_index = local_latencies.len() / 2;
    let p95_index = (local_latencies.len() as f64 * 0.95) as usize;
    let p95_index = p95_index.min(local_latencies.len() - 1); // guard against out of bounds

    let local_p50 = local_latencies[p50_index];
    let local_p95 = local_latencies[p95_index];
    println!("Local: p50={}ms p95={}ms", local_p50, local_p95);

    cloud_latencies.sort();
    let p50_index = cloud_latencies.len() / 2;
    let p95_index = (cloud_latencies.len() as f64 * 0.95) as usize; 
    let p95_index = p95_index.min(cloud_latencies.len() - 1);

    let local_p50 = cloud_latencies[p50_index];
    let local_p95 = cloud_latencies[p95_index];
    println!("Cloud: p50={}ms p95={}ms", local_p50, local_p95);
}
