//! Loading of the configuration file.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

/// Information about one archive.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Archive<'raw> {
	/// The requested compression level.
	pub compression: Cow<'raw, str>,

	/// The repository URL.
	pub repository: Cow<'raw, str>,

	/// The path to the root directory of the files to add to the archive.
	pub root: Cow<'raw, Path>,

	/// Whether to treat `root` as a Btrfs subvolume and actually create the archive from a
	/// snapshot thereof.
	pub btrfs_snapshot: bool,

	/// The list of pattern strings.
	pub patterns: Vec<Cow<'raw, str>>,
}

/// The complete configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config<'raw> {
	/// The requested archives.
	pub archives: BTreeMap<Cow<'raw, str>, Archive<'raw>>,

	/// The umask.
	pub umask: u16,
}

impl<'de> Deserialize<'de> for Config<'de> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		ParsedConfig::deserialize(deserializer)?.finish::<D>()
	}
}

/// The intermediate JSON-parsed form of the defaults section.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ParsedDefaults<'raw> {
	/// The requested compression level.
	#[serde(borrow, default)]
	compression: Option<Cow<'raw, str>>,

	/// The repository URL.
	#[serde(borrow, default)]
	repository: Option<Cow<'raw, str>>,
}

/// The intermediate JSON-parsed form of an archive.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParsedArchive<'raw> {
	/// The requested compression level.
	#[serde(borrow, default)]
	compression: Option<Cow<'raw, str>>,

	/// The repository URL.
	#[serde(borrow, default)]
	repository: Option<Cow<'raw, str>>,

	/// The path to the root directory of the files to add to the archive.
	#[serde(borrow)]
	root: Cow<'raw, Path>,

	/// Whether to treat `root` as a Btrfs subvolume and actually create the archive from a
	/// snapshot thereof.
	#[serde(default)]
	btrfs_snapshot: bool,

	/// The list of pattern strings.
	#[serde(borrow, default)]
	patterns: Vec<Cow<'raw, str>>,
}

impl<'raw> ParsedArchive<'raw> {
	/// Converts a `ParsedArchive` into an [`Archive`].
	fn finish<D: Deserializer<'raw>>(
		self,
		defaults: &ParsedDefaults<'raw>,
	) -> Result<Archive<'raw>, D::Error> {
		for pattern in &self.patterns {
			match pattern.chars().next() {
				Some('+') | Some('-') | Some('!') | Some('P') => (),
				_ => {
					return Err(D::Error::invalid_value(
						serde::de::Unexpected::Str(pattern),
						&"Borg pattern specification starting with +, -, !, or P",
					))
				}
			}
		}
		let compression = self
			.compression
			.or_else(|| defaults.compression.clone())
			.ok_or_else(|| D::Error::missing_field("compression"))?;
		let repository = self
			.repository
			.or_else(|| defaults.repository.clone())
			.ok_or_else(|| D::Error::missing_field("repository"))?;
		Ok(Archive {
			compression,
			repository,
			root: self.root,
			btrfs_snapshot: self.btrfs_snapshot,
			patterns: self.patterns,
		})
	}
}

/// Returns the default umask, used if one is not written in the config file.
const fn default_umask() -> u16 {
	0o0077
}

/// Decodes a umask from a three- or four-digit octal string.
fn deserialize_umask<'de, D: Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
	use serde::de::{Unexpected, Visitor};
	use std::fmt::Formatter;
	struct Vis;
	impl Visitor<'_> for Vis {
		type Value = u16;

		fn expecting(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
			write!(f, "an octal umask ≤777")
		}

		fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<u16, E> {
			let parsed = u16::from_str_radix(value, 8)
				.map_err(|_| E::invalid_value(Unexpected::Str(value), &self))?;
			if parsed <= 0o777 {
				Ok(parsed)
			} else {
				Err(E::invalid_value(Unexpected::Str(value), &self))
			}
		}
	}
	d.deserialize_str(Vis)
}

/// The intermediate JSON-parsed form of the config file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParsedConfig<'raw> {
	/// The defaults section.
	#[serde(borrow, default)]
	defaults: ParsedDefaults<'raw>,

	/// The archives section.
	#[serde(borrow)]
	archives: BTreeMap<Cow<'raw, str>, ParsedArchive<'raw>>,

	/// The umask option.
	#[serde(default = "default_umask", deserialize_with = "deserialize_umask")]
	umask: u16,
}

impl<'raw> ParsedConfig<'raw> {
	/// Converts a `ParsedConfig` into a [`Config`].
	fn finish<D: Deserializer<'raw>>(self) -> Result<Config<'raw>, D::Error> {
		Ok(Config {
			archives: self
				.archives
				.into_iter()
				.map(|(name, archive)| {
					Ok((name, ParsedArchive::finish::<D>(archive, &self.defaults)?))
				})
				.collect::<Result<BTreeMap<Cow<'raw, str>, Archive<'raw>>, D::Error>>()?,
			umask: self.umask,
		})
	}
}

