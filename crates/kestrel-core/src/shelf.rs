//! Bounded shelf entries: files, text, links, and images.

use std::{
    fmt,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

use crate::ConfigurationError;

/// Default number of entries the shelf keeps.
pub const DEFAULT_SHELF_MAX_ITEMS: u32 = 50;
pub const MIN_SHELF_MAX_ITEMS: u32 = 1;
pub const MAX_SHELF_MAX_ITEMS: u32 = 200;
/// Default largest text or link, in megabytes.
pub const DEFAULT_SHELF_TEXT_MEGABYTES: u32 = 1;
pub const MIN_SHELF_TEXT_MEGABYTES: u32 = 1;
pub const MAX_SHELF_TEXT_MEGABYTES: u32 = 4;
/// Default largest image, in megabytes.
pub const DEFAULT_SHELF_IMAGE_MEGABYTES: u32 = 16;
pub const MIN_SHELF_IMAGE_MEGABYTES: u32 = 1;
pub const MAX_SHELF_IMAGE_MEGABYTES: u32 = 64;
/// Oldest an unpinned entry may become before expiry can remove it.
pub const MAX_SHELF_AGE_HOURS: u32 = 720;

const BYTES_PER_MEGABYTE: u32 = 1024 * 1024;
/// Display names never grow past this many characters.
const MAX_DISPLAY_NAME_CHARS: usize = 80;

/// Portable settings for the shelf and how it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfConfiguration {
    #[serde(default)]
    pub keep_across_restarts: bool,
    /// Every new screenshot is added to the shelf as it is taken.
    #[serde(default)]
    pub add_screenshots: bool,
    #[serde(default = "default_shelf_max_items")]
    pub max_items: u32,
    #[serde(default = "default_shelf_text_megabytes")]
    pub max_text_megabytes: u32,
    #[serde(default = "default_shelf_image_megabytes")]
    pub max_image_megabytes: u32,
    #[serde(default)]
    pub max_age_hours: u32,
}

fn default_shelf_max_items() -> u32 {
    DEFAULT_SHELF_MAX_ITEMS
}
fn default_shelf_text_megabytes() -> u32 {
    DEFAULT_SHELF_TEXT_MEGABYTES
}
fn default_shelf_image_megabytes() -> u32 {
    DEFAULT_SHELF_IMAGE_MEGABYTES
}

impl Default for ShelfConfiguration {
    fn default() -> Self {
        Self {
            keep_across_restarts: false,
            add_screenshots: false,
            max_items: DEFAULT_SHELF_MAX_ITEMS,
            max_text_megabytes: DEFAULT_SHELF_TEXT_MEGABYTES,
            max_image_megabytes: DEFAULT_SHELF_IMAGE_MEGABYTES,
            max_age_hours: 0,
        }
    }
}

impl ShelfConfiguration {
    pub fn limits(&self) -> ShelfLimits {
        ShelfLimits {
            max_items: self.max_items,
            max_text_bytes: self.max_text_megabytes.saturating_mul(BYTES_PER_MEGABYTE),
            max_image_bytes: self.max_image_megabytes.saturating_mul(BYTES_PER_MEGABYTE),
            max_age_hours: self.max_age_hours,
        }
    }

    pub fn validate(&self) -> Result<(), ConfigurationError> {
        self.limits().validate()
    }
}

/// Enforced bounds for the shelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShelfLimits {
    pub max_items: u32,
    pub max_text_bytes: u32,
    pub max_image_bytes: u32,
    /// 0 keeps entries forever.
    pub max_age_hours: u32,
}

impl Default for ShelfLimits {
    fn default() -> Self {
        ShelfConfiguration::default().limits()
    }
}

impl ShelfLimits {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        if !(MIN_SHELF_MAX_ITEMS..=MAX_SHELF_MAX_ITEMS).contains(&self.max_items) {
            return Err(ConfigurationError::InvalidShelfMaxItems {
                items: self.max_items,
            });
        }
        if self.max_text_bytes == 0
            || self.max_text_bytes > MAX_SHELF_TEXT_MEGABYTES.saturating_mul(BYTES_PER_MEGABYTE)
        {
            return Err(ConfigurationError::InvalidShelfTextBytes {
                bytes: self.max_text_bytes,
            });
        }
        if self.max_image_bytes == 0
            || self.max_image_bytes > MAX_SHELF_IMAGE_MEGABYTES.saturating_mul(BYTES_PER_MEGABYTE)
        {
            return Err(ConfigurationError::InvalidShelfImageBytes {
                bytes: self.max_image_bytes,
            });
        }
        if self.max_age_hours > MAX_SHELF_AGE_HOURS {
            return Err(ConfigurationError::InvalidShelfAgeHours {
                hours: self.max_age_hours,
            });
        }
        Ok(())
    }
}

