//! Started by Chrome when the nexo extension connects: speaks native-messaging frames on stdin and stdout.
//! `nexo` does the same through `nexo browser-host`; this binary is for builds without the full CLI.

fn main() {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("nexo-browser-host: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(nexo_browser_host::run_stdio()) {
        eprintln!("nexo-browser-host: {e}");
        std::process::exit(1);
    }
}
