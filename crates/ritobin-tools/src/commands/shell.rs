//! Installs and removes the Windows Explorer context menu for `.bin` files, ritobin text files
//! and folders.
//!
//! The menu is a classic context menu. It is defined by registry keys under
//! `HKEY_CURRENT_USER\Software\Classes`, so installing it does not require administrator rights.
//! On Windows 11, Explorer shows a classic menu under "Show more options".
//!
//! Each registry class gets one `ritobin-tools` verb with an empty `SubCommands` value. With that
//! value, Explorer shows the verb as a submenu and reads the submenu entries from the `shell`
//! subkey of the verb.

use std::{fmt, io};

use clap::Subcommand;
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

use crate::commands::output::{OutputArgs, columns, print};

#[derive(Subcommand, Debug)]
pub enum ShellCommand {
    /// Add the ritobin-tools menu to the Explorer context menu of the current user
    Install,

    /// Remove the ritobin-tools menu from the Explorer context menu
    Uninstall,

    /// Show the install state and the command of each menu entry
    Status {
        #[command(flatten)]
        output: OutputArgs,
    },
}

/// The registry key of the per-user file classes, relative to `HKEY_CURRENT_USER`.
const USER_CLASSES: &str = "Software\\Classes";

/// The name of the menu key under the `shell` key of a class.
const MENU_KEY: &str = "ritobin-tools";

/// The label of the menu in Explorer.
const MENU_LABEL: &str = "ritobin-tools";

/// The placeholder for the path of the clicked item in the arguments of an [`Entry`]. Explorer
/// replaces it with the path.
const CLICKED: &str = "%1";

/// The `CommandFlags` value that adds a separator line above an entry (`ECF_SEPARATORBEFORE`).
const SEPARATOR_BEFORE: u32 = 0x20;

/// The menu of one registry class.
struct Menu {
    /// The class key, relative to [`USER_CLASSES`]. A `SystemFileAssociations` class applies to
    /// a file extension regardless of the program associated with the extension.
    class: &'static str,
    /// The name of the class in messages and in the `status` output.
    on: &'static str,
    /// If `true`, the menu is placed at the top of the context menu.
    top: bool,
    /// The entries of the submenu. Explorer sorts them by key name.
    entries: &'static [Entry],
}

/// One entry of a submenu.
struct Entry {
    /// The name of the registry key of the entry.
    key: &'static str,
    /// The label of the entry in Explorer.
    label: &'static str,
    /// The command line arguments passed to the tool.
    args: &'static [&'static str],
    /// If `true`, a separator line is drawn above the entry.
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

/// All menus that `shell install` writes.
///
/// A registry menu is selected by file extension and cannot inspect file content. The `.bin` menu
/// therefore appears on every `.bin` file, including files that are not League bins, and it is
/// not placed at the top. The legacy `.py` extension of ritobin text has no menu, because `.py`
/// is also the extension of Python source files.
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
            // The folder entries use `--pause always`, so the console window stays open and
            // the conversion summary can be read.
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
    /// Returns the registry key of the menu, relative to [`USER_CLASSES`].
    fn path(&self) -> String {
        format!("{}\\shell\\{MENU_KEY}", self.class)
    }

    /// Returns the registry key of `entry`, relative to [`USER_CLASSES`].
    fn entry_path(&self, entry: &Entry) -> String {
        format!("{}\\shell\\{}", self.path(), entry.key)
    }
}

