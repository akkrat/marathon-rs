mod support;
use marathon_apple::cmd::{CommandExecutor, CommandRequest, base_command::CommandTermination};
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn foreground_recorder_finalizes_on_int_d96() {
    watchdog(async {
        let executor = executor();
        let context = context();
        let recorder = [
            "/usr/bin/perl",
            "-e",
            "'$SIG{INT}=sub{print qq(finalized\n);exit 0};$|=1;print qq(ready\n);sleep 60'",
        ];
        let session = executor
            .execute(CommandRequest::new(recorder, duration(60_000)), context.clone())
            .await
            .unwrap();
        assert_eq!(session.base.stdout.receive().await.unwrap().as_deref(), Some("ready"));
        session.interrupt().unwrap();
        let completed = session.await_result(&context).await.unwrap();
        assert_eq!(completed.stdout, ["finalized"]);
        assert_eq!(
            session.base.exit_code.termination(),
            Some(CommandTermination::Status(0))
        );
        assert!(!session.is_alive());
    })
    .await;
}
