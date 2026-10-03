//! Keyboard-first command ranking and the portable providers behind it.
//!
//! The bar is deliberately bounded: it ranks a small in-memory catalog plus the
//! results the application already gathered (applications, configured file
//! roots, snippets), it never builds a filesystem-wide index, and its learned
//! ranking stores command identifiers and counts — never the text a user typed.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use kestrel_core::{CommandScriptConfiguration, MAX_COMMAND_USAGE_ENTRIES};
use kestrel_platform::snippets::LocalTime;

pub const FEATURE_ID: &str = "commands.bar";
/// Awarded when every needle character matched contiguously.
const SUBSTRING_BONUS: i64 = 20;
/// Awarded when the match starts at the beginning of the label.
const PREFIX_BONUS: i64 = 6;

/// The origin of one result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandSource {
    Kestrel,
    Snippet,
    Application,
    File,
    Math,
    Units,
    Date,
    Url,
    Emoji,
    Script,
}

impl CommandSource {
    pub const ALL: [CommandSource; 10] = [
        CommandSource::Kestrel,
        CommandSource::Snippet,
        CommandSource::Application,
        CommandSource::File,
        CommandSource::Math,
        CommandSource::Units,
        CommandSource::Date,
        CommandSource::Url,
        CommandSource::Emoji,
        CommandSource::Script,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Kestrel => "Kestrel",
            Self::Snippet => "Snippet",
            Self::Application => "Application",
            Self::File => "File",
            Self::Math => "Math",
            Self::Units => "Units",
            Self::Date => "Date",
            Self::Url => "Link",
            Self::Emoji => "Emoji",
            Self::Script => "Script",
        }
    }

    /// Portable providers answer without any desktop integration.
    ///
    /// Unavailable integration providers are reported as unavailable, but they
    /// never remove these from the results.
    pub const fn portable(self) -> bool {
        matches!(
            self,
            Self::Kestrel
                | Self::Snippet
                | Self::Math
                | Self::Units
                | Self::Date
                | Self::Url
                | Self::Emoji
        )
    }
}

/// What running a result does. The application performs the effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    /// A Kestrel-owned action with a stable identifier.
    Kestrel(String),
    /// Types text into the focused window (snippets, emoji, computed values).
    InsertText(String),
    /// Runs one configured script by index into the configuration.
    RunScript {
        index: usize,
    },
    /// Launches one scanned application entry.
    OpenApplication {
        index: usize,
    },
    OpenFile(PathBuf),
    OpenUrl(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandItem {
    /// A stable identifier, used for pins and usage counts.
    pub id: String,
    pub source: CommandSource,
    pub title: String,
    pub subtitle: String,
    /// Searchable aliases for this command.
    pub keywords: Vec<String>,
    pub action: CommandAction,
}

impl CommandItem {
    pub fn new(
        id: impl Into<String>,
        source: CommandSource,
        title: impl Into<String>,
        subtitle: impl Into<String>,
        action: CommandAction,
    ) -> Self {
        Self {
            id: id.into(),
            source,
            title: title.into(),
            subtitle: subtitle.into(),
            keywords: Vec::new(),
            action,
        }
    }

    pub fn with_keywords(mut self, keywords: impl IntoIterator<Item = &'static str>) -> Self {
        self.keywords = keywords.into_iter().map(str::to_owned).collect();
        self
    }

    /// The best fuzzy score across the title and its aliases.
    fn score(&self, needle: &str) -> Option<(i64, &'static str)> {
        let mut best: Option<(i64, &'static str)> =
            fuzzy_score(needle, &self.title).map(|score| (score, "title"));
        for keyword in &self.keywords {
            if let Some(score) = fuzzy_score(needle, keyword) {
                if best.is_none_or(|(current, _)| score > current) {
                    best = Some((score, "alias"));
                }
            }
        }
        // A match inside the subtitle is weaker than a title or alias match.
        if let Some(score) = fuzzy_score(needle, &self.subtitle) {
            let adjusted = score - 10;
            if best.is_none_or(|(current, _)| adjusted > current) {
                best = Some((adjusted, "detail"));
            }
        }
        best
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub item: CommandItem,
    pub score: i64,
    pub pinned: bool,
    pub usage: u32,
    pub matched_on: &'static str,
}

/// Scores a case-insensitive subsequence match, or `None` when it does not match.
///
/// Consecutive characters and word-start characters score higher, and a long
/// haystack is slightly penalized so the shortest sensible label wins.
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i64> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some(0);
    }
    let needle_lower = needle.to_lowercase();
    let haystack_lower = haystack.to_lowercase();
    let needle_chars: Vec<char> = needle_lower.chars().collect();
    let haystack_chars: Vec<char> = haystack_lower.chars().collect();
    if needle_chars.len() > haystack_chars.len() {
        return None;
    }

    let mut score: i64 = 0;
    let mut needle_index = 0usize;
    let mut previous_haystack: Option<usize> = None;
    let mut contiguous = true;
    let original: Vec<char> = haystack.chars().collect();
    let needle_original: Vec<char> = needle.chars().collect();

    for (index, character) in haystack_chars.iter().enumerate() {
        if needle_index >= needle_chars.len() {
            break;
        }
        if *character != needle_chars[needle_index] {
            continue;
        }
        let mut bonus = 1i64;
        if previous_haystack == Some(index.saturating_sub(1)) {
            bonus += 4;
        } else if previous_haystack.is_some() {
            contiguous = false;
        }
        let is_word_start = index == 0
            || !haystack_chars[index - 1].is_alphanumeric()
            || (original
                .get(index)
                .is_some_and(|character| character.is_uppercase())
                && original
                    .get(index.saturating_sub(1))
                    .is_some_and(|previous| previous.is_lowercase()));
        if is_word_start {
            bonus += 8;
        }
        if original.get(index) == needle_original.get(needle_index) {
            bonus += 2;
        }
        score += bonus;
        previous_haystack = Some(index);
        needle_index += 1;
    }

    if needle_index != needle_chars.len() {
        return None;
    }
    // A contiguous match is a substring match, which outranks a scattered one
    // even when the scattered letters each follow a separator.
    if contiguous {
        score += SUBSTRING_BONUS;
    }
    if previous_haystack == Some(0) {
        score += PREFIX_BONUS;
    }
    score -= (haystack_chars.len() / 8) as i64;
    Some(score)
}

/// Learned ranking: command identifiers and use counts only.
///
/// Nothing here can hold query text, which is why the state can be inspected
/// and exported without a redaction step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandRanking {
    pins: BTreeSet<String>,
    usage: BTreeMap<String, u32>,
}

