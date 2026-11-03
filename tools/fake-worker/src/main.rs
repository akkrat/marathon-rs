//! Standalone loopback worker, optionally with the WP 0.4b Apple world.
use anyhow::{Result, ensure};
use std::sync::Arc;
#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    ensure!(
        (args.len() == 3 || (args.len() == 7 && args[3] == "--apple" && args[5] == "--state")) && args[1] == "--config",
        "usage: fake-worker --config CONFIG.json [--apple SCENARIO.json --state STATE.json]"
    );
    let config = serde_json::from_slice(&tokio::fs::read(&args[2]).await?)?;
    let world = if args.len() == 7 {
        Some(fake_worker::apple::World::new(serde_json::from_slice(
            &tokio::fs::read(&args[4]).await?,
        )?)?)
    } else {
        None
    };
    let extensions = world
        .as_ref()
        .map(|w| vec![Arc::new(w.clone()) as Arc<dyn fake_worker::router::Handler>])
        .unwrap_or_default();
    // Installed before the ready line, so a harness may signal as soon as it has read it.
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let worker = fake_worker::start(config, extensions).await?;
    println!("{}", serde_json::to_string(&worker.ready)?);
    tokio::select! {_=interrupt.recv()=>(),_=terminate.recv()=>()};
    let fs = worker.fs.clone();
    worker.shutdown().await?;
    if let Some(world) = world {
        tokio::fs::write(&args[6], serde_json::to_vec_pretty(&world.final_state(&fs).await?)?).await?;
        world.validate()?;
    }
    Ok(())
}
