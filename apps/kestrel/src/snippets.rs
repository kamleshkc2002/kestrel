//! Private, atomic storage for snippets.
//!
//! Literal `{{...}}` tokens are stored; resolved values stay within the process.

use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::Snippet;
use kestrel_services::snippets::{SnippetLibrary, SnippetPolicy};
use serde::{Deserialize, Serialize};

use crate::{ConfigurationWarning, private_file::write_private_atomic};

/// Snippet file schema version.
pub const CURRENT_SNIPPET_SCHEMA_VERSION: u32 = 1;

/// On-disk snippet file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetFile {
    pub schema_version: u32,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

impl Default for SnippetFile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SNIPPET_SCHEMA_VERSION,
            snippets: Vec::new(),
        }
    }
}

/// Loaded library and rejected-entry diagnostics.
#[derive(Debug, Clone, Default)]
pub struct LoadedSnippets {
    pub library: SnippetLibrary,
    pub warnings: Vec<ConfigurationWarning>,
}

/// Unrecoverable snippet-file failure.
#[derive(Debug)]
pub enum SnippetStoreError {
    /// Document parsing failed.
    InvalidDocument(String),
    /// File I/O failed.
    Io(String),
    /// Library validation rejected a write.
    InvalidLibrary(String),
}

impl std::fmt::Display for SnippetStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDocument(message) => {
                write!(formatter, "snippet file is invalid: {message}")
            }
            Self::Io(message) => write!(formatter, "snippet file error: {message}"),
            Self::InvalidLibrary(message) => {
                write!(formatter, "snippet library is invalid: {message}")
            }
        }
    }
}

impl std::error::Error for SnippetStoreError {}

/// `$XDG_DATA_HOME/kestrel` (`kestrel-devel` in debug builds), falling back to
/// `~/.local/share`.
pub fn kestrel_data_directory() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(data_home.join(kestrel_core::DATA_DIRECTORY_NAME))
}

/// XDG data path for snippets.
pub fn snippet_path() -> Option<PathBuf> {
    kestrel_data_directory().map(|directory| directory.join("snippets.toml"))
}

/// Loads snippets, isolating rejected entries; a missing file is empty.
pub fn load_snippets(path: &Path, policy: SnippetPolicy) -> LoadedSnippets {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedSnippets::default();
        }
        Err(error) => {
            return LoadedSnippets {
                library: SnippetLibrary::default(),
                warnings: vec![warning(
                    "snippets",
                    format!(
                        "The snippet file could not be read and was ignored: {error}. \
                         Snippets stay unavailable until the file is readable."
                    ),
                )],
            };
        }
    };
    parse(&contents, policy)
}

/// Parses a snippet document, keeping every entry that validates.
pub fn parse(contents: &str, policy: SnippetPolicy) -> LoadedSnippets {
    let document: SnippetFile = match toml::from_str(contents) {
        Ok(document) => document,
        Err(error) => {
            return LoadedSnippets {
                library: SnippetLibrary::default(),
                warnings: vec![warning(
                    "snippets",
                    format!("The snippet file is not valid TOML and was ignored: {error}"),
                )],
            };
        }
    };

    let mut warnings = Vec::new();
    if document.schema_version != CURRENT_SNIPPET_SCHEMA_VERSION {
        warnings.push(warning(
            "snippets.schema_version",
            format!(
                "Snippet schema {} is not supported; entries were still read individually.",
                document.schema_version
            ),
        ));
    }

    let mut library = SnippetLibrary::default();
    for (index, snippet) in document.snippets.into_iter().enumerate() {
        let location = format!("snippets[{index}]");
        match library.upsert(snippet, policy) {
            Ok(()) => {}
            Err(error) => {
                warnings.push(warning(&location, format!("Snippet was ignored: {error}")))
            }
        }
    }
    LoadedSnippets { library, warnings }
}

/// Validates and atomically writes the library with private permissions.
pub fn save_snippets(
    path: &Path,
    library: &SnippetLibrary,
    policy: SnippetPolicy,
) -> Result<(), SnippetStoreError> {
    library
        .validate(policy)
        .map_err(|error| SnippetStoreError::InvalidLibrary(error.to_string()))?;

    let document = SnippetFile {
        schema_version: CURRENT_SNIPPET_SCHEMA_VERSION,
        snippets: library.snippets().to_vec(),
    };
    let serialized = toml::to_string_pretty(&document).map_err(|error| {
        SnippetStoreError::InvalidDocument(format!("snippets could not be serialized: {error}"))
    })?;

    write_private_atomic(path, serialized.as_bytes())
        .map_err(|error| SnippetStoreError::Io(error.to_string()))
}

