//! Offline account probes never contact Signal, link accounts, or send messages.
use super::*;
use std::cell::Cell;

const ACCOUNT: &str = "+15551234567";
const ACI: &str = "8f0c6e1a-1111-4222-8333-944455556666";

fn registered_json(number: &str) -> String {
    serde_json::json!([{"number": number, "aci": ACI}]).to_string()
}

#[test]
fn a_registered_account_requires_exact_structured_identity() {
    assert_eq!(parse_registration(registered_json(ACCOUNT).as_bytes(), ACCOUNT), Registration::Registered);
    assert_eq!(parse_registration(registered_json("+15559999999").as_bytes(), ACCOUNT), Registration::LinkRequired);
    assert_eq!(parse_registration(b"[]", ACCOUNT), Registration::LinkRequired);
    let aci_only = serde_json::json!([{"number": null, "aci": ACI}]).to_string();
    assert_eq!(parse_registration(aci_only.as_bytes(), ACCOUNT), Registration::LinkRequired);
}

#[test]
fn warnings_plain_text_and_malformed_json_never_authorize_a_new_link() {
    let warning = format!("WARN account {ACCOUNT} is not registered: NotRegisteredException");
    let invalid = ["".to_string(), warning, format!("Number: {ACCOUNT}"),
        "{}".into(), "[{}]".into(), "[null]".into(),
        format!("[{{\"number\":\"{ACCOUNT}\",\"aci\":\"invalid\"}}]"),
        format!("[{{\"number\":42,\"aci\":\"{ACI}\"}}]"),
        "[] trailing output".into()];
    for output in invalid {
        assert_eq!(parse_registration(output.as_bytes(), ACCOUNT), Registration::Unavailable(INVALID_ACCOUNTS));
    }
    assert_eq!(parse_registration(&vec![b' '; MAX_ACCOUNT_OUTPUT + 1], ACCOUNT), Registration::Unavailable(INVALID_ACCOUNTS));
}

#[test]
fn unregistered_or_unreadable_accounts_never_start_a_daemon_or_silently_relink() {
    for registration in [Registration::LinkRequired, Registration::Unavailable(INVALID_ACCOUNTS),
        Registration::Unavailable(CHECK_TIMED_OUT), Registration::Unavailable(CHECK_UNAVAILABLE)] {
        let starts = Cell::new(0);
        let error = start_if_registered(registration, || { starts.set(starts.get() + 1); Ok(()) }).unwrap_err();
        assert_eq!(starts.get(), 0);
        assert!(!error.contains(ACCOUNT) && !error.contains(ACI));
        let links = Cell::new(0);
        let outcome = link_if_needed(registration, || { links.set(links.get() + 1); Ok(()) });
        if registration == Registration::LinkRequired {
            assert_eq!(outcome, Ok(true));
            assert_eq!(links.get(), 1, "An explicit setup can still obtain a new phone QR link");
        } else {
            assert!(outcome.is_err());
            assert_eq!(links.get(), 0, "Probe uncertainty must not replace existing keys");
        }
    }
    assert_eq!(start_if_registered(Registration::Registered, || Ok(())), Ok(()));
    assert_eq!(link_if_needed(Registration::Registered, || panic!("Existing registration must be preserved")), Ok(false));
}

fn native_fixture() -> &'static std::path::Path {
    static PROGRAM: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    PROGRAM.get_or_init(|| {
        let directory = std::env::temp_dir().join(format!("consilium-registration-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let program = directory.join(if cfg!(windows) { "probe.exe" } else { "probe" });
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/signal_probe.rs");
        let mut compiler = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
        compiler.arg("--edition=2021").arg(&source).arg("-o").arg(&program);
        let output = quiet(&mut compiler).output().expect("The existing Rust toolchain must compile the native fixture");
        assert!(output.status.success(), "Native fixture compilation failed: {}", String::from_utf8_lossy(&output.stderr));
        program
    }).as_path()
}

fn fixture_command(output: &str, exit_code: i32, hang: bool) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(native_fixture());
    if hang { command.arg("hang"); } else { command.args(["reply", output, &exit_code.to_string()]); }
    command
}