/// What one shelf entry holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShelfContent {
    /// A reference to a file: its path and display name, never its bytes.
    File {
        path: PathBuf,
        name: String,
    },
    Text(String),
    /// A single-line http(s), mailto, or file address.
    Link {
        url: String,
    },
    /// PNG data held only in memory.
    Image {
        png: Vec<u8>,
        width: u32,
        height: u32,
    },
}

impl ShelfContent {
    pub fn kind(&self) -> ShelfItemKind {
        match self {
            Self::File { .. } => ShelfItemKind::File,
            Self::Text(_) => ShelfItemKind::Text,
            Self::Link { .. } => ShelfItemKind::Link,
            Self::Image { .. } => ShelfItemKind::Image,
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::File { path, name } => {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    truncate_chars(&path.display().to_string(), MAX_DISPLAY_NAME_CHARS)
                } else {
                    truncate_chars(trimmed, MAX_DISPLAY_NAME_CHARS)
                }
            }
            Self::Text(text) => {
                let first = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('\r')
                    .trim();
                if first.is_empty() {
                    "Text".to_owned()
                } else {
                    truncate_chars(first, MAX_DISPLAY_NAME_CHARS)
                }
            }
            Self::Link { url } => truncate_chars(&link_label(url), MAX_DISPLAY_NAME_CHARS),
            Self::Image { width, height, .. } => format!("Image {width}×{height}"),
        }
    }

    /// Bytes the content itself occupies, where that is known without I/O.
    pub fn size_bytes(&self) -> Option<u64> {
        match self {
            Self::File { .. } => None,
            Self::Text(text) => Some(text.len() as u64),
            Self::Link { url } => Some(url.len() as u64),
            Self::Image { png, .. } => Some(png.len() as u64),
        }
    }

    /// The exact value the entry offers when copied or dragged out.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Link { url } => Some(url),
            Self::File { .. } | Self::Image { .. } => None,
        }
    }
}

/// The four supported kinds, for presentation and persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShelfItemKind {
    File,
    Text,
    Link,
    Image,
}

impl ShelfItemKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Text => "Text",
            Self::Link => "Link",
            Self::Image => "Image",
        }
    }
}

/// One staged entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfItem {
    pub id: u64,
    pub added: SystemTime,
    /// Pinned entries survive count eviction, age expiry, and unpinned clears.
    pub pinned: bool,
    pub content: ShelfContent,
}

/// Why an entry could not be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShelfError {
    EmptyContent,
    InvalidLimits,
    OversizeText {
        bytes: usize,
        limit: u32,
    },
    OversizeImage {
        bytes: usize,
        limit: u32,
    },
    /// Every slot holds a pinned entry.
    NoRoom,
    UnknownItem,
}

impl fmt::Display for ShelfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyContent => formatter.write_str("There is nothing to add to the shelf."),
            Self::InvalidLimits => formatter.write_str("The shelf limits are not valid."),
            Self::OversizeText { bytes, limit } => write!(
                formatter,
                "Text is {} and the shelf accepts up to {}; raise the text limit in the shelf settings.",
                format_size(*bytes as u64),
                format_size(u64::from(*limit))
            ),
            Self::OversizeImage { bytes, limit } => write!(
                formatter,
                "The image is {} and the shelf accepts up to {}; raise the image limit in the shelf settings.",
                format_size(*bytes as u64),
                format_size(u64::from(*limit))
            ),
            Self::NoRoom => formatter
                .write_str("Every shelf slot holds a pinned entry; unpin one or raise the limit."),
            Self::UnknownItem => formatter.write_str("That shelf entry is no longer there."),
        }
    }
}

impl std::error::Error for ShelfError {}

