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
            // Perl can defer the handler if INT arrives just before a blocking sleep.
            // Block INT until sigsuspend can unblock it and wait atomically.
            r#"'use POSIX qw(SIGINT SIG_BLOCK sigprocmask sigsuspend);
my $interrupt = POSIX::SigSet->new(SIGINT);
my $previous = POSIX::SigSet->new;
sigprocmask(SIG_BLOCK, $interrupt, $previous) or die "block INT: $!";
$previous->delset(SIGINT);
$SIG{INT} = sub { print "finalized\n"; exit 0 };
$| = 1;
print "ready\n";
sigsuspend($previous);
die "wait for INT: $!";'"#,
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
