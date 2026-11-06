//! The exercises of the Polyglot benchmark (github.com/Aider-AI/polyglot-benchmark, Exercism practice
//! exercises in six languages): what is read from each, and the working directory the agent gets. The
//! working directory holds the solution stubs, the instructions' side files a build needs and nothing else:
//! no test, no example solution, no `.meta`, no `.docs`, no other exercise, no git history.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::prompts;

/// The benchmark's six languages. Java builds with gradle 8.7 (the exercises' wrapper version), fetched once
/// and run on a JDK 17 or 21.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lang {
    Python,
    Rust,
    Go,
    Cpp,
    Javascript,
    Java,
}

pub const LANGS: [Lang; 6] = [Lang::Python, Lang::Rust, Lang::Go, Lang::Cpp, Lang::Javascript, Lang::Java];

impl Lang {
    /// The folder of the language in the benchmark, and the name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::Cpp => "cpp",
            Lang::Javascript => "javascript",
            Lang::Java => "java",
        }
    }

    pub fn parse(s: &str) -> Result<Lang, String> {
        let s = s.trim().to_ascii_lowercase();
        LANGS
            .iter()
            .copied()
            .find(|l| l.name() == s)
            .ok_or_else(|| format!("unknown language {s:?} (python, rust, go, cpp, javascript, java)"))
    }

    /// Whether `rel` (a `/`-joined path inside the exercise) is a test file of the language's layout, besides
    /// those the exercise's config lists: Go's `cases_test.go`, C++'s `test/` folder (the test runner and
    /// its header), Rust's `tests/` folder.
    pub fn is_test(self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        match self {
            Lang::Python => file.ends_with("_test.py"),
            Lang::Rust => rel.starts_with("tests/"),
            Lang::Go => file.ends_with("_test.go"),
            Lang::Cpp => rel.starts_with("test/") || file.ends_with("_test.cpp"),
            Lang::Javascript => file.ends_with(".spec.js"),
            Lang::Java => rel.starts_with("src/test/"),
        }
    }
}

/// The part of `.meta/config.json` the bench uses.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Files {
    #[serde(default)]
    pub solution: Vec<String>,
    #[serde(default)]
    pub test: Vec<String>,
    #[serde(default)]
    pub example: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Config {
    #[serde(default)]
    files: Files,
}

/// One exercise of the benchmark.
#[derive(Debug, Clone)]
pub struct Exercise {
    pub lang: Lang,
    pub name: String,
    pub dir: PathBuf,
    pub files: Files,
}

/// Files the benchmark never lets the model edit even when the config lists them as solution
/// (`ignore_files` of `run_test_real`).
const NOT_EDITED: [&str; 2] = ["CMakeLists.txt", "Cargo.toml"];

/// Folders that never reach the agent: the exercise's metadata, docs and alternative approaches.
const HIDDEN_DIRS: [&str; 5] = [".meta", ".docs", ".approaches", ".articles", ".git"];