/// The result of placing one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShelfAddOutcome {
    pub id: u64,
    /// An equal entry was already present and moved to the top.
    pub existing: bool,
}

/// Classifies dropped text as a link or plain text.
///
/// A link is one line with no surrounding spaces whose scheme is http, https,
/// mailto, or file. Everything else stays text.
pub fn detect_shelf_content(text: &str) -> ShelfContent {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ShelfContent::Text(text.to_owned());
    }
    if trimmed.contains(['\n', '\r']) {
        return ShelfContent::Text(text.to_owned());
    }
    if is_shelf_link(trimmed) {
        return ShelfContent::Link {
            url: trimmed.to_owned(),
        };
    }
    ShelfContent::Text(text.to_owned())
}

fn is_shelf_link(candidate: &str) -> bool {
    if candidate.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((scheme, rest)) = candidate.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" | "file" => {
            rest.len() > 2 && rest.starts_with("//") && rest[2..].chars().any(|c| c != '/')
        }
        "mailto" => rest.contains('@') && !rest.ends_with('@'),
        _ => false,
    }
}

/// Rejects content outside the configured bounds.
pub fn shelf_content_fits(content: &ShelfContent, limits: &ShelfLimits) -> Result<(), ShelfError> {
    match content {
        ShelfContent::Text(text) => {
            let bytes = text.len();
            if bytes == 0 || text.trim().is_empty() {
                Err(ShelfError::EmptyContent)
            } else if bytes > limits.max_text_bytes as usize {
                Err(ShelfError::OversizeText {
                    bytes,
                    limit: limits.max_text_bytes,
                })
            } else {
                Ok(())
            }
        }
        ShelfContent::Link { url } => {
            if url.len() > limits.max_text_bytes as usize {
                Err(ShelfError::OversizeText {
                    bytes: url.len(),
                    limit: limits.max_text_bytes,
                })
            } else {
                Ok(())
            }
        }
        ShelfContent::Image { png, .. } => {
            if png.len() > limits.max_image_bytes as usize {
                Err(ShelfError::OversizeImage {
                    bytes: png.len(),
                    limit: limits.max_image_bytes,
                })
            } else {
                Ok(())
            }
        }
        ShelfContent::File { path, .. } => {
            if path.as_os_str().is_empty() {
                Err(ShelfError::EmptyContent)
            } else {
                Ok(())
            }
        }
    }
}

/// The index of an entry holding the same file path or the same exact text.
pub fn shelf_find_duplicate(items: &[ShelfItem], content: &ShelfContent) -> Option<usize> {
    items
        .iter()
        .position(|item| match (&item.content, content) {
            (ShelfContent::File { path: existing, .. }, ShelfContent::File { path, .. }) => {
                existing == path
            }
            (ShelfContent::Text(existing), ShelfContent::Text(text)) => existing == text,
            (ShelfContent::Link { url: existing }, ShelfContent::Link { url }) => existing == url,
            _ => false,
        })
}

/// The oldest unpinned entry, which a full shelf evicts first.
pub fn shelf_evict_index(items: &[ShelfItem]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| !item.pinned)
        .min_by_key(|(_, item)| item.added)
        .map(|(index, _)| index)
}

/// Places an entry at the top, refreshing an equal entry instead of copying it.
pub fn shelf_place(
    items: &mut Vec<ShelfItem>,
    id: u64,
    content: ShelfContent,
    now: SystemTime,
    limits: &ShelfLimits,
) -> Result<ShelfAddOutcome, ShelfError> {
    shelf_content_fits(&content, limits)?;
    if let Some(index) = shelf_find_duplicate(items, &content) {
        let mut existing = items.remove(index);
        existing.added = now;
        let existing_id = existing.id;
        items.insert(0, existing);
        return Ok(ShelfAddOutcome {
            id: existing_id,
            existing: true,
        });
    }
    if items.len() >= limits.max_items as usize {
        let Some(index) = shelf_evict_index(items) else {
            return Err(ShelfError::NoRoom);
        };
        items.remove(index);
    }
    items.insert(
        0,
        ShelfItem {
            id,
            added: now,
            pinned: false,
            content,
        },
    );
    Ok(ShelfAddOutcome {
        id,
        existing: false,
    })
}

