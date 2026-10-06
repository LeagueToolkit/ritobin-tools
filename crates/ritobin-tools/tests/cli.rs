//! Runs the built binary the way a user does.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use assert_cmd::Command;

const BASE: &str = r#"#PROP_text
type: string = "PROP"
version: u32 = 3
linked: list[string] = { "shared.bin" }
entries: map[hash, embed] = {
    "Characters/Test/Skins/Skin0" = SkinCharacterDataProperties {
        Size: f32 = 1
        Name: string = "base"
        Tags: list[string] = { "a", "b" }
    }
}
"#;

const FIELDS: &[&str] = &["Size", "Name", "Tags"];

struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// A directory of text tables naming the fields of [`BASE`].
    fn field_table(&self) -> PathBuf {
        let dir = self.path("tables");
        fs::create_dir_all(&dir).unwrap();
        let table: String = FIELDS
            .iter()
            .map(|name| format!("{:08x} {name}\n", fnv1a(name)))
            .collect();
        fs::write(dir.join("hashes.binfields.txt"), table).unwrap();
        dir
    }

    /// The tool, pointed at an empty cache so the tables installed on the machine are never read.
    fn tool(&self) -> Command {
        let mut command = Command::cargo_bin("ritobin-tools").unwrap();
        command
            .arg("--hashtable-dir")
            .arg(self.path("no-cache"))
            .arg("--config")
            .arg(self.path("config.toml"));
        command
    }
}

/// The hash a bin gives a name: FNV-1a of it in lower case.
fn fnv1a(name: &str) -> u32 {
    name.to_ascii_lowercase()
        .bytes()
        .fold(0x811c_9dc5, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        })
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn text_converts_to_a_bin_and_back_to_the_same_text() {
    let ws = Workspace::new();
    let text = ws.write("skin0.rito", BASE);
    let tables = ws.field_table();

    ws.tool().arg("convert").arg(&text).assert().success();
    let bin = ws.path("skin0.bin");
    assert!(fs::read(&bin).unwrap().starts_with(b"PROP"));

    let back = ws.path("back.rito");
    ws.tool()
        .arg("convert")
        .arg(&bin)
        .arg("--output")
        .arg(&back)
        .arg("--hashtable")
        .arg(&tables)
        .assert()
        .success();

    let printed = read(&back);
    assert!(printed.contains("Size: f32 = 1"));
    assert!(printed.contains("Name: string = \"base\""));

    let again = ws.path("again.bin");
    ws.tool()
        .arg("convert")
        .arg(&back)
        .arg("--output")
        .arg(&again)
        .assert()
        .success();
    assert_eq!(fs::read(&bin).unwrap(), fs::read(&again).unwrap());

    let output = ws
        .tool()
        .arg("convert")
        .arg(&again)
        .args(["--output", "-", "--hashtable"])
        .arg(&tables)
        .output()
        .unwrap();
    assert_eq!(stdout(&output), printed);
}

#[test]
fn a_bin_and_its_text_given_together_are_both_left_alone() {
    let ws = Workspace::new();
    let text = ws.write("skin0.rito", BASE);
    ws.tool().arg("convert").arg(&text).assert().success();
    let bin = ws.path("skin0.bin");
    let before = fs::read(&bin).unwrap();
    let edited = BASE.replace("Size: f32 = 1", "Size: f32 = 2");
    fs::write(&text, &edited).unwrap();

    let output = ws
        .tool()
        .arg("convert")
        .arg(&bin)
        .arg(&text)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("which is also an input"));
    assert_eq!(read(&text), edited);
    assert_eq!(fs::read(&bin).unwrap(), before);
}

#[test]
fn diff_with_exit_code_tells_a_failure_from_a_difference() {
    let ws = Workspace::new();
    let base = ws.write("base.rito", BASE);

    let output = ws
        .tool()
        .arg("diff")
        .arg(&base)
        .arg(ws.path("missing.rito"))
        .arg("--exit-code")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn convert_reads_standard_input_and_writes_standard_output() {
    let ws = Workspace::new();
    let output = ws
        .tool()
        .args(["convert", "-", "--keep-hashed"])
        .write_stdin(BASE)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"PROP"));

    let output = ws
        .tool()
        .args(["convert", "-", "--keep-hashed"])
        .write_stdin(output.stdout)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(stdout(&output).starts_with("#PROP_text"));
}

#[test]
fn keep_hashed_leaves_every_hash_as_hex() {
    let ws = Workspace::new();
    let text = ws.write("skin0.rito", BASE);
    let tables = ws.field_table();
    ws.tool().arg("convert").arg(&text).assert().success();

    let output = ws
        .tool()
        .arg("convert")
        .arg(ws.path("skin0.bin"))
        .args(["--output", "-", "--keep-hashed", "--hashtable"])
        .arg(&tables)
        .output()
        .unwrap();
    let printed = stdout(&output);
    assert!(!printed.contains("Size"));
    assert!(printed.contains(&format!("0x{:x}: f32 = 1", fnv1a("Size"))));
}

