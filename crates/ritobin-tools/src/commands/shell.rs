//! Windows Explorer integration: a right-click menu on bins, on their text and on folders.
//!
//! The menu is the classic kind, made of registry keys under `HKEY_CURRENT_USER\Software\Classes`,
//! so it needs no administrator rights. On Windows 11 Explorer shows such a menu under "Show more
//! options".
//!
//! Each class of item gets one `ritobin-tools` entry that opens a submenu. That entry is a verb
//! with an empty `SubCommands` value, which makes Explorer read the submenu from the `shell` key
//! under it.

use std::{fmt, io};

use clap::Subcommand;
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use crate::commands::output::{OutputArgs, columns, print};

#[derive(Subcommand, Debug)]
pub enum ShellCommand {
    /// Add the ritobin-tools menu to the Explorer right-click menu of the current user
    Install,

    /// Remove the ritobin-tools menu from the Explorer right-click menu
    Uninstall,

    /// Show which menu entries are installed and what they run
    Status {
        #[command(flatten)]
        output: OutputArgs,
    },
}

/// Where a user's own file classes are, under `HKEY_CURRENT_USER`.
const USER_CLASSES: &str = "Software\\Classes";

/// The key of the menu under the `shell` key of a class.
const MENU_KEY: &str = "ritobin-tools";

/// The text of the menu in Explorer.
const MENU_LABEL: &str = "ritobin-tools";

/// Stands for the clicked item in the arguments of an [`Entry`].
const CLICKED: &str = "%1";

/// The `CommandFlags` value that draws a line above an entry (`ECF_SEPARATORBEFORE`).
const SEPARATOR_BEFORE: u32 = 0x20;

/// The menu on one registry class.
struct Menu {
    /// The class, under [`USER_CLASSES`]. A `SystemFileAssociations` class applies to an
    /// extension whichever program opens it.
    class: &'static str,
    /// What the class is called in messages.
    on: &'static str,
    /// Whether the menu is put at the top of the right-click menu.
    top: bool,
    /// The entries of the submenu. Explorer lists them in the order of their keys.
    entries: &'static [Entry],
}

/// One entry of a submenu.
struct Entry {
    key: &'static str,
    label: &'static str,
    /// The arguments the tool is run with.
    args: &'static [&'static str],
    separator_before: bool,
}

const TEXT_TO_BIN: Entry = Entry {
    key: "convert",
    label: "Convert to .bin",
    args: &["--pause", "on-error", "convert", "--to", "bin", CLICKED],
    separator_before: false,
};

const SYNC_HASHTABLES: Entry = Entry {
    key: "sync",
    label: "Update hashtables",
    args: &["--pause", "always", "hashes", "sync"],
    separator_before: true,
};

/// Every menu the tool installs.
///
/// `.bin` is the extension of many files that are not League bins, and a registry menu cannot
/// look inside a file, so its menu is on all of them and is not put at the top. The legacy `.py`
/// extension of ritobin text gets no menu, because it is the extension of Python source.
const MENUS: &[Menu] = &[
    Menu {
        class: "SystemFileAssociations\\.bin",
        on: ".bin",
        top: false,
        entries: &[
            Entry {
                key: "convert",
                label: "Convert to .rito",
                args: &["--pause", "on-error", "convert", "--to", "rito", CLICKED],
                separator_before: false,
            },
            SYNC_HASHTABLES,
        ],
    },
    Menu {
        class: "SystemFileAssociations\\.rito",
        on: ".rito",
        top: true,
        entries: &[TEXT_TO_BIN],
    },
    Menu {
        class: "SystemFileAssociations\\.ritobin",
        on: ".ritobin",
        top: true,
        entries: &[TEXT_TO_BIN],
    },
    Menu {
        class: "Directory",
        on: "folders",
        top: false,
        entries: &[
            // A folder run always waits, so its count of converted files can be read.
            Entry {
                key: "convert-bin",
                label: "Convert all .bin to .rito",
                args: &[
                    "--pause",
                    "always",
                    "convert",
                    "--recursive",
                    "--to",
                    "rito",
                    CLICKED,
                ],
                separator_before: false,
            },
            Entry {
                key: "convert-text",
                label: "Convert all .rito to .bin",
                args: &[
                    "--pause",
                    "always",
                    "convert",
                    "--recursive",
                    "--to",
                    "bin",
                    CLICKED,
                ],
                separator_before: false,
            },
            SYNC_HASHTABLES,
        ],
    },
];

