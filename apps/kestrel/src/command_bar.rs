//! Private storage for the command bar's learned ranking.
//!
//! The file holds command identifiers, use counts, and pins — never the text a
//! user searched for. It is created `0600` in a `0700` directory and replaced
//! atomically through an exclusively created temporary file, and the window can
//! inspect and reset it.

use std::{
    fs,
    path::{Path, PathBuf},
};

use kestrel_core::MAX_COMMAND_USAGE_ENTRIES;
use kestrel_services::command_bar::CommandRanking;
use serde::{Deserialize, Serialize};

use crate::{ConfigurationWarning, private_file::write_private_atomic};

pub const CURRENT_RANKING_SCHEMA_VERSION: u32 = 1;

/// The on-disk shape of the learned ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankingFile {
    pub schema_version: u32,
    #[serde(default)]
    pub pinned: Vec<String>,
    #[serde(default)]
    pub usage: Vec<RankingEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankingEntry {
    pub id: String,
    pub uses: u32,
}

impl Default for RankingFile {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_RANKING_SCHEMA_VERSION,
            pinned: Vec::new(),
            usage: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadedRanking {
    pub ranking: CommandRanking,
    pub warnings: Vec<ConfigurationWarning>,
}

/// `$XDG_DATA_HOME/kestrel/command_ranking.toml`, else `$HOME/.local/share/...`.
pub fn ranking_path() -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(data_home.join("kestrel").join("command_ranking.toml"))
}

/// Loads the learned ranking, ignoring a missing or unreadable file.
pub fn load_ranking(path: &Path) -> LoadedRanking {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedRanking::default();
        }
        Err(error) => {
            return LoadedRanking {
                ranking: CommandRanking::default(),
                warnings: vec![warning(
                    "command_bar.ranking",
                    format!(
                        "The learned command ranking could not be read and was ignored: {error}"
                    ),
                )],
            };
        }
    };
    parse_ranking(&contents)
}

/// Parses a ranking document, dropping malformed entries with a warning.
pub fn parse_ranking(contents: &str) -> LoadedRanking {
    let document: RankingFile = match toml::from_str(contents) {
        Ok(document) => document,
        Err(error) => {
            return LoadedRanking {
                ranking: CommandRanking::default(),
                warnings: vec![warning(
                    "command_bar.ranking",
                    format!(
                        "The learned command ranking is not valid TOML and was ignored: {error}"
                    ),
                )],
            };
        }
    };

    let mut warnings = Vec::new();
    if document.schema_version != CURRENT_RANKING_SCHEMA_VERSION {
        warnings.push(warning(
            "command_bar.ranking.schema_version",
            format!(
                "Ranking schema {} is not supported; entries were read individually.",
                document.schema_version
            ),
        ));
    }

    let pinned = document
        .pinned
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .take(MAX_COMMAND_USAGE_ENTRIES);
    let mut usage: Vec<(String, u32)> = Vec::new();
    for entry in document.usage.into_iter().take(MAX_COMMAND_USAGE_ENTRIES) {
        if entry.id.trim().is_empty() {
            warnings.push(warning(
                "command_bar.ranking.usage",
                "A ranking entry without an identifier was ignored.",
            ));
            continue;
        }
        usage.push((entry.id, entry.uses));
    }

    LoadedRanking {
        ranking: CommandRanking::from_parts(pinned, usage),
        warnings,
    }
}

/// Writes the ranking atomically with private permissions.
pub fn save_ranking(path: &Path, ranking: &CommandRanking) -> Result<(), String> {
    let document = RankingFile {
        schema_version: CURRENT_RANKING_SCHEMA_VERSION,
        pinned: ranking.pinned_entries(),
        usage: ranking
            .entries()
            .into_iter()
            .map(|(id, uses)| RankingEntry { id, uses })
            .collect(),
    };
    let serialized = toml::to_string_pretty(&document)
        .map_err(|error| format!("the ranking could not be serialized: {error}"))?;
    write_private_atomic(path, serialized.as_bytes()).map_err(|error| error.to_string())
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

    #[test]
    fn a_missing_ranking_file_loads_empty() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let loaded = load_ranking(&dir.path().join("absent.toml"));

        assert_eq!(loaded.ranking, CommandRanking::default());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn saving_is_private_atomic_and_round_trips() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("nested/kestrel/command_ranking.toml");
        let mut ranking = CommandRanking::default();
        ranking.record_use("kestrel:refresh");
        ranking.record_use("kestrel:refresh");
        ranking.set_pinned("snippet:Address", true);

        save_ranking(&path, &ranking).expect("saving succeeds");

        let mode = fs::metadata(&path)
            .expect("file exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, FILE_MODE, "the ranking file stays private");

        let reloaded = load_ranking(&path);
        assert!(reloaded.warnings.is_empty());
        assert_eq!(reloaded.ranking.usage("kestrel:refresh"), 2);
        assert!(reloaded.ranking.is_pinned("snippet:Address"));
    }

    #[test]
    fn the_ranking_file_never_contains_query_text() {
        let mut ranking = CommandRanking::default();
        let typed = "quarterly revenue numbers";
        // Searching is what the user types, and it is never recorded.
        let _ = kestrel_services::command_bar::fuzzy_score(typed, "kestrel:refresh");
        ranking.record_use("kestrel:refresh");

        let serialized = toml::to_string_pretty(&RankingFile {
            schema_version: CURRENT_RANKING_SCHEMA_VERSION,
            pinned: ranking.pinned_entries(),
            usage: ranking
                .entries()
                .into_iter()
                .map(|(id, uses)| RankingEntry { id, uses })
                .collect(),
        })
        .expect("the ranking serializes");

        for word in typed.split_whitespace() {
            assert!(
                !serialized.contains(word),
                "no word of a query reaches disk: {serialized}"
            );
        }
        assert!(serialized.contains("kestrel:refresh"));
    }

    #[test]
    fn malformed_ranking_entries_are_reported_and_dropped() {
        let loaded = parse_ranking(
            r#"
schema_version = 1
pinned = ["", "kestrel:refresh"]
[[usage]]
id = ""
uses = 3
[[usage]]
id = "snippet:Address"
uses = 4
"#,
        );

        assert_eq!(
            loaded.ranking.pinned_entries(),
            vec!["kestrel:refresh".to_string()]
        );
        assert_eq!(loaded.ranking.usage("snippet:Address"), 4);
        assert_eq!(loaded.ranking.usage(""), 0);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "command_bar.ranking.usage")
        );
    }

    #[test]
    fn an_unparsable_ranking_is_reported_and_ignored() {
        let loaded = parse_ranking("schema_version = 1\n[[usage]\n");
        assert_eq!(loaded.ranking, CommandRanking::default());
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].reason.contains("not valid TOML"));
    }

    #[test]
    fn schema_version_mismatch_is_warned_not_fatal() {
        let loaded = parse_ranking("schema_version = 9\npinned = [\"a\"]\n");
        assert_eq!(loaded.ranking.pinned_entries(), vec!["a".to_string()]);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|warning| warning.feature_id == "command_bar.ranking.schema_version")
        );
    }
}
