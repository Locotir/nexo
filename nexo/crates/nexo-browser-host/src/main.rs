//! Started by Chrome when the nexo extension connects: speaks native-messaging frames on stdin and stdout.

use std::io::{self, BufReader, BufWriter};

fn main() {
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    if let Err(e) = nexo_browser_host::serve(&mut input, &mut output) {
        eprintln!("nexo-browser-host: {e}");
        std::process::exit(1);
    }
}
