//! Shared reader for portable configuration sections.

use std::{fmt::Display, ops::RangeInclusive};

use kestrel_core::ConfigurationError;
use serde::de::DeserializeOwned;

use super::{ConfigurationWarning, LoadedConfiguration, warning};

/// Message for a value that must be `true` or `false`.
pub(super) const BOOL_MESSAGE: &str = "Must be true or false; the default was retained.";

/// One TOML section and the warnings it produced, in document order.
pub(super) struct Section<'a> {
    name: &'a str,
    table: &'a toml::Table,
    warnings: Vec<ConfigurationWarning>,
}

impl<'a> Section<'a> {
    /// Opens a section, warning when it is present but not a table.
    pub(super) fn open(
        loaded: &mut LoadedConfiguration,
        name: &'a str,
        value: Option<&'a toml::Value>,
    ) -> Option<Self> {
        let value = value?;
        match value.as_table() {
            Some(table) => Some(Self::nested(name, table)),
            None => {
                loaded.warnings.push(warning(
                    name,
                    format!("The {name} value must be a TOML table."),
                ));
                None
            }
        }
    }

    /// Opens a nested table under `name`, for sub-sections with custom text.
    pub(super) fn nested(name: &'a str, table: &'a toml::Table) -> Self {
        Self {
            name,
            table,
            warnings: Vec::new(),
        }
    }

    pub(super) fn table(&self) -> &'a toml::Table {
        self.table
    }

    pub(super) fn get(&self, key: &str) -> Option<&'a toml::Value> {
        self.table.get(key)
    }

    /// Reads `name.key` into `target`, warning with a fixed message.
    pub(super) fn read<T: DeserializeOwned>(&mut self, key: &str, message: &str, target: &mut T) {
        let Some(value) = self.table.get(key) else {
            return;
        };
        match T::deserialize(value.clone()) {
            Ok(parsed) => *target = parsed,
            Err(_) => self.warn(key, message),
        }
    }

    /// Reads `name.key` into `target`, reporting the deserialization error.
    pub(super) fn read_detailed<T: DeserializeOwned>(
        &mut self,
        key: &str,
        message: &str,
        target: &mut T,
    ) {
        let Some(value) = self.table.get(key) else {
            return;
        };
        match T::deserialize(value.clone()) {
            Ok(parsed) => *target = parsed,
            Err(error) => self.warn(key, format!("{message}: {error}")),
        }
    }

    /// Reads a number in `range` into `target`, warning when it is outside.
    ///
    /// `bounds` is the message prefix before "between" and the unit suffix.
    pub(super) fn in_range<T>(
        &mut self,
        key: &str,
        range: RangeInclusive<T>,
        bounds: (&str, &str),
        target: &mut T,
    ) where
        T: DeserializeOwned + PartialOrd + Display,
    {
        if let Some(parsed) = self.number(key, range, bounds) {
            *target = parsed;
        }
    }

    /// Reads a number in `range`; the caller keeps its default when `None`.
    pub(super) fn number<T>(
        &mut self,
        key: &str,
        range: RangeInclusive<T>,
        (prefix, unit): (&str, &str),
    ) -> Option<T>
    where
        T: DeserializeOwned + PartialOrd + Display,
    {
        let value = self.table.get(key)?;
        match T::deserialize(value.clone()) {
            Ok(parsed) if range.contains(&parsed) => Some(parsed),
            _ => {
                let (minimum, maximum) = (range.start(), range.end());
                self.warn(
                    key,
                    format!(
                        "{prefix} between {minimum} and {maximum}{unit}; the default was retained."
                    ),
                );
                None
            }
        }
    }

    /// Warns at `name.key`.
    pub(super) fn warn(&mut self, key: &str, message: impl Into<String>) {
        self.warnings
            .push(warning(&format!("{}.{key}", self.name), message));
    }

    /// Warns at `location`, for locations that do not extend the section name.
    pub(super) fn warn_at(&mut self, location: &str, message: impl Into<String>) {
        self.warnings.push(warning(location, message));
    }

    /// Appends a nested section's warnings in order.
    pub(super) fn absorb(&mut self, child: Section<'_>) {
        self.warnings.extend(child.warnings);
    }

    /// Moves collected warnings into `loaded`, in order.
    pub(super) fn finish(self, loaded: &mut LoadedConfiguration) {
        loaded.warnings.extend(self.warnings);
    }
}

/// Reason for a feature entry that failed validation.
pub(super) fn feature_reason(error: ConfigurationError) -> String {
    format!("Feature settings were ignored: {error:?}")
}
