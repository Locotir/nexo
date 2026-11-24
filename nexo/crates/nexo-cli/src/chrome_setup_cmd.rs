//! `nexo chrome-setup`: connects nexo to the Chrome you already use and are logged in to. Chrome 136 and
//! later refuse outside debugging on the default profile, so nexo goes in through an extension instead:
//! this writes the native-messaging manifest that lets the extension start nexo itself as the host
//! (`nexo chrome-extension://<id>/`), puts the extension's files where the browser can load them, and opens
//! the browser on the page where that takes one click. One command does the lot, for Chrome, Chromium, Edge
//! and Brave, on Linux, macOS and Windows.

use anyhow::{anyhow, Result};
use clap::Args;
use nexo_tools::browser::chrome_setup::{
    copy_folder_on_mac, open_extensions, setup, Places, HOST_NAME, MAC_FOLDER_HINT,
};

const CONNECTION_HINT: &str = "3) click the nexo button in the toolbar to connect. The extension automatically grants access when a nexo session requests it; stop that session with Cancel on Chrome's debugging bar.";

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
    if cfg!(target_os = "macos") {
        let copied = copy_folder_on_mac(&done.extension_dir);
        println!("   {MAC_FOLDER_HINT}{}", if copied { " The path is on your clipboard." } else { "" });
    }
    for note in &done.notes {
        println!("Note: {note}");
    }
    println!("{CONNECTION_HINT}");
    println!("Run `nexo chrome-setup` again after moving or updating nexo, and to refresh the extension's files.");
    if a.no_open {
        return Ok(());
    }
    if !open_extensions(&done).map_err(|e| anyhow!(e))? {
        println!("No browser program found on PATH: open chrome://extensions yourself.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CONNECTION_HINT;

    #[test]
    fn connection_hint_explains_automatic_access_and_stop() {
        assert!(CONNECTION_HINT.contains("automatically grants access"));
        assert!(CONNECTION_HINT.contains("Cancel on Chrome's debugging bar"));
        assert!(!CONNECTION_HINT.contains("click Allow"));
    }

    #[test]
    fn browser_help_describes_automatic_access_not_an_allow_dialog() {
        use clap::CommandFactory;

        let mut cli = crate::cli::Args::command();
        let setup = cli.find_subcommand_mut("chrome-setup").unwrap().render_long_help().to_string();
        assert!(setup.contains("grants session access automatically"), "{setup}");
        assert!(!setup.contains("accept once"), "{setup}");
        let api = cli.find_subcommand_mut("api").unwrap();
        for name in ["call", "watch"] {
            let help = api.find_subcommand_mut(name).unwrap().render_long_help().to_string();
            assert!(help.contains("grants session access automatically"), "{help}");
            assert!(help.contains("Cancel on Chrome's debugging bar"), "{help}");
            assert!(!help.contains("click Allow"), "{help}");
        }
    }
}
