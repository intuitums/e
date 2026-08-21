//! The `e` binary. M0: version only; the frame arrives at M2.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("e {}", e_core::VERSION);
        return;
    }
    eprintln!("e {}: the interactive frame lands at milestone M2.", e_core::VERSION);
    std::process::exit(1);
}
