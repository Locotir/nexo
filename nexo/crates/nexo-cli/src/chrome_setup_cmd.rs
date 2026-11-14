//! `nexo chrome-setup`: connects nexo to the Chrome you already use and are logged in to. Chrome 136 and
//! later refuse outside debugging on the default profile, so nexo goes in through an extension instead:
//! this writes the native-messaging manifest that lets the extension start nexo itself as the host
//! (`nexo chrome-extension://<id>/`), puts the extension's files where the browser can load them, and opens
//! the browser on the page where that takes one click. One command does the lot, for Chrome, Chromium, Edge
//! and Brave, on Linux, macOS and Windows.

use anyhow::{anyhow, Result};
use clap::Args;
use nexo_tools::browser::chrome_setup::{open_extensions, setup, Places, HOST_NAME};

#[derive(Args, Debug)]
pub struct ChromeSetupArgs {
    /// Write the files and print the steps, but do not open the browser.
    #[arg(long)]
    pub no_open: bool,
}

pub fn run(a: &ChromeSetupArgs) -> i32 {
    match execute(a) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nexo chrome-setup: {e:#}");
            1
        }
    }
}

fn execute(a: &ChromeSetupArgs) -> Result<()> {
    // the same function `/chrome` calls inside nexo
    let done = setup(&Places::user().map_err(|e| anyhow!(e))?).map_err(|e| anyhow!(e))?;
    println!("Set up {} for {HOST_NAME}, host {}", done.browsers.join(", "), done.host.display());
    println!("In the browser, on chrome://extensions: 1) turn on Developer mode (top right)");
    println!("2) click Load unpacked and pick {}", done.extension_dir.display());
    println!(
        "3) click the nexo button in the toolbar. Nothing is controlled until a nexo session asks and you click Allow"
    );
    println!("Run `nexo chrome-setup` again after moving or updating nexo, and to refresh the extension's files.");
    if a.no_open {
        return Ok(());
    }
    if !open_extensions(&done).map_err(|e| anyhow!(e))? {
        println!("No browser program found on PATH: open chrome://extensions yourself.");
    }
    Ok(())
}
