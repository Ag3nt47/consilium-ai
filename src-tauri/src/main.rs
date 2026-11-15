// Prevents an extra console window on Windows in release builds. (As the terminal command, the
// program attaches to the terminal it was started from; see `cli::main`.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `consilium chat` and the other commands run in the terminal; no command opens the app.
    if consilium_lib::cli::wants_cli(&args) {
        std::process::exit(consilium_lib::cli::main(args));
    }
    consilium_lib::run()
}