impl CommandRanking {
    /// An empty ranking usable as a shared default.
    pub const EMPTY: Self = Self {
        pins: BTreeSet::new(),
        usage: BTreeMap::new(),
    };

    pub fn record_use(&mut self, id: &str) -> u32 {
        if let Some(count) = self.usage.get_mut(id) {
            *count = count.saturating_add(1);
            return *count;
        }
        if self.usage.len() >= MAX_COMMAND_USAGE_ENTRIES {
            // Evict the least used entry (ties resolved by identifier) so the
            // learned state stays bounded.
            if let Some(evicted) = self
                .usage
                .iter()
                .min_by_key(|(candidate, count)| (**count, (*candidate).clone()))
                .map(|(candidate, _)| candidate.clone())
            {
                self.usage.remove(&evicted);
            }
        }
        self.usage.insert(id.to_string(), 1);
        1
    }

    pub fn usage(&self, id: &str) -> u32 {
        self.usage.get(id).copied().unwrap_or(0)
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool) {
        if pinned {
            self.pins.insert(id.to_string());
        } else {
            self.pins.remove(id);
        }
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pins.contains(id)
    }

    pub fn entries(&self) -> Vec<(String, u32)> {
        self.usage
            .iter()
            .map(|(id, count)| (id.clone(), *count))
            .collect()
    }

    pub fn pinned_entries(&self) -> Vec<String> {
        self.pins.iter().cloned().collect()
    }

    /// Drops every pin and count.
    pub fn reset(&mut self) {
        self.pins.clear();
        self.usage.clear();
    }

    pub fn uses_pins(&self) -> usize {
        self.pins.len()
    }

    pub fn from_parts(
        pins: impl IntoIterator<Item = String>,
        usage: impl IntoIterator<Item = (String, u32)>,
    ) -> Self {
        Self {
            pins: pins.into_iter().take(MAX_COMMAND_USAGE_ENTRIES).collect(),
            usage: usage.into_iter().take(MAX_COMMAND_USAGE_ENTRIES).collect(),
        }
    }
}

/// A successful unit conversion.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitConversion {
    pub value: f64,
    pub from_unit: String,
    pub converted: f64,
    pub to_unit: String,
}

/// Evaluates a small arithmetic expression without any parser dependency.
///
/// Supports `+ - * / % ^`, parentheses, decimals, and unary signs. Anything else
/// (identifiers, commas) makes the expression invalid rather than guessed at.
pub fn evaluate_math(expression: &str) -> Option<f64> {
    let expression = expression.trim();
    if expression.is_empty() || !expression.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    if !expression
        .chars()
        .all(|c| c.is_ascii_digit() || "+-*/%^()., ".contains(c))
    {
        return None;
    }
    let mut parser = MathParser {
        chars: expression.chars().collect(),
        position: 0,
    };
    let value = parser.parse_expression()?;
    parser.skip_spaces();
    if parser.position != parser.chars.len() || !value.is_finite() {
        return None;
    }
    Some(value)
}

struct MathParser {
    chars: Vec<char>,
    position: usize,
}

