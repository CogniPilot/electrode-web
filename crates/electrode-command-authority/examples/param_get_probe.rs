use std::time::Duration;

use anyhow::{Context, Result};
use flatbuffers::FlatBufferBuilder;
use synapse_fbs::cmd::{
    ParamGetReply, ParamGetRequest, ParamGetRequestArgs, ParamKind, ParamSetReply, ParamSetRequest,
    ParamSetRequestArgs, ParamValue, ParamValueArgs,
};
use zenoh::{config::Config, Wait};

fn main() -> Result<()> {
    let browser = std::env::args().any(|argument| argument == "browser");
    let mut config = Config::default();
    config
        .insert_json5("mode", "\"client\"")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    config
        .insert_json5(
            "connect/endpoints",
            if browser {
                "[\"ws/127.0.0.1:7447\"]"
            } else {
                "[\"udp/127.0.0.1:7447\"]"
            },
        )
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let session = zenoh::open(config)
        .wait()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if browser {
        std::thread::sleep(Duration::from_secs(1));
    }

    let set = std::env::args().any(|argument| argument == "set");
    let mut builder = FlatBufferBuilder::new();
    let name = builder.create_string("velocity.setpoint");
    if set {
        let value = ParamValue::create(
            &mut builder,
            &ParamValueArgs {
                name: Some(name),
                kind: ParamKind::Float,
                float_value: 4.5,
                ..Default::default()
            },
        );
        let request =
            ParamSetRequest::create(&mut builder, &ParamSetRequestArgs { value: Some(value) });
        builder.finish(request, None);
    } else {
        let request = ParamGetRequest::create(
            &mut builder,
            &ParamGetRequestArgs {
                name: Some(name),
                offset: 0,
                limit: 1,
            },
        );
        builder.finish(request, None);
    }

    if browser {
        let subscriber = session
            .declare_subscriber("gcs/v1/status/reply/parameters")
            .wait()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        session
            .put("gcs/v1/cmd/parameters", builder.finished_data().to_vec())
            .wait()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let sample = subscriber
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .context("browser path returned no parameter reply")?;
        print_get_reply(&sample.payload().to_bytes())?;
        return Ok(());
    }

    let replies = session
        .get(if set {
            "cmd/param_set"
        } else {
            "cmd/param_get"
        })
        .payload(builder.finished_data().to_vec())
        .timeout(Duration::from_secs(2))
        .wait()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let reply = replies
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .context("param_get returned no reply")?
        .into_result()
        .map_err(|error| {
            anyhow::anyhow!(
                "{}: {:?}",
                error,
                String::from_utf8_lossy(&error.payload().to_bytes())
            )
        })?;
    let payload = reply.payload().to_bytes();
    if set {
        let decoded = flatbuffers::root::<ParamSetReply<'_>>(&payload)?;
        println!("result={:?}", decoded.result());
        return Ok(());
    }
    print_get_reply(&payload)
}

fn print_get_reply(payload: &[u8]) -> Result<()> {
    let decoded = flatbuffers::root::<ParamGetReply<'_>>(payload)?;
    let value = decoded.values().and_then(|values| {
        if values.is_empty() {
            None
        } else {
            Some(values.get(0))
        }
    });
    println!(
        "result={:?} name={} value={}",
        decoded.result(),
        value.and_then(|item| item.name()).unwrap_or("<none>"),
        value.map(|item| item.float_value()).unwrap_or(f64::NAN)
    );
    Ok(())
}