#[test]
fn a_successful_empty_cli_list_is_not_overridden_by_account_numbers_in_stderr() {
    assert_eq!(registration_probe(fixture_command("[]", 0, false), ACCOUNT, Duration::from_secs(10)), Registration::LinkRequired);
    assert_eq!(registration_probe(fixture_command(&registered_json(ACCOUNT), 0, false), ACCOUNT, Duration::from_secs(10)), Registration::Registered);
}

#[test]
fn a_failed_or_invalid_cli_probe_cannot_claim_registration_or_request_a_relink() {
    assert_eq!(registration_probe(fixture_command(&registered_json(ACCOUNT), 2, false), ACCOUNT, Duration::from_secs(10)), Registration::Unavailable(CHECK_UNAVAILABLE));
    assert_eq!(registration_probe(fixture_command("", 0, false), ACCOUNT, Duration::from_secs(10)), Registration::Unavailable(INVALID_ACCOUNTS));
}

#[test]
fn a_stuck_probe_has_a_bounded_deadline_and_keeps_phone_data_out_of_errors() {
    let command = fixture_command("", 0, true);
    let started = Instant::now();
    assert_eq!(registration_probe(command, ACCOUNT, Duration::from_millis(50)), Registration::Unavailable(CHECK_TIMED_OUT));
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn a_timed_out_ready_probe_does_not_leave_its_owned_descendant_running() {
    use tokio::io::AsyncBufReadExt;
    let mut command = tokio::process::Command::new(native_fixture());
    command.arg("descendant").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let pid = runtime.block_on(async {
        let mut parent = command.spawn().unwrap();
        let mut reader = tokio::io::BufReader::new(parent.stdout.take().unwrap());
        let mut line = String::new();
        let ready = tokio::time::timeout(Duration::from_secs(30), reader.read_line(&mut line)).await;
        if !matches!(ready, Ok(Ok(count)) if count > 0) {
            stop_registration_probe(&mut parent).await;
            panic!("The owned native fixture did not report descendant readiness");
        }
        let pid: u32 = line.trim().parse().unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(50), parent.wait()).await.is_err());
        stop_registration_probe(&mut parent).await;
        pid
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let system = sysinfo::System::new_all();
        let running = system.process(sysinfo::Pid::from_u32(pid)).is_some_and(|process|
            !matches!(process.status(), sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead));
        if !running { break; }
        assert!(Instant::now() < deadline, "An owned descendant survived the timeout");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_existing_ui_reports_phone_link_requirements_without_redefining_raw_daemon_health() {
    let mut cfg = BridgeConfig { enabled: true, account: ACCOUNT.into(), ..Default::default() };
    let raw = || bridge::ConnectorStatus { id: "signal", name: "Signal", state: "ok", detail: "Fixture daemon answered its raw version probe".into() };
    let idle = SetupState::default();
    let unverified = setup_connector_status(&cfg, raw(), &idle);
    assert_eq!(unverified.state, "down");
    assert!(unverified.detail.contains("phone verification"));
    cfg.allowed_acis.push(ACI.into());
    assert_eq!(setup_connector_status(&cfg, raw(), &idle).state, "ok");
    let missing_link = SetupState { phase: "failed".into(), detail: LINK_REQUIRED.into(), ..Default::default() };
    let missing = setup_connector_status(&cfg, bridge::ConnectorStatus { state: "down", ..raw() }, &missing_link);
    assert_eq!(missing.state, "down");
    assert_eq!(missing.detail, LINK_REQUIRED);
    assert_eq!(setup_connector_status(&cfg, raw(), &missing_link).state, "ok", "Fresh daemon recovery supersedes an older failed check");
    cfg.allowed_acis.clear();
    let recovered_unverified = setup_connector_status(&cfg, raw(), &missing_link);
    assert_eq!(recovered_unverified.state, "down");
    assert!(recovered_unverified.detail.contains("phone verification"));
    cfg.enabled = false;
    let off = bridge::ConnectorStatus { state: "off", ..raw() };
    assert_eq!(setup_connector_status(&cfg, off, &missing_link).state, "off");
}
