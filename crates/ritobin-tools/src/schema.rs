//! The class schema that is observed from the bins of the installed game.
//!
//! A bin value has a type, and a bin declaration does not write it. The type of a property is a
//! function of its class and its name: in the game, no class has a property with two types.
//! [`ObservedSchema`] records the type of every property that a game bin uses. It types the
//! properties of a declaration when the declaration is built back into a bin.
//!
//! The schema knows only the classes and the properties that occur in a game bin. A class or a
//! property that the game supports but no bin uses is missing.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufReader, Cursor},
    sync::atomic::{AtomicUsize, Ordering},
};

use camino::Utf8Path;
use indexmap::IndexMap;
use ltk_game_data::{Schema, Shape};
use ltk_hash::BinHash;
use ltk_meta::{BinFile, PropertyKind, PropertyValueEnum, path::ValueShape};
use ltk_wad::Wad;
use miette::{IntoDiagnostic, Result, WrapErr};

use crate::game::ArchiveBins;

/// The magic bytes of a schema cache file.
const MAGIC: &[u8; 4] = b"RTCS";

/// The format version of a schema cache file.
const VERSION: u32 = 1;

/// The byte that encodes a missing key kind or item kind in a cache file. It is not the value
/// of any [`PropertyKind`].
const NO_KIND: u8 = 0x7f;

/// The type of a property: its kind, the key kind of a map, and the item kind of a list, an
/// option or a map.
type Key = (PropertyKind, Option<PropertyKind>, Option<PropertyKind>);

/// The types of the properties of the classes that the bins of a game use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedSchema {
    /// The type of each property, by class hash and property name hash.
    shapes: HashMap<(u32, u32), Shape>,
    classes: HashSet<u32>,
}

impl Schema for ObservedSchema {
    fn expected(&self, class: BinHash, field: BinHash) -> Option<Shape> {
        self.shapes.get(&(class.0, field.0)).copied()
    }

    fn has_class(&self, class: BinHash) -> bool {
        self.classes.contains(&class.0)
    }
}

impl ObservedSchema {
    /// Returns the number of classes.
    pub fn classes(&self) -> usize {
        self.classes.len()
    }

    /// Returns the number of properties, counted once per class.
    pub fn properties(&self) -> usize {
        self.shapes.len()
    }

    /// Reads every bin chunk of `archives` and records the type of each property. The archives
    /// are read on all processor cores.
    ///
    /// A bin that cannot be read is skipped. If a property has more than one type, the most
    /// frequent type is kept.
    pub fn observe(archives: &[ArchiveBins]) -> Self {
        let next = AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
        let mut total = Observer::default();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads.clamp(1, archives.len().max(1)))
                .map(|_| {
                    scope.spawn(|| {
                        let mut observer = Observer::default();
                        while let Some(archive) = archives.get(next.fetch_add(1, Ordering::Relaxed))
                        {
                            observer.archive(archive);
                        }
                        observer
                    })
                })
                .collect();
            for worker in workers {
                // A worker does not panic. If it does, its bins are missing from the schema.
                if let Ok(observer) = worker.join() {
                    total.merge(observer);
                }
            }
        });
        total.into_schema()
    }

    /// Returns the schema of the objects of `file`. Tests and callers without a game use it.
    #[cfg(test)]
    pub fn of(file: &BinFile) -> Self {
        let mut observer = Observer::default();
        observer.file(file);
        observer.into_schema()
    }

    /// Writes the schema to the cache file at `path`. `fingerprint` identifies the archive set
    /// that the schema was observed from.
    pub fn save(&self, path: &Utf8Path, fingerprint: u64) -> Result<()> {
        let mut out = Vec::with_capacity(16 + self.shapes.len() * 11 + self.classes.len() * 4);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&fingerprint.to_le_bytes());

        // Sorted, so that two saves of one schema write the same bytes.
        let mut shapes: Vec<_> = self.shapes.iter().collect();
        shapes.sort_by_key(|(key, _)| **key);
        out.extend_from_slice(&(shapes.len() as u32).to_le_bytes());
        let kind = |kind: Option<PropertyKind>| kind.map_or(NO_KIND, u8::from);
        for ((class, field), shape) in shapes {
            out.extend_from_slice(&class.to_le_bytes());
            out.extend_from_slice(&field.to_le_bytes());
            out.extend_from_slice(&[u8::from(shape.kind), kind(shape.key), kind(shape.item)]);
        }
        let mut classes: Vec<_> = self.classes.iter().collect();
        classes.sort();
        out.extend_from_slice(&(classes.len() as u32).to_le_bytes());
        for class in classes {
            out.extend_from_slice(&class.to_le_bytes());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| format!("Failed to create {parent}"))?;
        }
        std::fs::write(path, out)
            .into_diagnostic()
            .wrap_err_with(|| format!("Failed to write {path}"))
    }

    /// Reads the schema from the cache file at `path`. Returns `None` if the file is missing,
    /// is not a schema cache of this version, or was observed from an archive set other than
    /// `fingerprint`.
    pub fn load(path: &Utf8Path, fingerprint: u64) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        let mut reader = Reader(&data);
        if reader.bytes(4)? != MAGIC
            || reader.u32()? != VERSION
            || u64::from_le_bytes(reader.bytes(8)?.try_into().ok()?) != fingerprint
        {
            return None;
        }

        let kind = |raw: u8| PropertyKind::try_from(raw).ok();
        let optional = |raw: u8| match raw {
            NO_KIND => Some(None),
            raw => kind(raw).map(Some),
        };
        let mut schema = Self::default();
        for _ in 0..reader.u32()? {
            let (class, field) = (reader.u32()?, reader.u32()?);
            let [raw, key, item] = <[u8; 3]>::try_from(reader.bytes(3)?).ok()?;
            schema.shapes.insert(
                (class, field),
                Shape {
                    kind: kind(raw)?,
                    key: optional(key)?,
                    item: optional(item)?,
                },
            );
        }
        for _ in 0..reader.u32()? {
            schema.classes.insert(reader.u32()?);
        }
        reader.0.is_empty().then_some(schema)
    }
}