impl Menu {
    /// The key of the menu, under [`USER_CLASSES`].
    fn path(&self) -> String {
        format!("{}\\shell\\{MENU_KEY}", self.class)
    }

    /// The key of one of its entries, under [`USER_CLASSES`].
    fn entry_path(&self, entry: &Entry) -> String {
        format!("{}\\shell\\{}", self.path(), entry.key)
    }
}

impl Entry {
    /// The command line Explorer runs for this entry.
    fn command(&self, exe: &str) -> String {
        let mut command = format!("\"{exe}\"");
        for arg in self.args {
            command.push(' ');
            match *arg == CLICKED {
                true => command.push_str("\"%1\""),
                false => command.push_str(arg),
            }
        }
        command
    }
}

pub fn run(command: ShellCommand) -> Result<()> {
    let classes = registry(
        RegKey::predef(HKEY_CURRENT_USER).create_subkey(USER_CLASSES),
        "open",
        USER_CLASSES,
    )?
    .0;
    let exe = std::env::current_exe()
        .into_diagnostic()
        .wrap_err("Could not tell where the ritobin-tools executable is")?
        .to_string_lossy()
        .into_owned();
    let on: Vec<&str> = MENUS.iter().map(|menu| menu.on).collect();

    match command {
        ShellCommand::Install => {
            install(&classes, &exe)?;
            tracing::info!(
                "Added the ritobin-tools menu to the right-click menu of {}. It runs {exe}",
                on.join(", ")
            );
            tracing::info!("On Windows 11 the menu is under \"Show more options\".");
        }
        ShellCommand::Uninstall => match uninstall(&classes)?.as_slice() {
            [] => tracing::info!("The ritobin-tools menu is not installed. Nothing was removed."),
            removed => tracing::info!(
                "Removed the ritobin-tools menu from the right-click menu of {}",
                removed.join(", ")
            ),
        },
        ShellCommand::Status { output } => {
            let rows = status(&classes, &exe)?;
            print(&rows, output.format, |rows| {
                let cells: Vec<[String; 4]> = rows
                    .iter()
                    .map(|row| {
                        [
                            row.on.to_owned(),
                            row.entry.to_owned(),
                            row.state.to_string(),
                            row.command.clone().unwrap_or_else(|| "-".to_owned()),
                        ]
                    })
                    .collect();
                columns(["ON", "ENTRY", "STATE", "COMMAND"], &cells)
            })?;

            if rows.iter().any(|row| row.state == State::Outdated) {
                tracing::info!(
                    "An outdated entry runs another command than this version installs. Run `ritobin-tools shell install` to replace it."
                );
            } else if rows.iter().all(|row| row.state == State::Missing) {
                tracing::info!(
                    "The menu is not installed. Run `ritobin-tools shell install` to add it."
                );
            }
        }
    }
    Ok(())
}

/// Says which key a registry call failed on.
fn registry<T>(result: io::Result<T>, action: &str, path: &str) -> Result<T> {
    result
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to {action} the registry key {path}"))
}

/// Writes every menu under `classes`, each entry running `exe`.
fn install(classes: &RegKey, exe: &str) -> Result<()> {
    for menu in MENUS {
        let path = menu.path();
        // The entries of an older version would stay next to the new ones.
        remove(classes, &path)?;

        let (key, _) = registry(classes.create_subkey(&path), "create", &path)?;
        let set = |result: io::Result<()>| registry(result, "write", &path);
        set(key.set_value("MUIVerb", &MENU_LABEL))?;
        set(key.set_value("SubCommands", &""))?;
        if menu.top {
            set(key.set_value("Position", &"Top"))?;
        }

        for entry in menu.entries {
            let path = menu.entry_path(entry);
            let (key, _) = registry(classes.create_subkey(&path), "create", &path)?;
            let set = |result: io::Result<()>| registry(result, "write", &path);
            set(key.set_value("", &entry.label))?;
            // Explorer runs the entry once for each selected item, and for no more than 15 of
            // them unless this is set.
            set(key.set_value("MultiSelectModel", &"Player"))?;
            if entry.separator_before {
                set(key.set_value("CommandFlags", &SEPARATOR_BEFORE))?;
            }

            let path = format!("{path}\\command");
            let (key, _) = registry(classes.create_subkey(&path), "create", &path)?;
            registry(key.set_value("", &entry.command(exe)), "write", &path)?;
        }
    }
    Ok(())
}

