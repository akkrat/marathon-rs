//! Minimal WP 0.4b extension seam, with no Apple command implementation.
use anyhow::{Result, ensure};
use fake_worker::{
    fs::VirtualFs,
    router::{Handler, HandlerFuture, Request},
    script::Action,
};
use std::sync::Arc;
struct FixtureWorld;
impl Handler for FixtureWorld {
    fn name(&self) -> &str {
        "fixture-world"
    }
    fn route<'a>(&'a self, request: &'a Request, fs: &'a VirtualFs) -> HandlerFuture<'a> {
        Box::pin(async move {
            if request.argv == ["/usr/bin/fixture-world", "create"] {
                fs.mkdir("/tmp/marathon/shared").await?;
                fs.write("/tmp/marathon/shared/example", b"fixture\n".to_vec(), 0o600)
                    .await?;
                return Ok(Some(Action::output("/tmp/marathon/shared/example\n", 0)));
            }
            Ok(None)
        })
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    ensure!(args.len() == 2, "usage: extension CONFIG.json");
    let config = serde_json::from_slice(&tokio::fs::read(&args[1]).await?)?;
    let worker = fake_worker::start(config, vec![Arc::new(FixtureWorld)]).await?;
    println!("{}", serde_json::to_string(&worker.ready)?);
    tokio::signal::ctrl_c().await?;
    worker.shutdown().await
}
