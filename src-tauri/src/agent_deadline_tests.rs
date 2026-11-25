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
    fixture_reply(mode, idle, cancel, activity, None)
        .await
        .map(|reply| reply.text)
}

async fn fixture_reply(
    mode: &str,
    idle: Duration,
    cancel: &CancellationToken,
    activity: Option<ActivitySink>,
    pid_file: Option<&Path>,
) -> Result<AgentReply, String> {
    let program = native_fixture();
    let dir = program.parent().unwrap();
    let mut args = vec![mode.into()];
    args.extend(pid_file.map(|p| p.as_os_str().to_owned()));
    execute(
        program,
        Kind::ClaudeCode,
        Access::Chat,
        dir,
        &dir.join("unused-reply"),
        Plan {
            args,
            input: "synthetic turn".into(),
        },
        cancel,
        idle,
        activity,
        vec![],
    )
    .await
}

#[tokio::test]
async fn final_result_survives_a_cli_that_does_not_exit() {
    // The result arrives immediately, so even a short activity budget must no longer apply.
    // Compile the fixture before starting the bound on the runner itself.
    native_fixture();
    for mode in ["final-stall", "final-split-stall", "final-legacy-stall"] {
        let pid_file =
            std::env::temp_dir().join(format!("final-result-{}.pid", uuid::Uuid::new_v4()));
        let reply = tokio::time::timeout(
            Duration::from_secs(20),
            fixture_reply(
                mode,
                Duration::from_secs(1),
                &CancellationToken::new(),
                None,
                Some(&pid_file),
            ),
        )
        .await
        .expect("a final result must not wait for the CLI's 30-second stall")
        .unwrap();
        assert_eq!(reply.text, "finished");
        let usage = reply.usage.unwrap();
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.total_tokens),
            (5, 7, 12)
        );
        assert_eq!(reply.model.as_deref(), Some("fixture-model"));
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let _ = std::fs::remove_file(pid_file);
        let mut system = sysinfo::System::new();
        let pid = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        assert!(
            system.process(pid).is_none(),
            "the completed CLI must be reaped"
        );
    }
}

#[tokio::test]
async fn final_result_does_not_hide_provider_or_process_errors() {
    for (mode, expected) in [
        ("final-error-stall", "Not logged in"),
        ("final-exit-failure", "exit 7:"),
    ] {
        let error = tokio::time::timeout(
            Duration::from_secs(20),
            fixture(
                mode,
                Duration::from_secs(1),
                &CancellationToken::new(),
                None,
            ),
        )
        .await
        .expect("a final provider error must be delivered promptly")
        .unwrap_err();
        assert!(error.starts_with(expected), "{mode}: {error}");
    }
}

#[tokio::test]
async fn stop_still_wins_after_a_final_result() {
    let cancel = CancellationToken::new();
    let on_result = cancel.clone();
    let sink: ActivitySink = Arc::new(move |signal| {
        if signal.kind == "reporting" {
            on_result.cancel();
        }
    });
    let error = fixture("final-stall", Duration::from_secs(1), &cancel, Some(sink))
        .await
        .unwrap_err();
    assert_eq!(error, "stopped");
}

#[tokio::test]
async fn nonterminal_output_cannot_claim_a_completed_result() {
    for mode in [
        "fake-final-stall",
        "stderr-final-stall",
        "partial-final-stall",
    ] {
        let error = tokio::time::timeout(
            Duration::from_secs(15),
            fixture(
                mode,
                Duration::from_secs(1),
                &CancellationToken::new(),
                None,
            ),
        )
        .await
        .expect("nested, stderr and partial results must still time out")
        .unwrap_err();
        assert!(
            error.starts_with("no activity for 1 seconds"),
            "{mode}: {error}"
        );
    }
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