/// Reads little-endian values from the start of a byte slice.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    /// Returns the next `count` bytes. Returns `None` if fewer bytes remain.
    fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }
}

/// Counts the types of each property while bins are read.
#[derive(Default)]
struct Observer {
    /// The number of occurrences of each type of each property.
    shapes: HashMap<(u32, u32), HashMap<Key, u64>>,
    classes: HashSet<u32>,
}

impl Observer {
    /// Reads the bin chunks of `archive`. Logs a debug message for an archive or a bin that
    /// cannot be read.
    fn archive(&mut self, archive: &ArchiveBins) {
        let mounted = File::open(&archive.path)
            .into_diagnostic()
            .and_then(|file| Wad::mount(BufReader::new(file)).into_diagnostic());
        let mut wad = match mounted {
            Ok(wad) => wad,
            Err(error) => {
                tracing::debug!("Skipped the archive {}: {error}", archive.name);
                return;
            }
        };
        for chunk in &archive.chunks {
            let Some(entry) = wad.chunks().get(*chunk).copied() else {
                continue;
            };
            let decoded = wad
                .load_chunk_decompressed(&entry)
                .into_diagnostic()
                .and_then(|data| BinFile::from_reader(&mut Cursor::new(data)).into_diagnostic());
            match decoded {
                Ok(file) => self.file(&file),
                Err(error) => {
                    tracing::debug!("Skipped {:016x} of {}: {error}", chunk.0, archive.name);
                }
            }
        }
    }

    /// Records the objects of `file`.
    fn file(&mut self, file: &BinFile) {
        let objects = match file {
            BinFile::Prop(bin) => &bin.objects,
            BinFile::Override(patch) => &patch.objects,
        };
        for object in objects.values() {
            self.node(object.class_hash, &object.properties);
        }
    }

    /// Records the properties of one object or struct of `class`, and of the structs inside.
    fn node(&mut self, class: BinHash, properties: &IndexMap<BinHash, PropertyValueEnum>) {
        self.classes.insert(class.0);
        for (field, value) in properties {
            let shape = ValueShape::of(value);
            *self
                .shapes
                .entry((class.0, field.0))
                .or_default()
                .entry((shape.kind, shape.key_kind, shape.item_kind))
                .or_default() += 1;
            self.value(value);
        }
    }

