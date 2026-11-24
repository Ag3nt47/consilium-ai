use super::*;

fn native_fixture() -> &'static Path {
    static PROGRAM: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PROGRAM
        .get_or_init(|| {
            let directory = std::env::temp_dir()
                .join(format!("consilium-runner-fixture-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let program = directory.join(if cfg!(windows) {
                "runner.exe"
            } else {
                "runner"
            });
            let source =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_runner.rs");
            let mut compiler = std::process::Command::new(
                std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
            );
            compiler
                .args(["--edition=2021", "-o"])
                .arg(&program)
                .arg(source);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                compiler.creation_flags(0x0800_0000);
            }
            let output = compiler
                .output()
                .expect("The existing Rust toolchain must compile the native fixture");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            program
        })
        .as_path()
}

async fn fixture(
    mode: &str,
    idle: Duration,
    cancel: &CancellationToken,
    activity: Option<ActivitySink>,
) -> Result<String, String> {
    let program = native_fixture();
    let dir = program.parent().unwrap();
    execute(
        program,
        Kind::ClaudeCode,
        Access::Chat,
        dir,
        &dir.join("unused-reply"),
        Plan {
            args: vec![mode.into()],
            input: "synthetic turn".into(),
        },
        cancel,
        idle,
        activity,
        vec![],
    )
    .await
    .map(|reply| reply.text)
}

#[tokio::test]
async fn tool_progress_renews_the_deadline_without_a_ui_sink() {
    let reply = fixture(
        "progress",
        Duration::from_secs(2),
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply, "finished");
}

#[tokio::test]
async fn large_tool_results_still_renew_the_deadline() {
    let reply = fixture(
        "large-progress",
        Duration::from_secs(2),
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply, "finished");
}

#[tokio::test]
async fn stderr_between_stdout_chunks_does_not_break_progress() {
    let reply = fixture(
        "split-progress",
        Duration::from_secs(2),
        &CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply, "finished");
}

#[tokio::test]
async fn raw_output_does_not_keep_a_stalled_turn_alive() {
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        fixture(
            "noise",
            Duration::from_secs(1),
            &CancellationToken::new(),
            None,
        ),
    )
    .await
    .expect("raw output must not renew the deadline")
    .unwrap_err();
    assert_eq!(
        error,
        "no activity for 1 seconds; no provider activity received"
    );
}

#[tokio::test]
async fn continuous_progress_still_reaches_the_total_runtime_cap() {
    let error = tokio::time::timeout(
        Duration::from_secs(15),
        fixture(
            "continuous",
            Duration::from_secs(2),
            &CancellationToken::new(),
            None,
        ),
    )
    .await
    .expect("recognized progress must not permit an unlimited turn")
    .unwrap_err();
    assert_eq!(
        error,
        "no answer within 8 seconds total runtime; last activity: Checking tool result"
    );
}

#[tokio::test]
async fn stop_still_ends_an_active_turn() {
    let cancel = CancellationToken::new();
    let on_progress = cancel.clone();
    let sink: ActivitySink = Arc::new(move |_| on_progress.cancel());
    let error = fixture("continuous", Duration::from_secs(2), &cancel, Some(sink))
        .await
        .unwrap_err();
    assert_eq!(error, "stopped");
}
