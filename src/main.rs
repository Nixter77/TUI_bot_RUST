fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = tui_bot::app::main_with_args(&argv);
    std::process::exit(code);
}
