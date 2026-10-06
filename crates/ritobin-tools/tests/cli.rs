//! Integration tests that run the built `ritobin-tools` binary.

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

/// A temporary directory for the files of one test.
struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Returns the path of `name` in the workspace directory.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Writes `content` to the file `name` and returns its path.
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, content).unwrap();
        path
    }

    /// Writes a text hashtable with the field names of [`BASE`] and returns its directory.
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

    /// Returns a command for the tool with a hashtable cache directory that does not exist and
    /// a config file in the workspace. The test therefore does not read the hashtables or the
    /// config installed on the machine.
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

/// Returns the bin hash of `name`: FNV-1a of the lowercased name.
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

/// Returns the standard error of `output` as one line. Removes ANSI color codes and the `│`
/// gutter that the diagnostic renderer adds to wrapped lines, and collapses whitespace. A test
/// uses it to match a message that the renderer may wrap at any word.
fn stderr_line(output: &Output) -> String {
    let text = stderr(output);
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            }
            '│' => {}
            _ => plain.push(c),
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn convert_round_trips_text_through_bin() {
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
fn convert_fails_when_output_is_another_input() {
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
    assert!(stderr_line(&output).contains("which is also an input"));
    assert_eq!(read(&text), edited);
    assert_eq!(fs::read(&bin).unwrap(), before);
}

#[test]
fn diff_exit_code_is_2_on_failure() {
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
fn convert_reads_stdin_and_writes_stdout() {
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
fn keep_hashed_prints_hashes_as_hex() {
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
fn convert_fails_on_invalid_text_and_lenient_converts_it() {
    let ws = Workspace::new();
    let broken = ws.write(
        "broken.rito",
        &BASE.replace("Size: f32 = 1", "Size: f32 = \"big\""),
    );

    let output = ws.tool().arg("convert").arg(&broken).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr_line(&output).contains("Type mismatch"));
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
fn diff_prints_csv_and_exit_code_is_1_when_bins_differ() {
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
fn diff_saves_patch_with_named_record() {
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
fn hashes_hash_prints_bin_hash() {
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
fn hashes_lookup_uses_text_tables() {
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

/// Writes a game directory with one archive containing `chunks`, given as `(chunk path, data)`
/// pairs. Returns the installation directory.
fn write_game(ws: &Workspace, chunks: &[(&str, &[u8])]) -> PathBuf {
    use std::{collections::BTreeMap, io::Write as _};

    use ltk_wad::{WadBuilder, WadChunkBuilder, WadChunkCompression, WadHash};

    let mut builder = WadBuilder::default();
    for (chunk, _) in chunks {
        builder = builder.with_chunk(
            WadChunkBuilder::default()
                .with_path(*chunk)
                .with_force_compression(WadChunkCompression::None),
        );
    }
    let data: BTreeMap<WadHash, Vec<u8>> = chunks
        .iter()
        .map(|(chunk, bytes)| (ltk_game_index::chunk_hash(chunk), bytes.to_vec()))
        .collect();
    let mut archive = std::io::Cursor::new(Vec::new());
    builder
        .build_to_writer(&mut archive, move |hash, writer| {
            writer.write_all(&data[&hash])?;
            Ok(())
        })
        .unwrap();

    let game = ws.path("League");
    let archives = game.join("Game").join("DATA").join("FINAL");
    fs::create_dir_all(&archives).unwrap();
    fs::write(archives.join("Test.wad.client"), archive.into_inner()).unwrap();
    game
}

#[test]
fn gamedata_apply_writes_bin_with_edits_and_override_patch() {
    let ws = Workspace::new();
    let tables = ws.field_table();
    let base = ws.write("base.rito", BASE);
    ws.tool().arg("convert").arg(&base).assert().success();
    let game = write_game(
        &ws,
        &[("data/skin0.bin", &fs::read(ws.path("base.bin")).unwrap())],
    );
    let game_args = |command: &mut Command| {
        command
            .arg("--game-dir")
            .arg(&game)
            .arg("--index-dir")
            .arg(ws.path("index"));
    };

    // Save a PTCH file with `diff --patch` and reference it from the manifest as an override.
    fs::create_dir_all(ws.path("layer")).unwrap();
    let edited = ws.write(
        "edited.rito",
        &BASE.replace("Size: f32 = 1", "Size: f32 = 2"),
    );
    ws.tool()
        .arg("diff")
        .arg(&base)
        .arg(&edited)
        .args(["--format", "summary", "--patch"])
        .arg(ws.path("layer").join("size.ptch"))
        .arg("--hashtable")
        .arg(&tables)
        .assert()
        .success();
    fs::write(
        ws.path("layer").join("game_data.yaml"),
        "version: 1\nmodules:\n  - target: data/skin0.bin\n    overrides: [size.ptch]\n    Characters/Test/Skins/Skin0:\n      Name: edited\n      +Tags: [c]\n",
    )
    .unwrap();

    let mut apply = ws.tool();
    apply
        .args(["gamedata", "apply"])
        .arg(ws.path("layer"))
        .arg("--output")
        .arg(ws.path("out"))
        .args(["--format", "json"]);
    game_args(&mut apply);
    let output = apply.output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["bins"][0]["target"], "data/skin0.bin");
    assert_eq!(report["bins"][0]["records"], 1);
    assert_eq!(report["bins"][0]["properties"], 2);
    assert_eq!(report["problems"], serde_json::json!([]));

    let written = ws.path("out").join("data").join("skin0.bin");
    let output = ws
        .tool()
        .arg("convert")
        .arg(&written)
        .args(["--output", "-", "--hashtable"])
        .arg(&tables)
        .output()
        .unwrap();
    let printed = stdout(&output);
    assert!(printed.contains("Size: f32 = 2"), "{printed}");
    assert!(printed.contains("Name: string = \"edited\""), "{printed}");
    assert!(printed.contains("\"c\""), "{printed}");

    // `apply` does not modify the game. The game still has the original value.
    let mut render = ws.tool();
    render.args(["gamedata", "render", "Characters/Test/Skins/Skin0:Name"]);
    game_args(&mut render);
    assert_eq!(stdout(&render.output().unwrap()), "base\n");

    let output = ws
        .tool()
        .args([
            "gamedata",
            "render",
            "Characters/Test/Skins/Skin0:Tags",
            "--bin",
        ])
        .arg(&written)
        .output()
        .unwrap();
    assert_eq!(stdout(&output), "[a, b, c]\n");
}

#[test]
fn gamedata_check_exits_1_when_a_problem_is_reported() {
    let ws = Workspace::new();
    let base = ws.write("base.rito", BASE);
    ws.tool().arg("convert").arg(&base).assert().success();
    let game = write_game(
        &ws,
        &[("data/skin0.bin", &fs::read(ws.path("base.bin")).unwrap())],
    );
    let manifest = ws.write(
        "game_data.yaml",
        "version: 1\nmodules:\n  - target: data/skin0.bin\n    Characters/Test/Skins/Skin0:\n      Size: 3\n  - entries:\n      Characters/Test/Skins/Skin9:\n        Size: 3\n",
    );

    ws.tool()
        .args(["gamedata", "check"])
        .arg(&manifest)
        .arg("--no-game")
        .assert()
        .success();

    let output = ws
        .tool()
        .args(["gamedata", "check"])
        .arg(&manifest)
        .arg("--game-dir")
        .arg(&game)
        .arg("--index-dir")
        .arg(ws.path("index"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = stdout(&output);
    assert!(report.contains("data/skin0.bin  1"), "{report}");
    assert!(
        report.contains("Entry Characters/Test/Skins/Skin9 is not declared by any game bin"),
        "{report}"
    );
    assert!(!ws.path("out").exists());
}

#[test]
fn hashtable_dir_prints_cache_directory() {
    let ws = Workspace::new();
    let output = ws.tool().arg("hashtable-dir").output().unwrap();
    assert_eq!(
        stdout(&output).trim(),
        ws.path("no-cache").to_str().unwrap()
    );
}

#[test]
fn bare_file_arguments_are_converted() {
    let ws = Workspace::new();
    let text = ws.write("skin0.rito", BASE);

    Command::cargo_bin("ritobin-tools")
        .unwrap()
        .arg(&text)
        .assert()
        .success();
    assert!(ws.path("skin0.bin").exists());
}
