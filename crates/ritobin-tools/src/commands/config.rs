use camino::Utf8Path;
use clap::Subcommand;
use colored::Colorize;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::{
    config::{self, AppConfig},
    context::Context,
};

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show the current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Key to set. Nested keys are separated with `.` (e.g. `hashtable_dir`,
        /// `print_config.indent_size`)
        key: String,
        /// New value
        value: String,
    },
    /// Reset the configuration to its defaults
    Reset,
}

/// Runs a `config` command.
pub fn run(ctx: &Context, command: ConfigCommand) -> Result<()> {
    let path = ctx.config_path.as_deref().ok_or_else(|| {
        miette::miette!("Failed to determine the config file path. Pass --config")
    })?;
    match command {
        ConfigCommand::Show => show(ctx, path),
        ConfigCommand::Set { key, value } => set(path, &key, &value),
        ConfigCommand::Reset => reset(path),
    }
}

/// Prints the config file path and the current configuration.
fn show(ctx: &Context, path: &Utf8Path) -> Result<()> {
    let existence = match path.exists() {
        true => "",
        false => " (file does not exist, showing the default values)",
    };
    println!("{} {path}{existence}", "config_file:".bright_white());
    println!();
    print!(
        "{}",
        toml::to_string_pretty(&ctx.config)
            .into_diagnostic()
            .wrap_err("Failed to serialize the config")?
    );
    if ctx.config.hashtable_dir.is_none() {
        println!();
        println!(
            "{} {}",
            "hashtable_dir:".bright_white(),
            "(not set, using the shared cache)".bright_yellow()
        );
    }
    Ok(())
}

/// Sets `key` to `value` in the config file at `path`. Fails without writing the file if the
/// key is unknown or the value has the wrong type.
fn set(path: &Utf8Path, key: &str, value: &str) -> Result<()> {
    let mut table = config::load_table(path)?;
    insert(&mut table, key, parse_value(value))?;

    // Validate the edited table by deserializing it as a config before the file is written.
    let config: AppConfig = table
        .try_into()
        .map_err(|error| miette::miette!("`{key}` cannot be set to `{value}`: {error}"))?;
    // Deserialization ignores unknown keys. The config is serialized again and checked for the
    // key, which detects an unknown key.
    let written = toml::Table::try_from(&config)
        .into_diagnostic()
        .wrap_err("Failed to serialize the config")?;
    if !contains(&written, key) {
        miette::bail!(
            "`{key}` is not a config key. Run `ritobin-tools config show` to list the valid keys"
        );
    }
    config::save(path, &config)?;

    println!("{}", format!("Set {key} = {value}").bright_green());
    Ok(())
}

/// Writes the default configuration to `path`.
fn reset(path: &Utf8Path) -> Result<()> {
    config::save(path, &AppConfig::default())?;
    println!("{}", format!("Reset {path} to the defaults").bright_green());
    Ok(())
}

/// Inserts `value` at the dotted `key`. Creates missing parent tables.
fn insert(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<()> {
    let mut segments: Vec<&str> = key.split('.').collect();
    let leaf = segments.pop().filter(|leaf| !leaf.is_empty());
    let Some(leaf) = leaf else {
        miette::bail!("`{key}` is not a config key");
    };

    let mut current = table;
    for segment in segments {
        current = current
            .entry(segment)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| miette::miette!("`{segment}` in `{key}` is not a table"))?;
    }
    current.insert(leaf.to_owned(), value);
    Ok(())
}

/// Returns `true` if `table` has a value at the dotted `key`.
fn contains(table: &toml::Table, key: &str) -> bool {
    let mut segments = key.split('.');
    let Some(mut value) = segments.next().and_then(|first| table.get(first)) else {
        return false;
    };
    for segment in segments {
        match value.get(segment) {
            Some(inner) => value = inner,
            None => return false,
        }
    }
    true
}

/// Parses `value` as a TOML boolean or integer. Falls back to a TOML string.
fn parse_value(value: &str) -> toml::Value {
    if let Ok(b) = value.parse::<bool>() {
        return toml::Value::Boolean(b);
    }
    if let Ok(i) = value.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    toml::Value::String(value.to_string())
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;

    fn temp_config() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join(config::CONFIG_FILE_NAME)).unwrap();
        (dir, path)
    }

    #[test]
    fn set_writes_top_level_and_nested_keys() {
        let (_guard, path) = temp_config();
        set(&path, "hashtable_dir", "C:/hashes").unwrap();
        set(&path, "print_config.indent_size", "2").unwrap();
        set(&path, "print_config.wrap.inline_structs", "true").unwrap();

        let config = config::load(&path).unwrap();
        assert_eq!(
            config.hashtable_dir.as_deref(),
            Some(Utf8Path::new("C:/hashes"))
        );
        assert_eq!(config.print_config.indent_size, 2);
        assert!(config.print_config.inline_structs);
    }

    #[test]
    fn set_rejects_value_of_wrong_type() {
        let (_guard, path) = temp_config();
        let error = set(&path, "print_config.indent_size", "wide").unwrap_err();
        assert!(error.to_string().contains("cannot be set"));
        assert!(!path.exists());
    }

    #[test]
    fn set_rejects_unknown_key() {
        let (_guard, path) = temp_config();
        for key in ["print_config.line_width", "no_such_key"] {
            let error = set(&path, key, "80").unwrap_err();
            assert!(error.to_string().contains("is not a config key"), "{key}");
        }
        assert!(!path.exists());
    }

    #[test]
    fn reset_writes_default_config() {
        let (_guard, path) = temp_config();
        set(&path, "print_config.indent_size", "2").unwrap();
        reset(&path).unwrap();
        assert_eq!(config::load(&path).unwrap(), AppConfig::default());
    }
}
