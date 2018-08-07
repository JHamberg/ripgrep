use hay::SHERLOCK;
use workdir::WorkDir;

// See: https://github.com/BurntSushi/ripgrep/issues/16
#[test]
fn r16() {
    let (wd, mut cmd) = WorkDir::new_with("r16");
    wd.create_dir(".git");
    wd.create(".gitignore", "ghi/");
    wd.create_dir("ghi");
    wd.create_dir("def/ghi");
    wd.create("ghi/toplevel.txt", "xyz");
    wd.create("def/ghi/subdir.txt", "xyz");

    cmd.arg("xyz");
    wd.assert_err(&mut cmd);
}

// See: https://github.com/BurntSushi/ripgrep/issues/25
#[test]
fn r25() {
    let (wd, mut cmd) = WorkDir::new_with("r25");
    wd.create_dir(".git");
    wd.create(".gitignore", "/llvm/");
    wd.create_dir("src/llvm");
    wd.create("src/llvm/foo", "test");

    cmd.arg("test");

    let lines: String = wd.stdout(&mut cmd);
    assert_eq_nice!("src/llvm/foo:test\n", lines);

    cmd.current_dir(wd.path().join("src"));
    let lines: String = wd.stdout(&mut cmd);
    assert_eq_nice!("llvm/foo:test\n", lines);
}