/// Removes unpinned entries older than the age limit and returns their IDs.
pub fn shelf_expire(items: &mut Vec<ShelfItem>, limits: &ShelfLimits, now: SystemTime) -> Vec<u64> {
    if limits.max_age_hours == 0 {
        return Vec::new();
    }
    let age = Duration::from_secs(u64::from(limits.max_age_hours) * 3_600);
    let mut expired = Vec::new();
    items.retain(|item| {
        let stale = !item.pinned
            && now
                .duration_since(item.added)
                .is_ok_and(|elapsed| elapsed > age);
        if stale {
            expired.push(item.id);
        }
        !stale
    });
    expired
}

/// The link's address without its scheme, trimmed to the display bound.
fn link_label(url: &str) -> String {
    if let Some(rest) = strip_scheme(url, "file") {
        return rest.to_owned();
    }
    if let Some(rest) = strip_scheme(url, "mailto") {
        return rest.to_owned();
    }
    strip_scheme(url, "http")
        .or_else(|| strip_scheme(url, "https"))
        .unwrap_or(url)
        .to_owned()
}

fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let rest = url.get(..scheme.len())?;
    if !rest.eq_ignore_ascii_case(scheme) {
        return None;
    }
    let rest = url.get(scheme.len()..)?;
    if scheme.eq_ignore_ascii_case("file") {
        return rest.strip_prefix("://");
    }
    if scheme.eq_ignore_ascii_case("mailto") {
        return rest.strip_prefix(':');
    }
    rest.strip_prefix("://")
}

fn truncate_chars(text: &str, maximum: usize) -> String {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index >= maximum {
            break;
        }
        out.push(character);
    }
    out
}