impl Entry {
    /// Returns the command line that Explorer runs for the entry, with `exe` as the program.
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

/// Runs a `shell` command against the registry of the current user.
pub fn run(command: ShellCommand) -> Result<()> {
    let classes = registry(
        RegKey::predef(HKEY_CURRENT_USER).create_subkey(USER_CLASSES),
        "open",
        USER_CLASSES,
    )?
    .0;
    let exe = std::env::current_exe()
        .into_diagnostic()
        .wrap_err("Failed to get the path of the ritobin-tools executable")?
        .to_string_lossy()
        .into_owned();
    let on: Vec<&str> = MENUS.iter().map(|menu| menu.on).collect();

    match command {
        ShellCommand::Install => {
            install(&classes, &exe)?;
            tracing::info!(
                "Added the ritobin-tools menu to the context menu of {}. The entries run {exe}",
                on.join(", ")
            );
            tracing::info!("On Windows 11, the menu is under \"Show more options\".");
        }
        ShellCommand::Uninstall => match uninstall(&classes)?.as_slice() {
            [] => tracing::info!("The ritobin-tools menu is not installed. Nothing was removed."),
            removed => tracing::info!(
                "Removed the ritobin-tools menu from the context menu of {}",
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
                    "The command of an outdated entry differs from the command this version installs. Run `ritobin-tools shell install` to replace it."
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

/// Adds the failed action and the registry key to the error of a registry call.
fn registry<T>(result: io::Result<T>, action: &str, path: &str) -> Result<T> {
    result
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to {action} the registry key {path}"))
}

/// Writes all menus under `classes`. Each entry runs `exe`.
fn install(classes: &RegKey, exe: &str) -> Result<()> {
    for menu in MENUS {
        let path = menu.path();
        // Remove the existing menu first. Otherwise entries that an older version installed
        // and this version no longer has would remain in the submenu.
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
            // Explorer runs the entry once for each selected item. Without this value it does
            // so for at most 15 selected items.
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

/// Removes all menus under `classes`. Returns the class names of the menus that existed.
fn uninstall(classes: &RegKey) -> Result<Vec<&'static str>> {
    let mut removed = Vec::new();
    for menu in MENUS {
        if remove(classes, &menu.path())? {
            removed.push(menu.on);
        }
        // `install` creates these parent keys if they do not exist. Remove them again if they
        // are empty.
        remove_if_empty(classes, &format!("{}\\shell", menu.class));
        remove_if_empty(classes, menu.class);
    }
    Ok(removed)
}

/// Removes the key at `path` with all its subkeys. Returns `false` if the key did not exist.
fn remove(classes: &RegKey, path: &str) -> Result<bool> {
    match classes.delete_subkey_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => registry(Err(error), "remove", path),
    }
}

/// Removes the key at `path` if it has no values and no subkeys.
fn remove_if_empty(classes: &RegKey, path: &str) {
    let empty = classes
        .open_subkey(path)
        .is_ok_and(|key| key.enum_keys().next().is_none() && key.enum_values().next().is_none());
    if empty {
        let _ = classes.delete_subkey(path);
    }
}

/// The install state of a menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    /// The registry command equals the command this version installs for this executable.
    Installed,
    /// The registry command differs: it has another executable path or other arguments.
    Outdated,
    /// The entry is not in the registry.
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

/// One row of the `shell status` output.
#[derive(Debug, Serialize)]
struct StatusRow {
    on: &'static str,
    entry: &'static str,
    state: State,
    /// The command line stored in the registry.
    command: Option<String>,
}

/// Returns the state of every entry of every menu under `classes`. An entry is `Installed` if
/// its registry command equals the command for `exe`.
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

    /// A temporary registry key that a test uses in place of the user classes key. The key is
    /// deleted when the value is dropped.
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
            // `delete_subkey` fails while the scratch key of another test exists. The last test
            // to finish removes the parent key.
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
    fn install_writes_submenu_with_entry_commands() {
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
    fn install_removes_entries_of_previous_install() {
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
    fn status_reports_entry_of_other_executable_as_outdated() {
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
    fn uninstall_removes_menus_and_empty_parent_keys() {
        let scratch = Scratch::new();
        // A folder menu entry of another program. `uninstall` must not remove it.
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
    fn entry_arguments_parse_as_valid_command_lines() {
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

                // Explorer runs the command in a new console window that closes on exit, so every
                // entry must set a pause mode.
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
    fn menu_entry_keys_are_sorted() {
        for menu in MENUS {
            let keys: Vec<&str> = menu.entries.iter().map(|entry| entry.key).collect();
            assert!(keys.is_sorted(), "{}: {keys:?}", menu.on);
        }
    }
}