    /// Records the structs inside `value`.
    fn value(&mut self, value: &PropertyValueEnum) {
        use PropertyValueEnum as V;
        match value {
            V::Container(items) => items.items().iter().for_each(|item| self.value(item)),
            V::UnorderedContainer(items) => {
                items.0.items().iter().for_each(|item| self.value(item));
            }
            V::Optional(option) => {
                if let Some(inner) = option.value() {
                    self.value(inner);
                }
            }
            V::Map(map) => map.entries().iter().for_each(|(_, item)| self.value(item)),
            // Class 0 is the null pointer. It has no properties.
            V::Struct(inner) if *inner.class_hash != 0 => {
                self.node(inner.class_hash, &inner.properties);
            }
            V::Embedded(inner) => self.node(inner.0.class_hash, &inner.0.properties),
            _ => {}
        }
    }

    fn merge(&mut self, other: Self) {
        for (key, shapes) in other.shapes {
            let into = self.shapes.entry(key).or_default();
            for (shape, count) in shapes {
                *into.entry(shape).or_default() += count;
            }
        }
        self.classes.extend(other.classes);
    }

    /// Returns the schema with the most frequent type of each property. Logs a debug message
    /// with the number of properties that have more than one type.
    fn into_schema(self) -> ObservedSchema {
        let conflicts = self
            .shapes
            .values()
            .filter(|shapes| shapes.len() > 1)
            .count();
        if conflicts > 0 {
            tracing::debug!(
                "{conflicts} properties have more than one type in the game. The most frequent type is used."
            );
        }
        let shapes = self
            .shapes
            .into_iter()
            .filter_map(|(key, shapes)| {
                // The kind breaks a tie, so that the result does not depend on map order.
                let ((kind, key_kind, item), _) = shapes
                    .into_iter()
                    .max_by_key(|((kind, ..), count)| (*count, u8::from(*kind)))?;
                Some((
                    key,
                    Shape {
                        kind,
                        key: key_kind,
                        item,
                    },
                ))
            })
            .collect();
        ObservedSchema {
            shapes,
            classes: self.classes,
        }
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use ltk_meta::{Bin, BinObject, property::values};

    use super::*;

    const CLASS: u32 = 0xaaaa_0001;
    const INNER: u32 = 0xaaaa_0002;

    fn file() -> BinFile {
        let mut inner = values::Struct {
            class_hash: BinHash(INNER),
            properties: IndexMap::new(),
        };
        inner
            .properties
            .insert(BinHash(0x20), values::F32::new(0.5).into());
        Bin::builder()
            .object(
                BinObject::builder(0x1u32, CLASS)
                    .property(0x10u32, values::U8::new(3))
                    .property(0x11u32, values::Embedded(inner))
                    .build(),
            )
            .build()
            .into()
    }

    #[test]
    fn of_records_properties_of_objects_and_of_nested_structs() {
        let schema = ObservedSchema::of(&file());
        assert_eq!((schema.classes(), schema.properties()), (2, 3));
        assert_eq!(
            schema.expected(BinHash(CLASS), BinHash(0x10)),
            Some(Shape::bare(PropertyKind::U8))
        );
        assert_eq!(
            schema.expected(BinHash(INNER), BinHash(0x20)),
            Some(Shape::bare(PropertyKind::F32))
        );
        assert_eq!(schema.expected(BinHash(CLASS), BinHash(0x20)), None);
        assert!(schema.has_class(BinHash(INNER)));
        assert!(!schema.has_class(BinHash(0x1)));
    }

    #[test]
    fn saved_schema_loads_only_for_same_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("cache").join("schema.bin")).unwrap();
        let schema = ObservedSchema::of(&file());

        schema.save(&path, 7).unwrap();
        assert_eq!(ObservedSchema::load(&path, 7), Some(schema));
        assert_eq!(ObservedSchema::load(&path, 8), None);

        // A truncated file and a file of another kind are not a schema.
        let data = std::fs::read(&path).unwrap();
        std::fs::write(&path, &data[..data.len() - 1]).unwrap();
        assert_eq!(ObservedSchema::load(&path, 7), None);
        std::fs::write(&path, b"not a schema").unwrap();
        assert_eq!(ObservedSchema::load(&path, 7), None);
        assert_eq!(
            ObservedSchema::load(&path.with_extension("missing"), 7),
            None
        );
    }
}
