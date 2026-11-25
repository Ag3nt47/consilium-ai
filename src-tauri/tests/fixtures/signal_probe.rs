//! Native, offline subprocess for registration/output and owned-tree tests.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("reply") => {
            let output = args.next().unwrap();
            let exit_code = args.next().unwrap().parse().unwrap();
            eprintln!("WARN account +15551234567 is not registered: NotRegisteredException");
            print!("{output}");
            std::io::stdout().flush().unwrap();
            std::process::exit(exit_code);
        }
        Some("descendant") => {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("hang")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let child = command.spawn().unwrap();
            // Readiness is observed before the test starts its short deadline.
            println!("{}", child.id());
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
        Some("hang") => std::thread::sleep(Duration::from_secs(60)),
        _ => std::process::exit(2),
    }
}