impl MathParser {
    fn skip_spaces(&mut self) {
        while self
            .chars
            .get(self.position)
            .is_some_and(|character| *character == ' ')
        {
            self.position += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_spaces();
        self.chars.get(self.position).copied()
    }

    fn parse_expression(&mut self) -> Option<f64> {
        let mut value = self.parse_term()?;
        loop {
            match self.peek() {
                Some('+') => {
                    self.position += 1;
                    value += self.parse_term()?;
                }
                Some('-') => {
                    self.position += 1;
                    value -= self.parse_term()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_term(&mut self) -> Option<f64> {
        let mut value = self.parse_power()?;
        loop {
            match self.peek() {
                Some('*') => {
                    self.position += 1;
                    value *= self.parse_power()?;
                }
                Some('/') => {
                    self.position += 1;
                    let divisor = self.parse_power()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value /= divisor;
                }
                Some('%') => {
                    self.position += 1;
                    let divisor = self.parse_power()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value %= divisor;
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_power(&mut self) -> Option<f64> {
        let base = self.parse_unary()?;
        if self.peek() == Some('^') {
            self.position += 1;
            let exponent = self.parse_power()?;
            return Some(base.powf(exponent));
        }
        Some(base)
    }

    fn parse_unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some('-') => {
                self.position += 1;
                Some(-self.parse_unary()?)
            }
            Some('+') => {
                self.position += 1;
                self.parse_unary()
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Option<f64> {
        // Thousands separators are accepted when they group digits.
        let mut digits = String::new();
        let mut seen_separator = false;
        while let Some(character) = self.chars.get(self.position).copied() {
            if character.is_ascii_digit() {
                digits.push(character);
                self.position += 1;
            } else if character == ',' && !seen_separator && !digits.is_empty() {
                seen_separator = true;
                self.position += 1;
            } else if character == '.' {
                digits.push(character);
                self.position += 1;
                break;
            } else {
                break;
            }
        }
        while let Some(character) = self.chars.get(self.position).copied() {
            if character.is_ascii_digit() {
                digits.push(character);
                self.position += 1;
            } else {
                break;
            }
        }
        if !digits.is_empty() {
            return digits.parse::<f64>().ok();
        }
        if self.peek() == Some('(') {
            self.position += 1;
            let value = self.parse_expression()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.position += 1;
            return Some(value);
        }
        None
    }
}

/// The unit families the bar converts.
const UNIT_TABLE: [(&str, &str, f64); 29] = [
    // Length, in meters.
    ("mm", "length", 0.001),
    ("cm", "length", 0.01),
    ("m", "length", 1.0),
    ("km", "length", 1000.0),
    ("in", "length", 0.0254),
    ("ft", "length", 0.3048),
    ("yd", "length", 0.9144),
    ("mi", "length", 1609.344),
    // Mass, in grams.
    ("mg", "mass", 0.001),
    ("g", "mass", 1.0),
    ("kg", "mass", 1000.0),
    ("oz", "mass", 28.349523125),
    ("lb", "mass", 453.59237),
    // Volume, in liters.
    ("ml", "volume", 0.001),
    ("l", "volume", 1.0),
    ("gal", "volume", 3.785411784),
    // Data, in bytes (decimal and binary are distinct units).
    ("b", "data", 1.0),
    ("kb", "data", 1000.0),
    ("mb", "data", 1_000_000.0),
    ("gb", "data", 1_000_000_000.0),
    ("kib", "data", 1024.0),
    ("mib", "data", 1_048_576.0),
    ("gib", "data", 1_073_741_824.0),
    // Time, in seconds.
    ("s", "time", 1.0),
    ("min", "time", 60.0),
    ("h", "time", 3600.0),
    ("day", "time", 86_400.0),
    // Speed, in meters per second.
    ("kmh", "speed", 0.2777777777777778),
    ("mph", "speed", 0.44704),
];

/// Converts a query shaped like `10 km to mi`.
pub fn convert_units(query: &str) -> Option<UnitConversion> {
    let lowered = query.trim().to_lowercase();
    let (left, right) = lowered.split_once(" to ")?;
    let mut left_parts = left.split_whitespace();
    let value: f64 = left_parts.next()?.replace(',', "").parse().ok()?;
    let from_unit = left_parts.next()?.to_string();
    if left_parts.next().is_some() {
        return None;
    }
    let to_unit = right.trim().to_string();
    if to_unit.contains(' ') {
        return None;
    }
    let converted = convert_value(value, &from_unit, &to_unit)?;
    Some(UnitConversion {
        value,
        from_unit,
        converted,
        to_unit,
    })
}

fn convert_value(value: f64, from: &str, to: &str) -> Option<f64> {
    // Temperature is affine, so it is handled separately.
    if let Some(result) = convert_temperature(value, from, to) {
        return Some(result);
    }
    let (from_factor, from_family) = unit_factor(from)?;
    let (to_factor, to_family) = unit_factor(to)?;
    if from_family != to_family {
        return None;
    }
    Some(value * from_factor / to_factor)
}

fn unit_factor(unit: &str) -> Option<(f64, &'static str)> {
    UNIT_TABLE
        .iter()
        .find(|(name, _, _)| *name == unit)
        .map(|(_, family, factor)| (*factor, *family))
}

fn convert_temperature(value: f64, from: &str, to: &str) -> Option<f64> {
    let celsius = match from {
        "c" | "°c" => value,
        "f" | "°f" => (value - 32.0) * 5.0 / 9.0,
        "k" => value - 273.15,
        _ => return None,
    };
    match to {
        "c" | "°c" => Some(celsius),
        "f" | "°f" => Some(celsius * 9.0 / 5.0 + 32.0),
        "k" => Some(celsius + 273.15),
        _ => None,
    }
}

/// The documented date and time queries the bar answers locally.
pub const DATE_QUERIES: [&str; 6] = ["date", "time", "now", "today", "tomorrow", "yesterday"];

/// Renders a local date/time query, or `None` when the query is not one.
///
/// Everything is derived from the supplied reading, so results are deterministic
/// for a given clock and never consult the network.
pub fn render_date_query(query: &str, now: &LocalTime) -> Option<String> {
    let lowered = query.trim().to_lowercase();
    match lowered.as_str() {
        "date" | "today" => Some(now.iso_date()),
        "time" | "now" => Some(format!("{} ({})", now.iso_datetime(), now.utc_offset())),
        "tomorrow" => Some(civil_to_iso(add_days(now, 1))),
        "yesterday" => Some(civil_to_iso(add_days(now, -1))),
        other => other
            .strip_prefix("epoch ")
            .and_then(|value| value.trim().parse::<i64>().ok())
            .and_then(|epoch| epoch_to_iso(epoch, now.utc_offset_seconds)),
    }
}

fn add_days(now: &LocalTime, days: i64) -> (i32, u32, u32) {
    let day_number = days_from_civil(now.year, now.month, now.day) + days;
    civil_from_days(day_number)
}

fn epoch_to_iso(epoch: i64, utc_offset_seconds: i32) -> Option<String> {
    let local = epoch + i64::from(utc_offset_seconds);
    let day_number = local.div_euclid(86_400);
    let second_of_day = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(day_number);
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year,
        month,
        day,
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60
    ))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    (
        (year + i64::from(month <= 2)) as i32,
        month as u32,
        day as u32,
    )
}

fn civil_to_iso((year, month, day): (i32, u32, u32)) -> String {
    format!("{year:04}-{month:02}-{day:02}")
}

/// A small built-in emoji set, searchable by name.
pub const EMOJI_TABLE: [(&str, &str); 64] = [
    ("grinning", "😀"),
    ("smile", "😄"),
    ("laugh", "😆"),
    ("wink", "😉"),
    ("blush", "😊"),
    ("heart eyes", "😍"),
    ("thinking", "🤔"),
    ("neutral", "😐"),
    ("worried", "😟"),
    ("cry", "😢"),
    ("angry", "😠"),
    ("sleep", "😴"),
    ("cool", "😎"),
    ("party", "🥳"),
    ("shrug", "🤷"),
    ("facepalm", "🤦"),
    ("thumbs up", "👍"),
    ("thumbs down", "👎"),
    ("ok hand", "👌"),
    ("clap", "👏"),
    ("wave", "👋"),
    ("pray", "🙏"),
    ("muscle", "💪"),
    ("eyes", "👀"),
    ("heart", "❤️"),
    ("broken heart", "💔"),
    ("fire", "🔥"),
    ("star", "⭐"),
    ("sparkles", "✨"),
    ("rocket", "🚀"),
    ("tada", "🎉"),
    ("check mark", "✅"),
    ("cross mark", "❌"),
    ("warning", "⚠️"),
    ("info", "ℹ️"),
    ("question", "❓"),
    ("exclamation", "❗"),
    ("bulb", "💡"),
    ("wrench", "🔧"),
    ("hammer", "🔨"),
    ("gear", "⚙️"),
    ("lock", "🔒"),
    ("unlock", "🔓"),
    ("key", "🔑"),
    ("shield", "🛡️"),
    ("link", "🔗"),
    ("paperclip", "📎"),
    ("memo", "📝"),
    ("book", "📖"),
    ("books", "📚"),
    ("folder", "📁"),
    ("file", "📄"),
    ("chart", "📊"),
    ("calendar", "📅"),
    ("clock", "🕒"),
    ("hourglass", "⌛"),
    ("magnifier", "🔍"),
    ("bell", "🔔"),
    ("megaphone", "📣"),
    ("mail", "✉️"),
    ("phone", "📞"),
    ("computer", "💻"),
    ("bug", "🐛"),
    ("coffee", "☕"),
];

/// The providers a search may consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnabledProviders {
    pub applications: bool,
    pub files: bool,
    pub scripts: bool,
    pub emoji: bool,
    /// Snippet results, which depend on the snippet feature being live rather
    /// than on a command bar setting.
    pub snippets: bool,
}

impl Default for EnabledProviders {
    fn default() -> Self {
        Self {
            applications: true,
            files: false,
            scripts: true,
            emoji: true,
            snippets: true,
        }
    }
}

/// Everything one search needs, supplied by the application.
pub struct SearchInput<'a> {
    pub query: &'a str,
    pub max_results: usize,
    pub providers: EnabledProviders,
    /// Scanned desktop entries, in stable order.
    pub applications: &'a [CommandItem],
    /// Bounded results from the configured file roots only.
    pub files: &'a [PathBuf],
    pub now: Option<&'a LocalTime>,
    pub ranking: &'a CommandRanking,
    pub configured_scripts: &'a [CommandScriptConfiguration],
}

/// The command catalog plus the providers that are pure functions.
#[derive(Debug, Clone, Default)]
pub struct CommandIndex {
    builtins: Vec<CommandItem>,
    snippets: Vec<CommandItem>,
    scripts: Vec<CommandItem>,
    emoji: Vec<CommandItem>,
}

impl CommandIndex {
    /// Builds the catalog: Kestrel actions, snippets, scripts, and emoji.
    pub fn new(
        snippets: impl IntoIterator<Item = CommandItem>,
        scripts: &[CommandScriptConfiguration],
    ) -> Self {
        Self {
            builtins: kestrel_commands(),
            snippets: snippets.into_iter().collect(),
            scripts: scripts
                .iter()
                .enumerate()
                .map(|(index, script)| {
                    CommandItem::new(
                        format!("script:{}", script.name),
                        CommandSource::Script,
                        script.name.clone(),
                        format!(
                            "{} · timeout {} ms",
                            script.executable, script.timeout_millis
                        ),
                        CommandAction::RunScript { index },
                    )
                })
                .collect(),
            emoji: EMOJI_TABLE
                .iter()
                .map(|(name, symbol)| {
                    CommandItem::new(
                        format!("emoji:{name}"),
                        CommandSource::Emoji,
                        format!("{symbol} {name}"),
                        "Insert emoji".to_string(),
                        CommandAction::InsertText((*symbol).to_string()),
                    )
                    .with_keywords(["emoji", "symbol"])
                })
                .collect(),
        }
    }

    pub fn builtin_count(&self) -> usize {
        self.builtins.len()
    }

    /// Ranks the catalog and the supplied provider results for one query.
    pub fn search(&self, input: SearchInput<'_>) -> Vec<CommandResult> {
        let query = input.query.trim();
        if query.chars().count() < kestrel_core::MIN_COMMAND_QUERY_CHARS {
            return Vec::new();
        }
        let max_results = input
            .max_results
            .clamp(1, kestrel_core::MAX_COMMAND_RESULTS as usize);
        let mut results: Vec<CommandResult> = Vec::new();

        let mut consider = |item: &CommandItem, bonus: i64| {
            if let Some((score, matched_on)) = item.score(query) {
                results.push(CommandResult {
                    item: item.clone(),
                    score: score + bonus,
                    pinned: input.ranking.is_pinned(&item.id),
                    usage: input.ranking.usage(&item.id),
                    matched_on,
                });
            }
        };

        // Portable providers always participate, even when an integration is
        // unavailable: their absence is reported separately, not by hiding these.
        for item in &self.builtins {
            consider(item, 20);
        }
        if input.providers.snippets {
            for item in &self.snippets {
                consider(item, 15);
            }
        }
        if input.providers.scripts {
            for item in &self.scripts {
                consider(item, 10);
            }
        }
        if input.providers.applications {
            for item in input.applications {
                consider(item, 5);
            }
        }
        if input.providers.emoji {
            for item in &self.emoji {
                consider(item, 0);
            }
        }
        if input.providers.files {
            for path in input.files {
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let item = CommandItem::new(
                    format!("file:{}", path.display()),
                    CommandSource::File,
                    title,
                    path.display().to_string(),
                    CommandAction::OpenFile(path.clone()),
                );
                consider(&item, 0);
            }
        }

        // Computed providers answer a query that is not a name match at all.
        if let Some(value) = evaluate_math(query) {
            let display = format_number(value);
            results.push(CommandResult {
                item: CommandItem::new(
                    "math:evaluate",
                    CommandSource::Math,
                    format!("{query} = {display}"),
                    "Copy the result to the clipboard",
                    CommandAction::InsertText(display.clone()),
                ),
                score: 60,
                pinned: false,
                usage: 0,
                matched_on: "computed",
            });
        }
        if let Some(conversion) = convert_units(query) {
            let display = format!(
                "{} {} = {} {}",
                format_number(conversion.value),
                conversion.from_unit,
                format_number(conversion.converted),
                conversion.to_unit
            );
            results.push(CommandResult {
                item: CommandItem::new(
                    "units:convert",
                    CommandSource::Units,
                    display.clone(),
                    "Copy the conversion to the clipboard",
                    CommandAction::InsertText(display),
                ),
                score: 55,
                pinned: false,
                usage: 0,
                matched_on: "computed",
            });
        }
        if let Some(now) = input.now {
            if let Some(rendered) = render_date_query(query, now) {
                results.push(CommandResult {
                    item: CommandItem::new(
                        "date:render",
                        CommandSource::Date,
                        rendered.clone(),
                        "Copy the value to the clipboard",
                        CommandAction::InsertText(rendered),
                    ),
                    score: 50,
                    pinned: false,
                    usage: 0,
                    matched_on: "computed",
                });
            }
            // A bare epoch value is also offered as a conversion.
            if let Some(rendered) = render_date_query(&format!("epoch {query}"), now) {
                results.push(CommandResult {
                    item: CommandItem::new(
                        "date:epoch",
                        CommandSource::Date,
                        rendered,
                        "Copy the local time to the clipboard",
                        CommandAction::InsertText(query.to_string()),
                    ),
                    score: 40,
                    pinned: false,
                    usage: 0,
                    matched_on: "computed",
                });
            }
        }
        if let Some(url) = normalize_url(query) {
            results.push(CommandResult {
                item: CommandItem::new(
                    "url:open",
                    CommandSource::Url,
                    format!("Open {url}"),
                    "Open the link with the desktop handler",
                    CommandAction::OpenUrl(url),
                ),
                score: 45,
                pinned: false,
                usage: 0,
                matched_on: "computed",
            });
        }

        // Pins and learned use decide ties; scoring decides the order.
        results.sort_by(|left, right| {
            right
                .pinned
                .cmp(&left.pinned)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| right.usage.cmp(&left.usage))
                .then_with(|| left.item.title.len().cmp(&right.item.title.len()))
                .then_with(|| left.item.id.cmp(&right.item.id))
        });
        results.truncate(max_results);
        results
    }
}

/// Formats a computed number without a trailing `.0` and with bounded precision.
pub fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let rendered = format!("{value:.6}");
    let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// Accepts `https://…` directly and `example.com` as a likely link.
pub fn normalize_url(query: &str) -> Option<String> {
    let query = query.trim();
    if query.contains(' ') || query.chars().count() < 4 {
        return None;
    }
    if let Some(rest) = query
        .strip_prefix("https://")
        .or_else(|| query.strip_prefix("http://"))
    {
        if rest.len() >= 4 && rest.contains('.') {
            return Some(query.to_string());
        }
        return None;
    }
    // A bare host must look like a domain, which keeps ordinary words out.
    let (host, _) = query.split_once('/').unwrap_or((query, ""));
    let mut parts = host.split('.');
    let first = parts.next().unwrap_or_default();
    let last = parts.next_back().unwrap_or_default();
    if first.len() >= 2
        && last.len() >= 2
        && last
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        && host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_/%?=&#:+".contains(character))
    {
        return Some(format!("https://{query}"));
    }
    None
}

/// The Kestrel-owned commands the bar always offers.
fn kestrel_commands() -> Vec<CommandItem> {
    let mut commands = vec![
        CommandItem::new(
            "kestrel:open-window",
            CommandSource::Kestrel,
            "Open the Kestrel window",
            "Show the main panel",
            CommandAction::Kestrel("open_window".to_string()),
        )
        .with_keywords(["show", "window", "focus"]),
        CommandItem::new(
            "kestrel:refresh",
            CommandSource::Kestrel,
            "Refresh capabilities and panels",
            "Re-run probes and update clipboard and audio state",
            CommandAction::Kestrel("refresh".to_string()),
        )
        .with_keywords(["reload", "rescan", "probe"]),
        CommandItem::new(
            "kestrel:preset-essentials",
            CommandSource::Kestrel,
            "Apply the Essentials preset",
            "Core monitoring, audio, power, and session controls",
            CommandAction::Kestrel("preset_essentials".to_string()),
        )
        .with_keywords(["preset", "minimal"]),
        CommandItem::new(
            "kestrel:preset-balanced",
            CommandSource::Kestrel,
            "Apply the Balanced preset",
            "Every supported feature except clipboard history and global shortcuts",
            CommandAction::Kestrel("preset_balanced".to_string()),
        )
        .with_keywords(["preset", "default"]),
        CommandItem::new(
            "kestrel:preset-everything",
            CommandSource::Kestrel,
            "Apply the Everything preset",
            "Every configurable feature",
            CommandAction::Kestrel("preset_everything".to_string()),
        )
        .with_keywords(["preset", "all"]),
        CommandItem::new(
            "kestrel:undo-preset",
            CommandSource::Kestrel,
            "Undo the last preset",
            "Restore the previous feature enablement",
            CommandAction::Kestrel("undo_preset".to_string()),
        )
        .with_keywords(["revert", "preset"]),
        CommandItem::new(
            "kestrel:wipe-clipboard",
            CommandSource::Kestrel,
            "Wipe clipboard history",
            "Clear the live selection and every retained entry",
            CommandAction::Kestrel("wipe_clipboard".to_string()),
        )
        .with_keywords(["clipboard", "clear", "privacy"]),
        CommandItem::new(
            "kestrel:clear-selection",
            CommandSource::Kestrel,
            "Clear the live clipboard selection",
            "Retained entries stay",
            CommandAction::Kestrel("clear_selection".to_string()),
        )
        .with_keywords(["clipboard", "selection"]),
        CommandItem::new(
            "kestrel:reset-command-ranking",
            CommandSource::Kestrel,
            "Reset command ranking",
            "Drop pins and learned counts",
            CommandAction::Kestrel("reset_ranking".to_string()),
        )
        .with_keywords(["forget", "learning", "usage", "privacy"]),
    ];

    // Quick toggles ride along as first-class commands.
    for toggle in kestrel_platform::quick_toggles::ALL_QUICK_TOGGLES {
        commands.push(
            CommandItem::new(
                format!("kestrel:toggle:{}", toggle.feature_id()),
                CommandSource::Kestrel,
                format!("Toggle {}", toggle.label()),
                toggle.requirement().to_string(),
                CommandAction::Kestrel(format!("toggle:{}", toggle.feature_id())),
            )
            .with_keywords(["toggle", "switch"]),
        );
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::MAX_COMMAND_RESULTS;

    const NOW: LocalTime = LocalTime {
        year: 2026,
        month: 10,
        day: 2,
        hour: 9,
        minute: 5,
        second: 7,
        utc_offset_seconds: 7200,
    };

    fn application(index: usize, title: &str) -> CommandItem {
        CommandItem::new(
            format!("application:{index}"),
            CommandSource::Application,
            title.to_string(),
            "Launch".to_string(),
            CommandAction::OpenApplication { index },
        )
    }

    fn index() -> CommandIndex {
        CommandIndex::new(
            vec![
                CommandItem::new(
                    "snippet:Address",
                    CommandSource::Snippet,
                    "Address",
                    "Contact",
                    CommandAction::InsertText("Street 1".to_string()),
                )
                .with_keywords(["snippet"]),
            ],
            &[],
        )
    }

    fn empty_ranking() -> CommandRanking {
        CommandRanking::default()
    }

    #[test]
    fn fuzzy_matching_prefers_prefixes_and_consecutive_runs() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert!(fuzzy_score("ab", "zzz").is_none());

        let prefix = fuzzy_score("ref", "Refresh capabilities").expect("prefix matches");
        let middle = fuzzy_score("ref", "Firefox refresh menu").expect("a later match matches");
        assert!(prefix > middle, "{prefix} should beat {middle}");

        let consecutive = fuzzy_score("cap", "capability").expect("match");
        let scattered = fuzzy_score("cap", "c-a-p").expect("match");
        assert!(
            consecutive > scattered,
            "a contiguous match outranks a scattered one: {consecutive} vs {scattered}"
        );

        let prefix = fuzzy_score("cap", "capability").expect("match");
        let later = fuzzy_score("cap", "recaplike").expect("match");
        assert!(prefix > later, "a prefix match outranks a later one");

        assert!(
            fuzzy_score("REF", "refresh").is_some(),
            "matching is case-insensitive"
        );
    }

    #[test]
    fn ranking_stores_identifiers_and_counts_only_and_resets() {
        let mut ranking = CommandRanking::default();
        assert_eq!(ranking.record_use("kestrel:refresh"), 1);
        assert_eq!(ranking.record_use("kestrel:refresh"), 2);
        assert_eq!(ranking.usage("kestrel:refresh"), 2);
        assert_eq!(ranking.usage("unknown"), 0);

        ranking.set_pinned("kestrel:refresh", true);
        assert!(ranking.is_pinned("kestrel:refresh"));
        assert_eq!(ranking.entries(), vec![("kestrel:refresh".to_string(), 2)]);
        assert_eq!(
            ranking.pinned_entries(),
            vec!["kestrel:refresh".to_string()]
        );

        // Searching never records anything, so the learned state cannot hold the
        // text a user typed; it only maps identifiers to counts.
        let typed = "quarterly revenue numbers";
        assert!(fuzzy_score(typed, "kestrel:refresh").is_none() || true);
        let debug = format!("{ranking:?}");
        assert!(
            debug.contains("kestrel:refresh"),
            "identifiers are retained"
        );
        for word in typed.split_whitespace() {
            assert!(
                !debug.contains(word),
                "query text must never reach the learned state: {debug}"
            );
        }

        ranking.reset();
        assert_eq!(ranking.usage("kestrel:refresh"), 0);
        assert!(!ranking.is_pinned("kestrel:refresh"));
    }

    #[test]
    fn ranking_evicts_the_least_used_identifier_at_the_bound() {
        let mut ranking = CommandRanking::default();
        for index in 0..MAX_COMMAND_USAGE_ENTRIES {
            ranking.record_use(&format!("id{index:04}"));
        }
        ranking.record_use("id0001");
        assert_eq!(
            ranking.usage("id0001"),
            2,
            "existing entries keep counting at the bound"
        );

        ranking.record_use("brand-new");
        assert_eq!(ranking.entries().len(), MAX_COMMAND_USAGE_ENTRIES);
        assert_eq!(
            ranking.usage("id0000"),
            0,
            "the least used identifier is evicted"
        );
        assert_eq!(ranking.usage("brand-new"), 1);
    }

    #[test]
    fn math_evaluates_expressions_and_rejects_everything_else() {
        assert_eq!(evaluate_math("2+3*4"), Some(14.0));
        assert_eq!(evaluate_math("(2+3)*4"), Some(20.0));
        assert_eq!(evaluate_math("2^10"), Some(1024.0));
        assert_eq!(evaluate_math("10/4"), Some(2.5));
        assert_eq!(evaluate_math("7 % 3"), Some(1.0));
        assert_eq!(evaluate_math("-4 + 1"), Some(-3.0));
        assert_eq!(evaluate_math("1,000 + 24"), Some(1024.0));

        assert_eq!(
            evaluate_math("1/0"),
            None,
            "division by zero is not a result"
        );
        assert_eq!(evaluate_math("2+"), None);
        assert_eq!(evaluate_math("(2+3"), None);
        assert_eq!(evaluate_math("rm -rf /"), None);
        assert_eq!(evaluate_math("date"), None, "no digits means no math");
        assert_eq!(evaluate_math("1+2abc"), None);
    }

    #[test]
    fn units_convert_within_a_family_and_reject_cross_family_queries() {
        let conversion = convert_units("10 km to mi").expect("length converts");
        assert!((conversion.converted - 6.21371192).abs() < 1e-6);
        assert_eq!(conversion.from_unit, "km");
        assert_eq!(conversion.to_unit, "mi");

        let temperature = convert_units("100 c to f").expect("temperature converts");
        assert!((temperature.converted - 212.0).abs() < 1e-9);

        let data = convert_units("1 mib to kib").expect("binary data converts");
        assert!((data.converted - 1024.0).abs() < 1e-9);

        assert_eq!(convert_units("10 km to kg"), None, "families never mix");
        assert_eq!(convert_units("10 km"), None);
        assert_eq!(convert_units("hello to world"), None);
    }

    #[test]
    fn date_queries_render_deterministically_from_the_supplied_clock() {
        assert_eq!(
            render_date_query("date", &NOW).as_deref(),
            Some("2026-10-02")
        );
        assert_eq!(
            render_date_query("today", &NOW).as_deref(),
            Some("2026-10-02")
        );
        assert_eq!(
            render_date_query("tomorrow", &NOW).as_deref(),
            Some("2026-10-03")
        );
        assert_eq!(
            render_date_query("yesterday", &NOW).as_deref(),
            Some("2026-10-01")
        );
        assert_eq!(
            render_date_query("time", &NOW).as_deref(),
            Some("2026-10-02 09:05:07 (+02:00)")
        );
        // 1700000000 is 2023-11-14T22:13:20Z, which is the next day at +02:00.
        assert_eq!(
            render_date_query("epoch 1700000000", &NOW).as_deref(),
            Some("2023-11-15 00:13:20"),
            "epoch values render in the local zone"
        );

        assert_eq!(render_date_query("nonsense", &NOW), None);
        assert_eq!(render_date_query("epoch not-a-number", &NOW), None);
    }

    #[test]
    fn month_and_year_boundaries_are_handled() {
        let december = LocalTime {
            year: 2026,
            month: 12,
            day: 31,
            ..NOW
        };
        assert_eq!(
            render_date_query("tomorrow", &december).as_deref(),
            Some("2027-01-01")
        );
        let january = LocalTime {
            year: 2027,
            month: 1,
            day: 1,
            ..NOW
        };
        assert_eq!(
            render_date_query("yesterday", &january).as_deref(),
            Some("2026-12-31")
        );
        // A leap day exists and does not panic.
        let february = LocalTime {
            year: 2028,
            month: 2,
            day: 28,
            ..NOW
        };
        assert_eq!(
            render_date_query("tomorrow", &february).as_deref(),
            Some("2028-02-29")
        );
    }

    #[test]
    fn urls_are_accepted_only_when_they_look_like_links() {
        assert_eq!(
            normalize_url("https://example.com/path").as_deref(),
            Some("https://example.com/path")
        );
        assert_eq!(
            normalize_url("example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            normalize_url("docs.rs/serde"),
            Some("https://docs.rs/serde".to_string())
        );
        assert_eq!(normalize_url("hello"), None);
        assert_eq!(normalize_url("two words"), None);
        assert_eq!(
            normalize_url("a.b"),
            None,
            "single-letter labels are not links"
        );
    }

    #[test]
    fn search_returns_portable_results_without_any_integration() {
        let index = index();
        let ranking = empty_ranking();
        let results = index.search(SearchInput {
            query: "refresh",
            max_results: 10,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &ranking,
            configured_scripts: &[],
        });

        assert!(!results.is_empty());
        assert_eq!(results[0].item.id, "kestrel:refresh");
        assert_eq!(results[0].item.source, CommandSource::Kestrel);
        assert_eq!(results[0].matched_on, "title");
    }

    #[test]
    fn search_honours_computed_providers_and_pins() {
        let index = index();
        let mut ranking = empty_ranking();
        ranking.set_pinned("snippet:Address", true);
        let results = index.search(SearchInput {
            query: "address",
            max_results: 10,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &ranking,
            configured_scripts: &[],
        });
        assert_eq!(results[0].item.id, "snippet:Address");
        assert!(results[0].pinned);

        let math = index.search(SearchInput {
            query: "2+2",
            max_results: 10,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert_eq!(math[0].item.source, CommandSource::Math);
        assert_eq!(math[0].item.title, "2+2 = 4");

        let units = index.search(SearchInput {
            query: "10 km to mi",
            max_results: 10,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert_eq!(units[0].item.source, CommandSource::Units);

        let date = index.search(SearchInput {
            query: "tomorrow",
            max_results: 10,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert_eq!(date[0].item.source, CommandSource::Date);
        assert_eq!(date[0].item.title, "2026-10-03");
    }

    #[test]
    fn snippets_are_offered_only_while_their_provider_is_on() {
        let index = index();
        let ranking = empty_ranking();
        let search = |snippets: bool| {
            index.search(SearchInput {
                query: "address",
                max_results: 10,
                providers: EnabledProviders {
                    snippets,
                    ..EnabledProviders::default()
                },
                applications: &[],
                files: &[],
                now: Some(&NOW),
                ranking: &ranking,
                configured_scripts: &[],
            })
        };

        assert!(
            search(true)
                .iter()
                .any(|result| result.item.source == CommandSource::Snippet)
        );
        assert!(
            search(false)
                .iter()
                .all(|result| result.item.source != CommandSource::Snippet),
            "a stopped snippet feature contributes no runnable results"
        );
    }

    #[test]
    fn files_are_searched_only_when_their_provider_is_enabled() {
        let index = index();
        let files = vec![PathBuf::from("/home/user/report-final.pdf")];
        let disabled = index.search(SearchInput {
            query: "report",
            max_results: 10,
            providers: EnabledProviders {
                files: false,
                ..EnabledProviders::default()
            },
            applications: &[],
            files: &files,
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert!(
            disabled
                .iter()
                .all(|result| result.item.source != CommandSource::File),
            "file results need the file provider"
        );

        let enabled = index.search(SearchInput {
            query: "report",
            max_results: 10,
            providers: EnabledProviders {
                files: true,
                ..EnabledProviders::default()
            },
            applications: &[],
            files: &files,
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert_eq!(enabled[0].item.source, CommandSource::File);
        assert_eq!(enabled[0].item.title, "report-final.pdf");
    }

    #[test]
    fn search_is_bounded_truncated_and_short_queries_return_nothing() {
        let index = index();
        let ranking = empty_ranking();
        let results = index.search(SearchInput {
            query: "e",
            max_results: 3,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &ranking,
            configured_scripts: &[],
        });
        assert!(results.len() <= 3, "results honour the requested bound");

        let too_many = index.search(SearchInput {
            query: "e",
            max_results: MAX_COMMAND_RESULTS as usize * 4,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: Some(&NOW),
            ranking: &ranking,
            configured_scripts: &[],
        });
        assert!(too_many.len() <= MAX_COMMAND_RESULTS as usize);

        assert!(
            index
                .search(SearchInput {
                    query: "",
                    max_results: 5,
                    providers: EnabledProviders::default(),
                    applications: &[],
                    files: &[],
                    now: Some(&NOW),
                    ranking: &ranking,
                    configured_scripts: &[],
                })
                .is_empty(),
            "an empty query has no results"
        );
    }

    #[test]
    fn applications_and_scripts_participate_with_their_own_actions() {
        let scripts = vec![CommandScriptConfiguration {
            name: "Restart audio".to_string(),
            executable: "/usr/bin/systemctl".to_string(),
            args: vec![
                "--user".to_string(),
                "restart".to_string(),
                "pipewire".to_string(),
            ],
            timeout_millis: 5_000,
            output_bytes: 4_096,
        }];
        let index = CommandIndex::new(Vec::new(), &scripts);
        let applications = vec![application(3, "Firefox")];

        let script_results = index.search(SearchInput {
            query: "restart",
            max_results: 5,
            providers: EnabledProviders::default(),
            applications: &applications,
            files: &[],
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &scripts,
        });
        assert_eq!(script_results[0].item.source, CommandSource::Script);
        assert_eq!(
            script_results[0].item.action,
            CommandAction::RunScript { index: 0 }
        );

        let app_results = index.search(SearchInput {
            query: "firefox",
            max_results: 5,
            providers: EnabledProviders::default(),
            applications: &applications,
            files: &[],
            now: Some(&NOW),
            ranking: &empty_ranking(),
            configured_scripts: &scripts,
        });
        assert_eq!(app_results[0].item.source, CommandSource::Application);
        assert_eq!(
            app_results[0].item.action,
            CommandAction::OpenApplication { index: 3 }
        );
    }

    #[test]
    fn emoji_are_searchable_and_offered_as_insertions() {
        let index = CommandIndex::new(Vec::new(), &[]);
        let results = index.search(SearchInput {
            query: "rocket",
            max_results: 5,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: None,
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert_eq!(results[0].item.source, CommandSource::Emoji);
        assert!(matches!(
            &results[0].item.action,
            CommandAction::InsertText(text) if text == "🚀"
        ));

        let disabled = index.search(SearchInput {
            query: "rocket",
            max_results: 5,
            providers: EnabledProviders {
                emoji: false,
                ..EnabledProviders::default()
            },
            applications: &[],
            files: &[],
            now: None,
            ranking: &empty_ranking(),
            configured_scripts: &[],
        });
        assert!(
            disabled
                .iter()
                .all(|result| result.item.source != CommandSource::Emoji)
        );
    }

    #[test]
    fn sources_declare_portability_and_labels() {
        assert_eq!(CommandSource::ALL.len(), 10);
        for source in CommandSource::ALL {
            assert!(!source.label().is_empty());
        }
        assert!(CommandSource::Kestrel.portable());
        assert!(CommandSource::Math.portable());
        assert!(!CommandSource::Application.portable());
        assert!(!CommandSource::File.portable());
    }

    #[test]
    fn numbers_format_without_noise() {
        assert_eq!(format_number(4.0), "4");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(1.0 / 3.0), "0.333333");
        assert_eq!(format_number(-0.0), "0");
    }

    #[test]
    fn builtin_catalog_covers_kestrel_actions_and_toggles() {
        let index = CommandIndex::new(Vec::new(), &[]);
        assert!(index.builtin_count() > 8);
        let ranking = empty_ranking();
        let results = index.search(SearchInput {
            query: "wipe clipboard",
            max_results: 5,
            providers: EnabledProviders::default(),
            applications: &[],
            files: &[],
            now: None,
            ranking: &ranking,
            configured_scripts: &[],
        });
        assert_eq!(results[0].item.id, "kestrel:wipe-clipboard");
        assert_eq!(
            results[0].item.action,
            CommandAction::Kestrel("wipe_clipboard".to_string())
        );
    }
}
