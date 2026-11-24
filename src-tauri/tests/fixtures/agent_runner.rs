//! Native, offline provider stream for runner deadline tests; never calls a model.
use std::io::{Read, Write};
use std::time::Duration;

fn progress() {
    println!(
        r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"done"}}]}}}}"#
    );
    std::io::stdout().flush().unwrap();
}

fn framed_progress(large: bool, stderr_between_chunks: bool) {
    print!(r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":""#);
    print!("{}", "x".repeat(if large { 65_536 } else { 1_024 }));
    std::io::stdout().flush().unwrap();
    if stderr_between_chunks {
        eprintln!("warning from stderr between stdout chunks");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(r#""}}]}}}}"#);
    std::io::stdout().flush().unwrap();
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    match std::env::args().nth(1).as_deref() {
        Some("progress") => {
            for _ in 0..6 {
                progress();
                std::thread::sleep(Duration::from_millis(500));
            }
            println!(r#"{{"type":"result","is_error":false,"result":"finished"}}"#);
        }
        Some(mode @ ("large-progress" | "split-progress")) => {
            for _ in 0..6 {
                framed_progress(mode == "large-progress", mode == "split-progress");
                std::thread::sleep(Duration::from_millis(500));
            }
            println!(r#"{{"type":"result","is_error":false,"result":"finished"}}"#);
        }
        Some("continuous") => loop {
            progress();
            std::thread::sleep(Duration::from_millis(200));
        },
        Some("noise") => loop {
            eprintln!("still waiting");
            std::thread::sleep(Duration::from_millis(100));
        },
        _ => std::process::exit(2),
    }
}