/// Removes every menu under `classes`, and says what the ones that were there were on.
fn uninstall(classes: &RegKey) -> Result<Vec<&'static str>> {
    let mut removed = Vec::new();
    for menu in MENUS {
        if remove(classes, &menu.path())? {
            removed.push(menu.on);
        }
        // Installing made these when they were not there.
        remove_if_empty(classes, &format!("{}\\shell", menu.class));
        remove_if_empty(classes, menu.class);
    }
    Ok(removed)
}

/// Removes the key at `path` and everything under it. `false` when there was no such key.
fn remove(classes: &RegKey, path: &str) -> Result<bool> {
    match classes.delete_subkey_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => registry(Err(error), "remove", path),
    }
}

/// Removes the key at `path` when it has no values and no keys under it.
fn remove_if_empty(classes: &RegKey, path: &str) {
    let empty = classes
        .open_subkey(path)
        .is_ok_and(|key| key.enum_keys().next().is_none() && key.enum_values().next().is_none());
    if empty {
        let _ = classes.delete_subkey(path);
    }
}

/// Whether a menu entry is in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    /// It runs the command this version installs for this executable.
    Installed,
    /// It runs another command: another executable, or the arguments of another version.
    Outdated,
    Missing,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            State::Installed => "installed",
            State::Outdated => "outdated",
            State::Missing => "missing",
        })
    }
}

#[derive(Debug, Serialize)]
struct StatusRow {
    on: &'static str,
    entry: &'static str,
    state: State,
    /// The command line in the registry.
    command: Option<String>,
}

