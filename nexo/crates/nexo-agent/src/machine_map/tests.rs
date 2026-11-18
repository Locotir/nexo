use super::*;

/// A home, a `PATH` and a package list in a temp folder: the tests never read the real ones.
struct Fixture {
    dir: tempfile::TempDir,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A program in `dir`: executable on Unix, `.exe` on Windows.
fn program(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() });
    std::fs::write(&file, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        // quickshell: one real config, four backups of it and a backup folder
        let qs = home.join(".config/quickshell");
        write(&qs.join("shell.qml"), "import Quickshell\nShellRoot {}\n");
        for n in 1..=4 {
            write(&qs.join(format!("shell.qml.bak-{n}")), "old\n");
        }
        write(&qs.join(".bak/shell.qml"), "older\n");
        // dotfiles
        write(&home.join(".zshrc"), "alias ll='ls -l'\n");
        write(&home.join(".gitconfig"), "[user]\n\tname = a\n");
        write(&home.join(".vimrc"), "set number\n");
        write(&home.join(".bash_history"), "ls\n");
        // folders of other programs
        write(&home.join(".config/kitty/kitty.conf"), "font_size 12\n");
        write(&home.join(".config/nvim/init.lua"), "vim.opt.number = true\n");
        write(&home.join(".config/starship.toml"), "add_newline = false\n");
        write(&home.join(".config/mpv/mpv.conf"), "loop-playlist=inf\n");
        write(&home.join(".config/mpv/big.log"), &"line of a log\n".repeat(150_000));
        std::fs::write(home.join(".config/mpv/shot.dat"), [0u8, 1, 2, 3, 0, 255]).unwrap();
        write(&home.join(".config/fonts/a.conf"), "x\n");
        // what is never listed
        write(&home.join(".ssh/config"), "Host *\n");
        write(&home.join(".ssh/id_ed25519"), "key\n");
        write(&home.join(".config/gh/hosts.yml"), "github.com:\n");
        write(&home.join(".config/google-chrome/Default/Cookies"), "x\n");
        write(&home.join(".claude/CLAUDE.md"), "x\n");
        write(&home.join(".config/mpv/credentials.json"), "{\"a\":1}\n");
        // programs
        for name in [
            "quickshell",
            "zsh",
            "git",
            "vim",
            "kitty",
            "nvim",
            "mpv",
            "htop",
            "starship",
            "sort",
            "find",
            "file",
            "time",
            "make",
            "test",
            "ssh",
            "ffmpeg",
        ] {
            program(&bin, name);
        }
        Fixture { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn roots(&self) -> Roots {
        let home = self.home();
        Roots {
            config_dirs: vec![home.join(".config"), home.join(".local/share")],
            path_dirs: vec![self.dir.path().join("bin")],
            packages: Some(
                ["quickshell", "zsh", "git", "vim", "kitty", "neovim", "mpv", "htop", "starship"]
                    .map(String::from)
                    .to_vec(),
            ),
            owners: false,
            home,
        }
    }

    fn map(&self) -> MachineMap {
        MachineMap::with(Index::build(&self.roots()))
    }
}

fn always(_: &Path, _: &Path) -> bool {
    true
}

fn labels(map: &MachineMap, text: &str) -> Vec<String> {
    map.found(text).into_iter().map(|f| f.key).collect()
}

#[test]
fn a_program_and_its_config_folder_are_found_by_the_words_of_the_message() {
    let fx = Fixture::new();
    let mut map = fx.map();
    let found = map.found("add a CPU temperature module to my quickshell");
    assert_eq!(found.len(), 1, "{found:?}");
    let got = map.attach(found, 5_000, &always);
    // the config the message names comes whole, and nothing else of the folder
    assert_eq!(got.files, [fx.home().join(".config/quickshell/shell.qml")]);
    let text = got.lines.join("\n");
    assert!(text.starts_with("quickshell: "), "{text}");
    assert!(text.contains("quickshell/"), "{text}");
    assert!(text.contains("  shell.qml  "), "{text}");
    // four copies and a folder of copies are one count
    assert!(text.contains("  5 backups"), "{text}");
    assert!(!text.contains("bak-"), "{text}");
}

#[test]
fn a_program_alone_is_no_hit_unless_the_person_installed_its_package() {
    let fx = Fixture::new();
    let map = fx.map();
    assert!(map.found("sort these lines by length").is_empty());
    for text in ["find the file", "what time is it", "run make test", "convert it with ffmpeg"] {
        assert!(map.found(text).is_empty(), "{text}");
    }
    // htop has no config here, but its package was installed by hand
    assert_eq!(labels(&map, "htop shows too many columns"), ["htop"]);
    // without a package list no program alone is a hit
    let mut roots = fx.roots();
    roots.packages = None;
    let map = MachineMap::with(Index::build(&roots));
    assert!(map.found("htop shows too many columns").is_empty());
    assert_eq!(labels(&map, "fix my zsh alias"), ["zsh"], "a dotfile needs no package");
}

#[test]
fn a_dotfile_is_found_without_its_dot_and_without_rc() {
    let fx = Fixture::new();
    let mut map = fx.map();
    let found = map.found("fix my zsh alias");
    assert_eq!(found.len(), 1);
    let got = map.attach(found, 5_000, &always);
    assert_eq!(got.files, [fx.home().join(".zshrc")]);
    assert!(got.lines[0].contains(".zshrc"), "{:?}", got.lines);
    assert_eq!(labels(&map, "explain my .zshrc"), ["zshrc"]);
    assert_eq!(labels(&map, "set up git aliases"), ["git"], "`.gitconfig` without `config`");
    assert_eq!(labels(&map, "arregla mi vim"), ["vim"]);
    assert_eq!(labels(&map, "the starship prompt is slow"), ["starship"], "a file without its extension");
}

#[test]
fn nothing_of_the_ssh_folder_or_other_private_places_is_ever_listed() {
    let fx = Fixture::new();
    let mut map = fx.map();
    assert!(map.found("change my ssh config").is_empty());
    assert!(map.found("show me my .ssh/config and id_ed25519").is_empty());
    for text in ["my chrome profile", "the gh hosts", "my claude settings", "google-chrome cookies", "mpv config"] {
        for f in map.found(text) {
            let all = f.lines.join("\n");
            for private in [".ssh", "id_ed25519", "hosts.yml", "Cookies", "CLAUDE.md", "gh/"] {
                assert!(!all.contains(private), "{text}: {all}");
            }
        }
    }
    // a name that looks secret in a folder that is allowed is listed by name only, and never attached
    let found = map.found("mpv");
    let got = map.attach(found, 20_000, &always);
    let text = got.lines.join("\n");
    assert!(text.contains("credentials.json\n"), "{text}");
    assert!(!text.contains("credentials.json  "), "{text}");
    assert!(!got.files.iter().any(|p| p.ends_with("credentials.json")), "{:?}", got.files);
}

#[cfg(unix)]
#[test]
fn a_link_that_leaves_the_matched_folder_is_not_listed() {
    let fx = Fixture::new();
    let mpv = fx.home().join(".config/mpv");
    std::os::unix::fs::symlink(fx.home().join(".ssh/config"), mpv.join("hosts.conf")).unwrap();
    std::os::unix::fs::symlink(fx.home().join(".ssh"), mpv.join("keys")).unwrap();
    // a link inside the folder to a file in it is the person's own, and is listed
    std::os::unix::fs::symlink(mpv.join("mpv.conf"), mpv.join("same.conf")).unwrap();
    let mut map = fx.map();
    let found = map.found("tune mpv");
    let got = map.attach(found, 20_000, &always);
    let text = got.lines.join("\n");
    assert!(!text.contains("hosts.conf") && !text.contains("keys"), "{text}");
    assert!(text.contains("same.conf"), "{text}");
    assert!(got.files.iter().all(|p| !p.ends_with("hosts.conf")), "{:?}", got.files);
}

#[test]
fn a_big_file_and_a_binary_are_listed_and_not_attached() {
    let fx = Fixture::new();
    let mut map = fx.map();
    let found = map.found("tune mpv");
    let got = map.attach(found, 20_000, &always);
    let text = got.lines.join("\n");
    assert!(text.contains("  big.log  "), "{text}");
    assert!(text.contains("  shot.dat  "), "{text}");
    assert!(text.contains("  mpv.conf  "), "{text}");
    assert_eq!(got.files, [fx.home().join(".config/mpv/mpv.conf")]);
}

#[test]
fn a_match_shown_and_unchanged_is_not_shown_again_but_a_change_shows_it_again() {
    let fx = Fixture::new();
    let mut map = fx.map();
    let first = map.attach(map.found("my quickshell"), 5_000, &always);
    assert_eq!(first.lines.len(), 1);
    assert_eq!(first.files.len(), 1);
    // the same match on the next turn, files unchanged
    let again = map.attach(map.found("quickshell again"), 5_000, &always);
    assert!(again.lines.is_empty() && again.files.is_empty(), "{again:?}");
    // another match is new
    assert_eq!(map.attach(map.found("and my zsh"), 5_000, &always).lines.len(), 1);
    // a change of the files (their size) shows it again
    write(&fx.home().join(".config/quickshell/shell.qml"), "import Quickshell\nShellRoot { a: 1 }\n");
    let changed = map.attach(map.found("quickshell"), 5_000, &always);
    assert_eq!(changed.lines.len(), 1);
    assert_eq!(changed.files.len(), 1);
    // a conversation that was cleared or compacted shows it again too
    map.forget();
    assert_eq!(map.attach(map.found("quickshell"), 5_000, &always).lines.len(), 1);
}

#[test]
fn an_index_that_is_not_built_carries_nothing_and_nothing_waits_for_it() {
    let fx = Fixture::new();
    let mut map = MachineMap::pending();
    assert!(map.found("add a CPU temperature module to my quickshell").is_empty());
    assert!(map.cell().get().is_none());
    assert!(map.attach(Vec::new(), 5_000, &always).lines.is_empty());
    // the same map finds it as soon as the index is there
    let _ = map.cell().set(Index::build(&fx.roots()));
    assert_eq!(map.found("my quickshell").len(), 1);
}

#[test]
fn thirty_requests_find_what_they_name_and_nothing_else() {
    let fx = Fixture::new();
    let map = fx.map();
    let table: &[(&str, &[&str])] = &[
        ("add a CPU temperature module to my quickshell", &["quickshell"]),
        ("sort these lines by length", &[]),
        ("fix my zsh alias", &["zsh"]),
        ("change my ssh config", &[]),
        ("make the bar in quick shell taller", &["quickshell"]),
        ("cambia el tema de mi kitty", &["kitty"]),
        ("set tabstop in my nvim config", &["nvim"]),
        ("enable git aliases", &["git"]),
        ("ponme el prompt de starship más corto", &["starship"]),
        ("find the file named foo", &[]),
        ("what time is it", &[]),
        ("run make test", &[]),
        ("mpv should loop playlists", &["mpv"]),
        ("arregla mi vimrc", &["vimrc"]),
        ("htop shows too many columns", &["htop"]),
        ("convert this video with ffmpeg", &[]),
        ("dime qué hora es", &[]),
        ("edit ~/.config/quickshell/shell.qml", &["quickshell"]),
        ("tweak kitty fonts", &["kitty"]),
        ("fix my .ssh/config", &[]),
        ("explain how a zshrc works", &["zshrc"]),
        ("añade un módulo de batería a mi quickshell", &["quickshell"]),
        ("revisa la configuración de git, zsh, kitty y mpv", &["kitty", "git", "mpv"]),
        ("hello there", &[]),
        ("list the files in this folder", &[]),
        ("quiero ver la hora", &[]),
        ("update the nvim plugin list", &["nvim"]),
        ("do a test of the build", &[]),
        ("restore it from the backup", &[]),
        ("ok", &[]),
    ];
    assert_eq!(table.len(), 30);
    for (text, want) in table {
        assert_eq!(labels(&map, text), *want, "{text}");
    }
}

#[test]
fn the_listing_stays_in_its_cap_and_says_what_it_left_out() {
    let fx = Fixture::new();
    for i in 0..40 {
        write(&fx.home().join(format!(".config/kitty/themes/t{i:02}.conf")), "x\n");
    }
    let mut map = fx.map();
    let got = map.attach(map.found("kitty and quickshell and mpv"), 20_000, &always);
    let all = got.lines.join("\n");
    assert!(all.len() <= LINE_CHARS, "{} characters", all.len());
    assert!(all.contains("and 2"), "{all}");
    assert!(got.lines.iter().all(|b| b.lines().count() <= MAX_ROWS + 6), "{all}");
    // the tokens bound what comes: with room for the lines alone no file is attached
    let mut map = fx.map();
    let tight = map.attach(map.found("my quickshell"), 40, &always);
    assert!(tight.files.is_empty());
    // a file the model has already is listed and not attached
    let mut map = fx.map();
    let known = map.attach(map.found("my quickshell"), 5_000, &|p: &Path, _: &Path| !p.ends_with("shell.qml"));
    assert!(known.files.is_empty());
    assert!(known.lines[0].contains("shell.qml"));
}

#[test]
fn dotfiles_of_the_session_and_its_history_are_no_programs() {
    let fx = Fixture::new();
    let map = fx.map();
    assert!(map.found("clean my bash_history and .bash_history").is_empty());
    assert!(map.found("open the config folder").is_empty());
}