/// A short, plain byte size for messages and subtitles.
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{
        MAX_SHELF_AGE_HOURS, ShelfConfiguration, ShelfContent, ShelfError, ShelfItem, ShelfLimits,
        detect_shelf_content, shelf_evict_index, shelf_expire, shelf_place,
    };

    fn limits() -> ShelfLimits {
        ShelfConfiguration {
            max_items: 3,
            max_age_hours: 0,
            ..ShelfConfiguration::default()
        }
        .limits()
    }

    fn at(seconds: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn item(id: u64, added: u64, pinned: bool) -> ShelfItem {
        ShelfItem {
            id,
            added: at(added),
            pinned,
            content: ShelfContent::Text(format!("entry {id}")),
        }
    }

    #[test]
    fn limits_reject_every_bound_alone() {
        assert!(ShelfLimits::default().validate().is_ok());
        assert!(ShelfConfiguration::default().limits().validate().is_ok());
        assert!(
            ShelfConfiguration {
                max_items: 0,
                ..ShelfConfiguration::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ShelfConfiguration {
                max_text_megabytes: 5,
                ..ShelfConfiguration::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ShelfConfiguration {
                max_image_megabytes: 65,
                ..ShelfConfiguration::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ShelfConfiguration {
                max_age_hours: MAX_SHELF_AGE_HOURS + 1,
                ..ShelfConfiguration::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_full_shelf_drops_the_oldest_unpinned_entry_and_keeps_pinned_ones() {
        let mut items = vec![item(3, 300, false), item(2, 200, true), item(1, 100, false)];
        assert_eq!(shelf_evict_index(&items), Some(2), "oldest unpinned");
        let outcome = shelf_place(
            &mut items,
            4,
            ShelfContent::Text("new".to_owned()),
            at(400),
            &limits(),
        )
        .expect("there is room after eviction");
        assert!(!outcome.existing);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, 4);
        assert!(items.iter().any(|item| item.id == 2 && item.pinned));

        let mut pinned = vec![item(1, 100, true), item(2, 200, true), item(3, 300, true)];
        assert_eq!(
            shelf_place(
                &mut pinned,
                4,
                ShelfContent::Text("new".to_owned()),
                at(400),
                &limits()
            ),
            Err(ShelfError::NoRoom)
        );
    }

    #[test]
    fn duplicates_move_to_the_top_instead_of_copying() {
        let mut items = vec![
            item(1, 100, true),
            ShelfItem {
                id: 2,
                added: at(200),
                pinned: true,
                content: ShelfContent::File {
                    path: "/tmp/one".into(),
                    name: "one".to_owned(),
                },
            },
            item(3, 300, false),
        ];
        let outcome = shelf_place(
            &mut items,
            7,
            ShelfContent::File {
                path: "/tmp/one".into(),
                name: "renamed".to_owned(),
            },
            at(400),
            &limits(),
        )
        .expect("an equal file is refreshed");
        assert_eq!(
            outcome,
            super::ShelfAddOutcome {
                id: 2,
                existing: true
            }
        );
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, 2);
        assert_eq!(items[0].added, at(400));
        assert!(items[0].pinned, "the existing pin is kept");

        let text = ShelfContent::Text("same".to_owned());
        let mut texts = vec![ShelfItem {
            id: 5,
            added: at(100),
            pinned: false,
            content: text.clone(),
        }];
        assert!(
            shelf_place(&mut texts, 6, text.clone(), at(200), &limits())
                .expect("equal text is refreshed")
                .existing
        );
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].id, 5);
    }

    #[test]
    fn oversized_content_is_rejected_with_its_limit() {
        let limits = ShelfLimits {
            max_text_bytes: 4,
            max_image_bytes: 4,
            ..ShelfLimits::default()
        };
        assert_eq!(
            shelf_place(
                &mut Vec::new(),
                1,
                ShelfContent::Text("too long".to_owned()),
                at(1),
                &limits
            ),
            Err(ShelfError::OversizeText { bytes: 8, limit: 4 })
        );
        assert_eq!(
            shelf_place(
                &mut Vec::new(),
                1,
                ShelfContent::Image {
                    png: vec![0; 5],
                    width: 1,
                    height: 1
                },
                at(1),
                &limits
            ),
            Err(ShelfError::OversizeImage { bytes: 5, limit: 4 })
        );
        assert_eq!(
            shelf_place(
                &mut Vec::new(),
                1,
                ShelfContent::Text("   ".to_owned()),
                at(1),
                &limits
            ),
            Err(ShelfError::EmptyContent)
        );
    }

    #[test]
    fn only_single_line_web_mail_and_file_addresses_become_links() {
        for link in [
            "https://example.com/a?b=c",
            "http://example.com",
            "mailto:someone@example.com",
            "file:///tmp/report.txt",
        ] {
            assert_eq!(
                detect_shelf_content(link),
                ShelfContent::Link {
                    url: link.to_owned()
                },
                "{link}"
            );
        }
        for text in [
            "https://example.com\nsecond line",
            "ftp://example.com/file",
            "see https://example.com",
            "example.com",
            "mailto:",
            "just some text",
        ] {
            assert!(
                matches!(detect_shelf_content(text), ShelfContent::Text(_)),
                "{text}"
            );
        }
        assert!(
            matches!(
                detect_shelf_content("file:///tmp/one.txt\nhttps://example.com/page"),
                ShelfContent::Text(_)
            ),
            "a whole URI list is not one link"
        );
        assert_eq!(
            detect_shelf_content("  https://example.com  "),
            ShelfContent::Link {
                url: "https://example.com".to_owned()
            }
        );
    }

    #[test]
    fn expiry_skips_pinned_entries_and_uses_the_injected_time() {
        let limits = ShelfLimits {
            max_age_hours: 1,
            ..limits()
        };
        let mut items = vec![item(1, 0, false), item(2, 0, true), item(3, 7_000, false)];
        assert_eq!(shelf_expire(&mut items, &limits, at(10_000)), vec![1]);
        assert_eq!(
            items.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn display_names_stay_short_and_describe_each_kind() {
        let long = "a".repeat(300);
        assert_eq!(
            ShelfContent::Text(format!("first line\n{long}")).display_name(),
            "first line"
        );
        assert_eq!(ShelfContent::Text(long.clone()).display_name().len(), 80);
        assert_eq!(
            ShelfContent::Link {
                url: "https://example.com/deep/path".to_owned()
            }
            .display_name(),
            "example.com/deep/path"
        );
        assert_eq!(
            ShelfContent::File {
                path: "/tmp/a".into(),
                name: "a.txt".to_owned()
            }
            .display_name(),
            "a.txt"
        );
        assert_eq!(
            ShelfContent::Image {
                png: Vec::new(),
                width: 4,
                height: 3
            }
            .display_name(),
            "Image 4×3"
        );
    }
}