#[test]
fn text_with_a_problem_fails_and_names_the_problem() {
    let ws = Workspace::new();
    let broken = ws.write(
        "broken.rito",
        &BASE.replace("Size: f32 = 1", "Size: f32 = \"big\""),
    );

    let output = ws.tool().arg("convert").arg(&broken).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("Type mismatch"));
    assert!(!ws.path("broken.bin").exists());

    ws.tool()
        .arg("convert")
        .arg(&broken)
        .arg("--lenient")
        .assert()
        .success();
    assert!(ws.path("broken.bin").exists());
}

#[test]
fn diff_reports_changes_as_csv_and_sets_the_exit_code() {
    let ws = Workspace::new();
    let base = ws.write("base.rito", BASE);
    let edited = ws.write(
        "edited.rito",
        &BASE
            .replace("Size: f32 = 1", "Size: f32 = 2")
            .replace("        Name: string = \"base\"\n", ""),
    );
    let tables = ws.field_table();

    let output = ws
        .tool()
        .arg("diff")
        .arg(&base)
        .arg(&edited)
        .args(["--format", "csv", "--exit-code", "--hashtable"])
        .arg(&tables)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));

    let object = format!("0x{:08x}", fnv1a("Characters/Test/Skins/Skin0"));
    let class = format!("0x{:08x}", fnv1a("SkinCharacterDataProperties"));
    assert_eq!(
        stdout(&output),
        format!(
            "kind,object,object_name,class,path,type,old,new\nchanged,{object},,{class},Size,f32,1,2\nremoved,{object},,{class},Name,string,\"\"\"base\"\"\",\n"
        )
    );

    ws.tool()
        .arg("diff")
        .arg(&base)
        .arg(&base)
        .arg("--exit-code")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn diff_saves_a_patch_that_names_the_changed_property() {
    let ws = Workspace::new();
    let base = ws.write("base.rito", BASE);
    let edited = ws.write(
        "edited.rito",
        &BASE.replace("Size: f32 = 1", "Size: f32 = 2"),
    );
    let tables = ws.field_table();
    let patch = ws.path("size.ptch.bin");

    ws.tool()
        .arg("diff")
        .arg(&base)
        .arg(&edited)
        .args(["--format", "json", "--output"])
        .arg(ws.path("diff.json"))
        .arg("--patch")
        .arg(&patch)
        .arg("--hashtable")
        .arg(&tables)
        .assert()
        .success();
    assert!(fs::read(&patch).unwrap().starts_with(b"PTCH"));

    let document: serde_json::Value = serde_json::from_str(&read(&ws.path("diff.json"))).unwrap();
    assert_eq!(document["patch"]["records"], 1);
    assert_eq!(document["patch"]["exact"], true);
    assert_eq!(document["changes"][0]["path"], "Size");

    let output = ws
        .tool()
        .arg("convert")
        .arg(&patch)
        .args(["--output", "-", "--keep-hashed"])
        .output()
        .unwrap();
    let printed = stdout(&output);
    assert!(printed.starts_with("#PTCH_text"));
    assert!(printed.contains("path: string = \"Size\""));
    assert!(printed.contains("value: f32 = 2"));
}

#[test]
fn hashes_hash_prints_the_bin_hash_of_a_name() {
    let ws = Workspace::new();
    let output = ws
        .tool()
        .args(["hashes", "hash", "mName", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(rows[0]["hash"], format!("0x{:08x}", fnv1a("mName")));
    assert_eq!(rows[0]["known_in"], serde_json::json!([]));
}

#[test]
fn hashes_lookup_reads_the_extra_text_tables() {
    let ws = Workspace::new();
    let tables = ws.field_table();
    let output = ws
        .tool()
        .args(["hashes", "lookup"])
        .arg(format!("{:08x}", fnv1a("Size")))
        .args(["--format", "json", "--hashtable"])
        .arg(&tables)
        .output()
        .unwrap();

    let rows: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(rows[0]["table"], "binfields");
    assert_eq!(rows[0]["name"], "Size");
}

#[test]
fn hashtable_dir_prints_the_cache_directory_in_use() {
    let ws = Workspace::new();
    let output = ws.tool().arg("hashtable-dir").output().unwrap();
    assert_eq!(
        stdout(&output).trim(),
        ws.path("no-cache").to_str().unwrap()
    );
}

#[test]
fn files_dropped_on_the_executable_are_converted() {
    let ws = Workspace::new();
    let text = ws.write("skin0.rito", BASE);

    Command::cargo_bin("ritobin-tools")
        .unwrap()
        .arg(&text)
        .assert()
        .success();
    assert!(ws.path("skin0.bin").exists());
}
