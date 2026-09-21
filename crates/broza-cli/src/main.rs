//! `broza` binary entry point. The only place that calls `std::process::exit`.

fn main() {
    let code = broza_cli::run(std::env::args_os());
    std::process::exit(code.code());
}
