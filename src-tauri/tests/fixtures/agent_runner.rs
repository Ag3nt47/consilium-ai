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
        std::thread::sleep(Duration::from_millis(50));
        eprintln!("warning from stderr between stdout chunks");
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(r#""}}]}}}}"#);
    std::io::stdout().flush().unwrap();
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    if let Some(pid_file) = std::env::args().nth(2) {
        std::fs::write(pid_file, std::process::id().to_string()).unwrap();
    }
    match std::env::args().nth(1).as_deref() {
        Some(
            mode @ ("final-stall" | "final-split-stall" | "final-legacy-stall"
            | "final-exit-failure"),
        ) => {
            let result = r#"{"type":"result","is_error":false,"result":"finished","usage":{"input_tokens":5,"output_tokens":7},"modelUsage":{"fixture-model":{"outputTokens":7}}}"#;
            if mode == "final-split-stall" {
                print!("{}", &result[..17]);
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_millis(50));
                // No newline: the runner must recognize the complete, flushed JSON record.
                print!("{}", &result[17..]);
            } else if mode == "final-legacy-stall" {
                print!("{}", result.replacen(r#""type":"result","#, "", 1));
            } else {
                println!("{result}");
            }
            std::io::stdout().flush().unwrap();
            if mode == "final-exit-failure" {
                std::process::exit(7);
            }
            std::thread::sleep(Duration::from_secs(30));
        }
        Some("final-error-stall") => {
            println!(r#"{{"type":"result","is_error":true,"result":"Not logged in"}}"#);
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        Some("fake-final-stall") => {
            println!(
                r#"{{"type":"assistant","result":"not a terminal reply","message":{{"content":[{{"type":"text","text":"a tool said type:result"}}]}}}}"#
            );
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        Some("stderr-final-stall") => {
            eprintln!(r#"{{"type":"result","is_error":false,"result":"stderr is not a reply"}}"#);
            std::thread::sleep(Duration::from_secs(30));
        }
        Some("partial-final-stall") => {
            print!(r#"{{"type":"result","is_error":false,"result":"not complete"#);
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        }
        Some("progress") => {
            for _ in 0..12 {
                progress();
                std::thread::sleep(Duration::from_millis(500));
            }
            println!(r#"{{"type":"result","is_error":false,"result":"finished"}}"#);
        }
        Some(mode @ ("large-progress" | "split-progress")) => {
            for _ in 0..12 {
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
