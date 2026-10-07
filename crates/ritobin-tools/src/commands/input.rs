//! Reads the bin inputs of a command: a file, standard input or a bin of the game.

use std::cell::OnceCell;

use camino::Utf8Path;
use miette::Result;

use crate::{
    commands::gamedata::GameArgs,
    context::Context,
    document::{Document, ReadOptions},
    game::{BinRef, Game},
};

/// The prefix of an input that is read from the game, for example
/// `game:data/characters/teemo/skins/skin0.bin`.
pub const GAME_PREFIX: &str = "game:";

/// Returns the text after [`GAME_PREFIX`] if `input` starts with the prefix. Otherwise returns
/// `None`.
pub fn game_bin(input: &Utf8Path) -> Option<&str> {
    input.as_str().strip_prefix(GAME_PREFIX)
}

/// Reads the inputs of one command. The game is opened when the first input with
/// [`GAME_PREFIX`] is read, and stays open for the following inputs.
pub struct Inputs<'a> {
    ctx: &'a Context,
    game_args: &'a GameArgs,
    options: ReadOptions,
    game: OnceCell<Game>,
}

impl<'a> Inputs<'a> {
    pub fn new(ctx: &'a Context, game_args: &'a GameArgs, options: ReadOptions) -> Self {
        Self {
            ctx,
            game_args,
            options,
            game: OnceCell::new(),
        }
    }

    /// Reads the document of `input`.
    ///
    /// An input with [`GAME_PREFIX`] is read from the game. The text after the prefix is parsed
    /// with [`BinRef::parse`]. Fails if it selects no bin or more than one bin. Any other input
    /// is read with [`Document::read`].
    pub fn read(&self, input: &Utf8Path) -> Result<Document> {
        let Some(text) = game_bin(input) else {
            return Document::read(input, self.options);
        };
        let bins = BinRef::parse(text)?;
        let game = self.game()?;

        let chunk = match game.select(&bins).as_slice() {
            [chunk] => *chunk,
            [] => miette::bail!("{}", not_found(&bins)),
            chunks => {
                let names: Vec<String> = chunks
                    .iter()
                    .map(|chunk| format!("{GAME_PREFIX}{}", game.chunk_name(*chunk)))
                    .collect();
                miette::bail!(
                    "{input} selects {} bins, but one is required. Pass one of: {}",
                    chunks.len(),
                    names.join(", ")
                );
            }
        };
        let data = game
            .chunk(chunk)?
            .ok_or_else(|| miette::miette!("{}", not_found(&bins)))?;
        Document::parse(input.as_str(), data, self.options)
    }

    /// Returns the game. Opens it on the first call.
    fn game(&self) -> Result<&Game> {
        match self.game.get() {
            Some(game) => Ok(game),
            None => {
                let game = self.game_args.open(self.ctx)?;
                Ok(self.game.get_or_init(|| game))
            }
        }
    }
}

/// Returns the message for a selection that matches no bin of the game.
pub fn not_found(bins: &BinRef) -> String {
    match bins {
        BinRef::Chunk { name, .. } => format!("No game archive contains the bin {name}"),
        BinRef::Entry { name, .. } => format!("No game bin declares the entry {name}"),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_meta::{Bin, BinFile, BinObject, property::values};

    use super::*;
    use crate::{document::to_bin, game::testing::Installation};

    fn bin(object: u32, value: i32) -> Bin {
        Bin::builder()
            .object(
                BinObject::builder(object, 0xaaaa_0001u32)
                    .property(0x10u32, values::I32::new(value))
                    .build(),
            )
            .build()
    }

    fn bytes(bin: &Bin) -> Vec<u8> {
        to_bin(&bin.clone().into()).unwrap()
    }

    fn game_args(installation: &Installation) -> GameArgs {
        GameArgs {
            game_dir: Some(installation.root.clone()),
            index_dir: Some(installation.root.join("index")),
        }
    }

    #[test]
    fn read_returns_game_bin_for_path_and_for_entry() {
        let installation = Installation::new();
        installation.archive(
            "A.wad.client",
            &[
                ("data/one.bin", &bytes(&bin(1, 10))),
                ("data/two.bin", &bytes(&bin(2, 20))),
            ],
        );
        let (ctx, args) = (Context::for_tests(None), game_args(&installation));
        let inputs = Inputs::new(&ctx, &args, ReadOptions::default());

        let by_path = inputs.read(Utf8Path::new("game:data/One.bin")).unwrap();
        assert_eq!(by_path.file, BinFile::Prop(bin(1, 10)));
        let by_entry = inputs.read(Utf8Path::new("game:0x00000002")).unwrap();
        assert_eq!(by_entry.file, BinFile::Prop(bin(2, 20)));
    }

    #[test]
    fn read_fails_if_selection_matches_no_bin_or_several_bins() {
        let installation = Installation::new();
        installation.archive("A.wad.client", &[("data/one.bin", &bytes(&bin(1, 10)))]);
        installation.archive("B.wad.client", &[("data/other.bin", &bytes(&bin(1, 30)))]);
        let (ctx, args) = (Context::for_tests(None), game_args(&installation));
        let inputs = Inputs::new(&ctx, &args, ReadOptions::default());
        let error = |input: &str| inputs.read(Utf8Path::new(input)).unwrap_err().to_string();

        assert_eq!(
            error("game:data/missing.bin"),
            "No game archive contains the bin data/missing.bin"
        );
        assert_eq!(
            error("game:0x00000009"),
            "No game bin declares the entry 0x00000009"
        );
        assert!(
            error("game:0x00000001").contains("selects 2 bins, but one is required"),
            "{}",
            error("game:0x00000001")
        );
    }

    #[test]
    fn read_reads_file_without_game_prefix_and_does_not_open_game() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("one.bin")).unwrap();
        std::fs::write(&path, bytes(&bin(1, 10))).unwrap();

        // No game directory is set. Reading a file must not require one.
        let (ctx, args) = (Context::for_tests(None), GameArgs::default());
        let inputs = Inputs::new(&ctx, &args, ReadOptions::default());
        assert_eq!(inputs.read(&path).unwrap().file, BinFile::Prop(bin(1, 10)));

        let error = inputs.read(Utf8Path::new("game:data/one.bin")).unwrap_err();
        assert!(error.to_string().contains("No game directory is set"));
    }
}
