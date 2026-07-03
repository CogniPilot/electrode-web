//! Minimal native Zenoh client that connects to an endpoint (default a WebSocket
//! locator) and prints a few `synapse/**` samples. Used to isolate whether a
//! Zenoh WS listener accepts client sessions independent of the browser wasm.
//!
//!   cargo run -p electrode-fake-sim --bin wsprobe -- ws/127.0.0.1:7447 synapse/** 5
//!   cargo run -p electrode-fake-sim --bin wsprobe -- peer synapse/** 5
use zenoh::Wait;

fn main() -> anyhow::Result<()> {
    zenoh::init_log_from_env_or("info");
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws/127.0.0.1:7447".to_string());
    let key_expr = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "synapse/**".to_string());
    let sample_count = std::env::args()
        .nth(3)
        .as_deref()
        .unwrap_or("5")
        .parse::<usize>()
        .unwrap_or(5);

    let mut config = zenoh::Config::default();
    let set = |config: &mut zenoh::Config, key: &str, value: &str| {
        config
            .insert_json5(key, value)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    };
    if endpoint == "peer" {
        set(&mut config, "mode", "\"peer\"")?;
    } else {
        set(&mut config, "mode", "\"client\"")?;
        set(
            &mut config,
            "connect/endpoints",
            &format!("[\"{endpoint}\"]"),
        )?;
        set(&mut config, "scouting/multicast/enabled", "false")?;
    }

    println!("wsprobe: opening {endpoint} session ...");
    let session = zenoh::open(config)
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("wsprobe: CONNECTED, zid={}", session.zid());

    let subscriber = session
        .declare_subscriber(key_expr.clone())
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("wsprobe: subscribed {key_expr}, waiting for {sample_count} samples ...");

    for _ in 0..sample_count {
        match subscriber.recv() {
            Ok(sample) => println!(
                "wsprobe: SAMPLE key={} bytes={}",
                sample.key_expr(),
                sample.payload().to_bytes().len()
            ),
            Err(err) => {
                println!("wsprobe: recv error: {err}");
                break;
            }
        }
    }
    println!("wsprobe: done");
    Ok(())
}