impl Exercise {
    /// Reads the exercise in `dir`.
    pub fn read(lang: Lang, dir: &Path) -> Result<Exercise, String> {
        let at = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());
        let text = std::fs::read_to_string(dir.join(".meta/config.json")).map_err(|e| at(&e))?;
        let cfg: Config = serde_json::from_str(&text).map_err(|e| at(&e))?;
        if !dir.join(".docs/instructions.md").is_file() {
            return Err(at(&"no .docs/instructions.md"));
        }
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Exercise { lang, name, dir: dir.to_path_buf(), files: cfg.files })
    }

    /// The id of its records: `polyglot/<lang>/<name>`.
    pub fn id(&self) -> String {
        format!("polyglot/{}/{}", self.lang.name(), self.name)
    }

    /// The solution files the agent edits that exist, in the config's order: the listed ones less
    /// `CMakeLists.txt`, `Cargo.toml`, the test and example files and anything under `.meta` or `.docs`.
    pub fn solution(&self) -> Vec<String> {
        self.files
            .solution
            .iter()
            .filter(|f| {
                let hidden = HIDDEN_DIRS.iter().any(|d| f.starts_with(&format!("{d}/")));
                !NOT_EDITED.contains(&f.as_str())
                    && !hidden
                    && !self.files.test.contains(f)
                    && !self.files.example.contains(f)
            })
            .filter(|f| self.dir.join(f).is_file())
            .cloned()
            .collect()
    }

    /// The names of the solution files, space separated: the benchmark's `file_list`.
    pub fn file_list(&self) -> String {
        self.solution().iter().map(|f| f.rsplit('/').next().unwrap_or(f).to_string()).collect::<Vec<_>>().join(" ")
    }

    /// The first message of the exercise.
    pub fn prompt(&self) -> Result<String, String> {
        let read = |rel: &str| std::fs::read_to_string(self.dir.join(rel));
        let instructions = read(".docs/instructions.md").map_err(|e| format!("{}: {e}", self.id()))?;
        Ok(prompts::first_prompt(
            read(".docs/introduction.md").ok().as_deref(),
            &instructions,
            read(".docs/instructions.append.md").ok().as_deref(),
            &self.file_list(),
        ))
    }

    /// Every file of the exercise as a `/`-joined path relative to it, skipping the hidden folders.
    fn walk(&self) -> Vec<String> {
        fn go(base: &Path, rel: &str, out: &mut Vec<String>, top: bool) {
            let Ok(rd) = std::fs::read_dir(base.join(rel)) else { return };
            let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let Ok(meta) = e.metadata() else { continue };
                if meta.is_dir() {
                    if top && (name.starts_with('.') || HIDDEN_DIRS.contains(&name.as_str())) {
                        continue;
                    }
                    go(base, &path, out, false);
                } else if meta.is_file() {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        go(&self.dir, "", &mut out, true);
        out
    }

    /// The test files: those the config lists, those of the language's layout, and the example files the
    /// config lists (never given to the agent).
    pub fn tests(&self) -> Vec<String> {
        let mut t: Vec<String> =
            self.walk().into_iter().filter(|f| self.lang.is_test(f) || self.files.test.contains(f)).collect();
        for f in &self.files.test {
            if !t.contains(f) && self.dir.join(f).is_file() {
                t.push(f.clone());
            }
        }
        t.sort();
        t
    }

    /// Writes the agent's working directory `wd` (empty): the exercise less its tests, examples and hidden
    /// folders.
    pub fn stage(&self, wd: &Path) -> Result<(), String> {
        let tests = self.tests();
        for rel in self.walk() {
            if tests.contains(&rel) || self.files.example.contains(&rel) {
                continue;
            }
            copy_file(&self.dir.join(&rel), &wd.join(&rel))?;
        }
        Ok(())
    }

    /// Copies the test files into `wd`, as the benchmark does after the model has answered. JavaScript
    /// `xtest(` (skipped) becomes `test(`, as the benchmark's `npm-test.sh` does; Java's `@Disabled(...)` lines
    /// go, as Aider's harness does.
    pub fn install_tests(&self, wd: &Path) -> Result<(), String> {
        for rel in self.tests() {
            copy_file(&self.dir.join(&rel), &wd.join(&rel))?;
            if self.lang == Lang::Javascript && rel.ends_with(".spec.js") {
                let p = wd.join(&rel);
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let fixed = text.replace("xtest(", "test(");
                std::fs::write(&p, fixed).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            if self.lang == Lang::Java && rel.ends_with(".java") {
                let p = wd.join(&rel);
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                std::fs::write(&p, enable_java_tests(&text)).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        Ok(())
    }

    /// Takes the test files and the builds of the tests out of `wd` again, so the model never sees them.
    pub fn remove_tests(&self, wd: &Path) {
        for rel in self.tests() {
            let _ = std::fs::remove_file(wd.join(&rel));
            // an emptied folder of tests goes too (C++ `test/`, Rust `tests/`)
            if let Some((dir, _)) = rel.rsplit_once('/') {
                let _ = std::fs::remove_dir(wd.join(dir));
            }
        }
        for d in ["target", "build", ".gradle", "__pycache__", ".pytest_cache", ".cargo-lock"] {
            let _ = std::fs::remove_dir_all(wd.join(d));
        }
    }
}

/// Folders of a working directory that hold installs and builds, not the agent's files.
const MACHINE_DIRS: [&str; 6] = ["node_modules", "vendor", "target", "build", ".cargo", ".git"];

/// Files that change what a test run collects or how it judges it (Python's configuration).
const RUN_CONFIG: [&str; 5] = ["conftest.py", "pytest.ini", "tox.ini", "setup.cfg", "pyproject.toml"];

impl Exercise {
    /// Whether `rel` is a file the test run would pick up as a test of the language, wherever it is.
    fn collected_as_test(&self, rel: &str) -> bool {
        let file = rel.rsplit('/').next().unwrap_or(rel);
        self.lang.is_test(rel)
            || match self.lang {
                Lang::Python => file.starts_with("test_") && file.ends_with(".py"),
                Lang::Javascript => file.ends_with(".test.js") || rel.contains("__tests__/"),
                _ => false,
            }
    }

    /// Puts `wd` back to what the benchmark's run would see besides the model's solution files: the files the
    /// agent was given that are not solution files (`Cargo.toml`, `CMakeLists.txt`, `go.mod`, `package.json`...)
    /// are restored from the exercise, and test files or test configuration the agent left behind are removed.
    /// Files the agent added that are not tests (a Rust module, a C++ header) stay.
    pub fn restore_harness(&self, wd: &Path) -> Result<(), String> {
        let solution = self.solution();
        let tests = self.tests();
        let given = self.walk();
        for rel in &given {
            if tests.contains(rel) || self.files.example.contains(rel) || solution.contains(rel) {
                continue;
            }
            copy_file(&self.dir.join(rel), &wd.join(rel))?;
        }
        fn go(ex: &Exercise, base: &Path, rel: &str, given: &[String], solution: &[String]) {
            let Ok(rd) = std::fs::read_dir(base.join(rel)) else { return };
            for e in rd.filter_map(Result::ok) {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
                if meta.is_dir() {
                    if rel.is_empty() && MACHINE_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    go(ex, base, &path, given, solution);
                } else if !solution.contains(&path) {
                    let config = ex.lang == Lang::Python && RUN_CONFIG.contains(&name.as_str());
                    if (ex.collected_as_test(&path) || config) && !given.contains(&path) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
        go(self, wd, "", &given, &solution);
        Ok(())
    }
}

/// A Java test file with its `@Disabled(...)` lines taken out, the pattern Aider's harness removes
/// (`@Disabled\([^)]*\)\s*\n`).
pub fn enable_java_tests(text: &str) -> String {
    let re = regex::Regex::new(r"@Disabled\([^)]*\)\s*\n").expect("a fixed pattern");
    re.replace_all(text, "").into_owned()
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))
}

/// The exercises of `lang` under the benchmark's `root`, sorted by name.
pub fn list(root: &Path, lang: Lang) -> Result<Vec<Exercise>, String> {
    let dir = root.join(lang.name()).join("exercises").join("practice");
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    paths.sort();
    paths.iter().map(|p| Exercise::read(lang, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn fixture(lang: Lang, name: &str, config: &str, files: &[&str]) -> (tempfile::TempDir, Exercise) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(lang.name()).join("exercises/practice").join(name);
        write(&dir, ".meta/config.json", config);
        write(&dir, ".docs/instructions.md", "Do the thing.\n");
        for f in files {
            write(&dir, f, &format!("content of {f}"));
        }
        let ex = Exercise::read(lang, &dir).unwrap();
        (root, ex)
    }

    fn all_files(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        fn go(base: &Path, rel: &str, out: &mut Vec<String>) {
            for e in std::fs::read_dir(base.join(rel)).unwrap() {
                let e = e.unwrap();
                let p = if rel.is_empty() {
                    e.file_name().to_string_lossy().into_owned()
                } else {
                    format!("{rel}/{}", e.file_name().to_string_lossy())
                };
                if e.metadata().unwrap().is_dir() {
                    go(base, &p, out);
                } else {
                    out.push(p);
                }
            }
        }
        go(dir, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn no_dot_folder_reaches_the_agent() {
        let (_root, ex) = fixture(
            Lang::Go,
            "pig-latin",
            r#"{"files":{"solution":["pig_latin.go"],"test":["pig_latin_test.go"],"example":[".meta/example.go"]}}"#,
            &[
                "pig_latin.go",
                "pig_latin_test.go",
                "go.mod",
                ".articles/config.json",
                ".articles/performance/content.md",
                ".approaches/a.md",
            ],
        );
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        assert_eq!(all_files(wd.path()), ["go.mod", "pig_latin.go"]);
    }

    #[test]
    fn grading_restores_the_harness_files_and_removes_stray_tests_but_keeps_added_sources() {
        let (_root, ex) = fixture(
            Lang::Python,
            "bob",
            r#"{"files":{"solution":["bob.py"],"test":["bob_test.py"],"example":[".meta/example.py"]}}"#,
            &["bob.py", "bob_test.py"],
        );
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        for (f, t) in [
            ("bob.py", "my solution"),
            ("helper.py", "an added module"),
            ("test_scratch.py", "def test_x(): assert False"),
            ("notes_test.py", "stray"),
            ("conftest.py", "import pytest"),
            ("pytest.ini", "[pytest]"),
            ("sub/test_more.py", "stray"),
        ] {
            write(wd.path(), f, t);
        }
        ex.restore_harness(wd.path()).unwrap();
        assert_eq!(all_files(wd.path()), ["bob.py", "helper.py"]);
        assert_eq!(std::fs::read_to_string(wd.path().join("bob.py")).unwrap(), "my solution");
    }

    #[test]
    fn grading_puts_back_an_edited_cargo_toml_and_drops_a_tests_folder() {
        let (_root, ex) = fixture(
            Lang::Rust,
            "react",
            r#"{"files":{"solution":["src/lib.rs"],"test":["tests/react.rs"],"example":[".meta/example.rs"]}}"#,
            &["src/lib.rs", "tests/react.rs", "Cargo.toml"],
        );
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        write(wd.path(), "Cargo.toml", "[dependencies]\nevil = \"1\"\n");
        write(wd.path(), "tests/mine.rs", "stray");
        write(wd.path(), "src/extra.rs", "an added module");
        write(wd.path(), "target/debug/x_test.rs", "build output");
        ex.restore_harness(wd.path()).unwrap();
        assert_eq!(std::fs::read_to_string(wd.path().join("Cargo.toml")).unwrap(), "content of Cargo.toml");
        assert!(!wd.path().join("tests/mine.rs").exists() && wd.path().join("src/extra.rs").exists());
        assert!(wd.path().join("target/debug/x_test.rs").exists(), "installs and builds are left alone");
    }

    #[test]
    fn the_cpp_workdir_has_stubs_and_the_build_file_but_no_test_example_or_meta() {
        let (_root, ex) = fixture(
            Lang::Cpp,
            "all-your-base",
            r#"{"files":{"solution":["all_your_base.cpp","all_your_base.h"],"test":["all_your_base_test.cpp"],"example":[".meta/example.cpp",".meta/example.h"]}}"#,
            &[
                "all_your_base.cpp",
                "all_your_base.h",
                "all_your_base_test.cpp",
                "CMakeLists.txt",
                "test/tests-main.cpp",
                "test/catch.hpp",
                ".meta/example.cpp",
                ".meta/example.h",
                ".meta/tests.toml",
                ".approaches/introduction.md",
            ],
        );
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        assert_eq!(all_files(wd.path()), ["CMakeLists.txt", "all_your_base.cpp", "all_your_base.h"]);
        assert_eq!(ex.file_list(), "all_your_base.cpp all_your_base.h");
        ex.install_tests(wd.path()).unwrap();
        assert_eq!(
            all_files(wd.path()),
            [
                "CMakeLists.txt",
                "all_your_base.cpp",
                "all_your_base.h",
                "all_your_base_test.cpp",
                "test/catch.hpp",
                "test/tests-main.cpp"
            ]
        );
        ex.remove_tests(wd.path());
        assert_eq!(all_files(wd.path()), ["CMakeLists.txt", "all_your_base.cpp", "all_your_base.h"]);
    }

    #[test]
    fn the_rust_workdir_keeps_cargo_toml_but_cargo_toml_is_not_a_solution_file() {
        let (_root, ex) = fixture(
            Lang::Rust,
            "accumulate",
            r#"{"files":{"solution":["src/lib.rs","Cargo.toml"],"test":["tests/accumulate.rs"],"example":[".meta/example.rs"]}}"#,
            &["src/lib.rs", "Cargo.toml", "tests/accumulate.rs", ".meta/example.rs", ".docs/hints.md"],
        );
        assert_eq!(ex.solution(), ["src/lib.rs"]);
        assert_eq!(ex.file_list(), "lib.rs");
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        assert_eq!(all_files(wd.path()), ["Cargo.toml", "src/lib.rs"]);
    }

    #[test]
    fn go_editor_cases_and_every_language_layout_test_stay_out() {
        let (_root, ex) = fixture(
            Lang::Go,
            "alphametics",
            r#"{"files":{"solution":["alphametics.go"],"test":["alphametics_test.go"],"example":[".meta/example.go"],"editor":["cases_test.go"],"invalidator":["go.mod"]}}"#,
            &["alphametics.go", "alphametics_test.go", "cases_test.go", "go.mod", ".meta/example.go", ".meta/gen.go"],
        );
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        assert_eq!(all_files(wd.path()), ["alphametics.go", "go.mod"]);
        assert!(
            Lang::Python.is_test("x/foo_test.py")
                && Lang::Javascript.is_test("a.spec.js")
                && !Lang::Javascript.is_test("a.js")
        );
    }

    #[test]
    fn javascript_skipped_tests_are_enabled_when_installed_and_nowhere_else() {
        let (_root, ex) = fixture(
            Lang::Javascript,
            "bob",
            r#"{"files":{"solution":["bob.js"],"test":["bob.spec.js"],"example":[".meta/proof.ci.js"]}}"#,
            &["bob.js", "package.json", "bob.spec.js", ".meta/proof.ci.js"],
        );
        std::fs::write(ex.dir.join("bob.spec.js"), "xtest('a', () => {});\ntest('b', () => {});\n").unwrap();
        let wd = tempfile::tempdir().unwrap();
        ex.stage(wd.path()).unwrap();
        assert!(!wd.path().join("bob.spec.js").exists());
        ex.install_tests(wd.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(wd.path().join("bob.spec.js")).unwrap(),
            "test('a', () => {});\ntest('b', () => {});\n"
        );
        assert!(
            std::fs::read_to_string(ex.dir.join("bob.spec.js")).unwrap().starts_with("xtest("),
            "the benchmark itself is not touched"
        );
    }

    #[test]
    fn the_prompt_joins_introduction_instructions_and_append() {
        let (_root, ex) = fixture(
            Lang::Python,
            "bob",
            r#"{"files":{"solution":["bob.py"],"test":["bob_test.py"],"example":[".meta/example.py"]}}"#,
            &["bob.py"],
        );
        write(&ex.dir, ".docs/introduction.md", "Intro.\n");
        write(&ex.dir, ".docs/instructions.append.md", "More.\n");
        let p = ex.prompt().unwrap();
        assert!(p.starts_with("Intro.\nDo the thing.\nMore.\n\n####\n"), "{p:?}");
        assert!(p.contains("supplied files: bob.py\n"));
        assert_eq!(ex.id(), "polyglot/python/bob");
    }

    #[test]
    fn java_disabled_tests_are_enabled_as_aider_does() {
        let text =
            "    @Test\n    @Disabled(\"Remove to run test\")\n    void b() {}\n    @Disabled\n    void c() {}\n";
        // the bare `@Disabled` stays: Aider's pattern needs the parentheses
        assert_eq!(enable_java_tests(text), "    @Test\n        void b() {}\n    @Disabled\n    void c() {}\n");
    }

    #[test]
    fn languages_parse() {
        assert_eq!(Lang::parse(" Rust ").unwrap(), Lang::Rust);
        assert_eq!(Lang::parse("Java").unwrap(), Lang::Java);
        assert!(Lang::Java.is_test("src/test/java/BobTest.java") && !Lang::Java.is_test("src/main/java/Bob.java"));
        assert!(Lang::parse("cobol").is_err());
    }
}