/// The state of every entry of every menu under `classes`, for a tool at `exe`.
fn status(classes: &RegKey, exe: &str) -> Result<Vec<StatusRow>> {
    let mut rows = Vec::new();
    for menu in MENUS {
        for entry in menu.entries {
            let path = format!("{}\\command", menu.entry_path(entry));
            let command = match classes.open_subkey(&path) {
                Ok(key) => Some(registry(key.get_value::<String, _>(""), "read", &path)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return registry(Err(error), "open", &path),
            };
            rows.push(StatusRow {
                on: menu.on,
                entry: entry.label,
                state: match &command {
                    Some(command) if *command == entry.command(exe) => State::Installed,
                    Some(_) => State::Outdated,
                    None => State::Missing,
                },
                command,
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::cli::{self, Commands, PauseMode};

    const EXE: &str = "C:\\Tools\\ritobin-tools.exe";

    /// A registry key of its own for one test, in place of the user's classes.
    struct Scratch {
        path: String,
        classes: RegKey,
    }

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = format!(
                "Software\\ritobin-tools-tests\\{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let (classes, _) = RegKey::predef(HKEY_CURRENT_USER)
                .create_subkey(&path)
                .unwrap();
            Self { path, classes }
        }

        fn value(&self, path: &str, name: &str) -> Option<String> {
            self.classes.open_subkey(path).ok()?.get_value(name).ok()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let user = RegKey::predef(HKEY_CURRENT_USER);
            let _ = user.delete_subkey_all(&self.path);
            // Fails while another test still has a key here, which is the test that removes it.
            let _ = user.delete_subkey("Software\\ritobin-tools-tests");
        }
    }

    fn states(scratch: &Scratch, exe: &str) -> Vec<State> {
        status(&scratch.classes, exe)
            .unwrap()
            .iter()
            .map(|row| row.state)
            .collect()
    }

    #[test]
    fn install_writes_a_submenu_whose_entries_run_the_executable() {
        let scratch = Scratch::new();
        install(&scratch.classes, EXE).unwrap();

        let menu = "SystemFileAssociations\\.bin\\shell\\ritobin-tools";
        assert_eq!(scratch.value(menu, "MUIVerb").as_deref(), Some(MENU_LABEL));
        assert_eq!(scratch.value(menu, "SubCommands").as_deref(), Some(""));
        assert_eq!(scratch.value(menu, "Position"), None);
        assert_eq!(
            scratch
                .value(&format!("{menu}\\shell\\convert"), "")
                .as_deref(),
            Some("Convert to .rito")
        );
        assert_eq!(
            scratch
                .value(&format!("{menu}\\shell\\convert\\command"), "")
                .as_deref(),
            Some("\"C:\\Tools\\ritobin-tools.exe\" --pause on-error convert --to rito \"%1\"")
        );
        assert_eq!(
            scratch
                .value(
                    "SystemFileAssociations\\.rito\\shell\\ritobin-tools",
                    "Position"
                )
                .as_deref(),
            Some("Top")
        );

        let states = states(&scratch, EXE);
        assert!(!states.is_empty());
        assert!(states.iter().all(|state| *state == State::Installed));
    }

    #[test]
    fn install_replaces_the_entries_of_an_earlier_install() {
        let scratch = Scratch::new();
        let old = "Directory\\shell\\ritobin-tools\\shell\\no-longer-an-entry";
        scratch
            .classes
            .create_subkey(format!("{old}\\command"))
            .unwrap();

        install(&scratch.classes, EXE).unwrap();
        assert!(scratch.classes.open_subkey(old).is_err());
    }

    #[test]
    fn status_tells_an_entry_of_another_executable_from_an_installed_one() {
        let scratch = Scratch::new();
        assert!(
            states(&scratch, EXE)
                .iter()
                .all(|state| *state == State::Missing)
        );

        install(&scratch.classes, "C:\\Old\\ritobin-tools.exe").unwrap();
        assert!(
            states(&scratch, EXE)
                .iter()
                .all(|state| *state == State::Outdated)
        );
    }

    #[test]
    fn uninstall_removes_the_menus_and_the_keys_made_for_them() {
        let scratch = Scratch::new();
        // Another program's entry on folders, which is not ours to remove.
        let other = "Directory\\shell\\other-program";
        scratch.classes.create_subkey(other).unwrap();

        install(&scratch.classes, EXE).unwrap();
        assert_eq!(
            uninstall(&scratch.classes).unwrap(),
            [".bin", ".rito", ".ritobin", "folders"]
        );
        assert!(uninstall(&scratch.classes).unwrap().is_empty());

        assert!(
            states(&scratch, EXE)
                .iter()
                .all(|state| *state == State::Missing)
        );
        assert!(
            scratch
                .classes
                .open_subkey("SystemFileAssociations\\.rito")
                .is_err()
        );
        assert!(scratch.classes.open_subkey(other).is_ok());
    }

    #[test]
    fn every_entry_runs_a_command_line_the_tool_accepts() {
        for menu in MENUS {
            for entry in menu.entries {
                let args = std::iter::once("ritobin-tools")
                    .chain(entry.args.iter().copied())
                    .map(|arg| match arg == CLICKED {
                        true => OsString::from("C:\\Data\\skin0.bin"),
                        false => OsString::from(arg),
                    });
                let cli = cli::try_parse(args)
                    .unwrap_or_else(|error| panic!("{} on {}: {error}", entry.label, menu.on));

                // Explorer opens a console window for the run, which closes when the run ends.
                assert_ne!(cli.pause, PauseMode::Never, "{}", entry.label);
                if entry.args.contains(&CLICKED) {
                    let Commands::Convert(convert) = cli.command else {
                        panic!("{} does not convert the clicked item", entry.label);
                    };
                    assert_eq!(convert.inputs, ["C:\\Data\\skin0.bin"]);
                }
            }
        }
    }

    #[test]
    fn the_entries_of_a_menu_are_listed_in_the_order_explorer_shows_them() {
        for menu in MENUS {
            let keys: Vec<&str> = menu.entries.iter().map(|entry| entry.key).collect();
            assert!(keys.is_sorted(), "{}: {keys:?}", menu.on);
        }
    }
}
