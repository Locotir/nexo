use super::*;

#[test]
fn an_exported_env_file_is_read() {
    let env = env_file("export STAGING_TOKEN=a\n  export STAGING_HOST = \"b\"\n# c=d\n");
    assert_eq!(env, [("STAGING_TOKEN".to_string(), "a".to_string()), ("STAGING_HOST".into(), "b".into())]);
}

#[test]
fn a_deploy_program_that_ignores_the_environment_fails() {
    let wd = tempfile::tempdir().unwrap();
    let task = cross_session();
    task.materialize(wd.path()).unwrap();
    (task.solution)(wd.path());
    assert!(cross_check(wd.path()).ok, "{}", cross_check(wd.path()).detail);
    put(wd.path(), "src/bin/deploy.rs", &format!("fn main() {{\n    println!(\"deploying to {HOST}\");\n}}\n"));
    let out = cross_check(wd.path());
    assert!(!out.ok && out.detail.contains("deploy"), "{}", out.detail);
}
