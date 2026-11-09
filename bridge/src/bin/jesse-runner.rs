//! `jesse-runner <run dir> [--no-bootout]` — one round of a Studio coding agent run, started by
//! launchd as the run's own job (`<ns>.jesse-run.<run_id>`). See
//! `jesse_bridge::hostrun::runner`.

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next() else {
        eprintln!("usage: jesse-runner <run dir> [--no-bootout]");
        std::process::exit(2);
    };
    let bootout = !args.any(|a| a == "--no-bootout");
    let code = jesse_bridge::hostrun::runner::run(std::path::Path::new(&dir), bootout);
    std::process::exit(code);
}