fn warning(location: &str, reason: impl Into<String>) -> ConfigurationWarning {
    ConfigurationWarning {
        feature_id: location.to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::private_file::FILE_MODE;
    use kestrel_core::Snippet;

    fn policy() -> SnippetPolicy {
        SnippetPolicy::default()
    }

    #[test]
    fn a_missing_file_loads_an_empty_library() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let loaded = load_snippets(&dir.path().join("absent.toml"), policy());

        assert!(loaded.library.is_empty());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn rejected_entries_are_isolated_with_their_location() {
        let loaded = parse(
            r#"
schema_version = 1
[[snippets]]
name = "Good"
content = "hello"

[[snippets]]
name = ""
content = "nameless"

[[snippets]]
name = "Duplicate trigger"
trigger = ";x"
content = "a"

[[snippets]]
name = "Also duplicate"
trigger = ";x"
content = "b"
"#,
            policy(),
        );

        assert_eq!(loaded.library.names(), vec!["Good", "Duplicate trigger"]);
        assert_eq!(loaded.warnings.len(), 2);
        assert_eq!(loaded.warnings[0].feature_id, "snippets[1]");
        assert!(loaded.warnings[0].reason.contains("needs a name"));
        assert_eq!(loaded.warnings[1].feature_id, "snippets[3]");
        assert!(loaded.warnings[1].reason.contains("already used"));
    }

    #[test]
    fn an_unparsable_document_is_reported_and_ignored() {
        let loaded = parse("schema_version = 1\n[[snippets]\n", policy());
        assert!(loaded.library.is_empty());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].reason.contains("not valid TOML"));
    }

    #[test]
    fn saving_is_atomic_private_and_round_trips() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("nested/kestrel/snippets.toml");
        let mut library = SnippetLibrary::default();
        library
            .upsert(
                Snippet::new(
                    "Address",
                    Some("Contact".to_string()),
                    Some(";addr".to_string()),
                    "Street 1\n{{date}}",
                ),
                policy(),
            )
            .expect("snippet is valid");

        save_snippets(&path, &library, policy()).expect("saving succeeds");

        let mode = fs::metadata(&path)
            .expect("file exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, FILE_MODE, "the snippet file stays private");
        assert_eq!(
            fs::read_dir(path.parent().expect("parent"))
                .expect("directory is readable")
                .count(),
            1,
            "no temporary file is left behind"
        );

        let reloaded = load_snippets(&path, policy());
        assert!(reloaded.warnings.is_empty());
        assert_eq!(reloaded.library, library);
        assert_eq!(
            reloaded
                .library
                .get("Address")
                .map(|snippet| snippet.content.as_str()),
            Some("Street 1\n{{date}}"),
            "variables are stored as literal tokens, never as resolved values"
        );
    }

    #[test]
    fn saving_an_invalid_library_is_refused() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("snippets.toml");
        let mut library = SnippetLibrary::default();
        library
            .upsert(
                Snippet::new("One", None, Some(";dup".to_string()), "a"),
                SnippetPolicy::default(),
            )
            .expect("first snippet is valid");
        // Bypass validation to test a conflicting trigger.
        let conflicting = SnippetLibrary::from_snippets(vec![
            Snippet::new("One", None, Some(";dup".to_string()), "a"),
            Snippet::new("Two", None, Some(";dup".to_string()), "b"),
        ]);

        assert!(matches!(
            save_snippets(&path, &conflicting, policy()),
            Err(SnippetStoreError::InvalidLibrary(_))
        ));
        assert!(!path.exists(), "nothing is written for an invalid library");
    }

    #[test]
    fn schema_version_mismatch_is_warned_not_fatal() {
        let loaded = parse(
            "schema_version = 99\n[[snippets]]\nname = \"A\"\ncontent = \"a\"\n",
            policy(),
        );

        assert_eq!(loaded.library.names(), vec!["A"]);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "snippets.schema_version")
        );
    }
}