/// Tests deserializing a basic config file with no archives.
#[test]
fn test_deserialize_empty() {
	const INPUT: &[u8] = br#"{"archives": {}}"#;
	assert_eq!(
		serde_json::from_slice::<Config>(INPUT).unwrap(),
		Config {
			archives: BTreeMap::new(),
		}
	);
}

/// Tests deserializing a config file with two complete archive specifications.
#[test]
fn test_deserialize_two_archives() {
	const INPUT: &[u8] = br#"
		{
			"archives": {
				"foo": {
					"compression": "lzma",
					"repository": "/path/to/foo/repo",
					"root": "/path/to/foo/archive/root",
					"btrfs_snapshot": false
				},
				"bar": {
					"compression": "lzma",
					"repository": "/path/to/bar/repo",
					"root": "/path/to/bar/archive/root",
					"btrfs_snapshot": true,
					"patterns": [
						"+pattern1"
					]
				}
			}
		}"#;
	assert_eq!(
		serde_json::from_slice::<Config>(INPUT).unwrap(),
		Config {
			archives: [
				(
					Cow::Borrowed("foo"),
					Archive {
						compression: Cow::Borrowed("lzma"),
						repository: Cow::Borrowed(Path::new("/path/to/foo/repo")),
						root: Cow::Borrowed(Path::new("/path/to/foo/archive/root")),
						btrfs_snapshot: false,
						patterns: Vec::new(),
					}
				),
				(
					Cow::Borrowed("bar"),
					Archive {
						compression: Cow::Borrowed("lzma"),
						repository: Cow::Borrowed(Path::new("/path/to/bar/repo")),
						root: Cow::Borrowed(Path::new("/path/to/bar/archive/root")),
						btrfs_snapshot: true,
						patterns: vec![Cow::Borrowed("+pattern1")],
					}
				),
			]
			.into_iter()
			.collect(),
		}
	);
}

/// Tests deserializing a config file with a complete archive specification, an incomplete archive
/// specification, and a defaults section that completes the incomplete specification.
#[test]
fn test_deserialize_partial_and_complete() {
	const INPUT: &[u8] = br#"
		{
			"defaults": {
				"compression": "lz4",
				"repository": "/path/to/default/repo"
			},
			"archives": {
				"foo": {
					"root": "/path/to/foo/archive/root",
					"btrfs_snapshot": false
				},
				"bar": {
					"compression": "lzma",
					"repository": "/path/to/bar/repo",
					"root": "/path/to/bar/archive/root",
					"btrfs_snapshot": true,
					"patterns": [
						"+pattern1"
					]
				}
			}
		}"#;
	assert_eq!(
		serde_json::from_slice::<Config>(INPUT).unwrap(),
		Config {
			archives: [
				(
					Cow::Borrowed("foo"),
					Archive {
						compression: Cow::Borrowed("lz4"),
						repository: Cow::Borrowed(Path::new("/path/to/default/repo")),
						root: Cow::Borrowed(Path::new("/path/to/foo/archive/root")),
						btrfs_snapshot: false,
						patterns: Vec::new(),
					}
				),
				(
					Cow::Borrowed("bar"),
					Archive {
						compression: Cow::Borrowed("lzma"),
						repository: Cow::Borrowed(Path::new("/path/to/bar/repo")),
						root: Cow::Borrowed(Path::new("/path/to/bar/archive/root")),
						btrfs_snapshot: true,
						patterns: vec![Cow::Borrowed("+pattern1")],
					}
				),
			]
			.into_iter()
			.collect(),
		}
	);
}

/// Tests deserializing a partial archive where the missing information is not provided in the
/// defaults section.
///
/// This should fail because information is completely missing.
#[test]
fn test_deserialize_missing_info() {
	const INPUT: &[u8] = br#"
		{
			"defaults": {
				"compression": "lz4",
			},
			"archives": {
				"foo": {
					"root": "/path/to/foo/archive/root",
					"btrfs_snapshot": false
				}
			}
		}"#;
	assert!(serde_json::from_slice::<Config>(INPUT).is_err());
}

/// Tests deserializing an archive with an illegal pattern entry.
#[test]
fn test_deserialize_bad_pattern() {
	const INPUT: &[u8] = br#"
		{
			"archives": {
				"foo": {
					"compression": "lzma",
					"repository": "/path/to/foo/repo",
					"root": "/path/to/foo/archive/root",
					"btrfs_snapshot": false,
					"patterns": [
						"X mypattern"
					]
				}
			}
		}"#;
	assert!(serde_json::from_slice::<Config>(INPUT).is_err());
}
