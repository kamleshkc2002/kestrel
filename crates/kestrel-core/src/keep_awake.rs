//! How long a keep-awake session runs.

use std::{fmt, time::Duration};

/// The shortest session the UI accepts.
pub const KEEP_AWAKE_MIN: Duration = Duration::from_secs(60);
/// The longest session the UI accepts.
pub const KEEP_AWAKE_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// The choices offered without typing a custom length.
pub const KEEP_AWAKE_PRESETS: [KeepAwakeDuration; 7] = [
    KeepAwakeDuration::UntilStopped,
    KeepAwakeDuration::For(Duration::from_secs(15 * 60)),
    KeepAwakeDuration::For(Duration::from_secs(30 * 60)),
    KeepAwakeDuration::For(Duration::from_secs(60 * 60)),
    KeepAwakeDuration::For(Duration::from_secs(2 * 60 * 60)),
    KeepAwakeDuration::For(Duration::from_secs(4 * 60 * 60)),
    KeepAwakeDuration::For(Duration::from_secs(8 * 60 * 60)),
];

/// A chosen keep-awake length: indefinite, or a bounded timed session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAwakeDuration {
    UntilStopped,
    For(Duration),
}

impl KeepAwakeDuration {
    /// A timed choice within the supported range.
    pub fn timed(duration: Duration) -> Result<Self, KeepAwakeDurationError> {
        if duration < KEEP_AWAKE_MIN || duration > KEEP_AWAKE_MAX {
            return Err(KeepAwakeDurationError {
                requested: duration,
            });
        }
        Ok(Self::For(duration))
    }

    /// The row and menu label for this choice.
    pub fn label(self) -> String {
        match self {
            Self::UntilStopped => "Until turned off".to_owned(),
            Self::For(duration) => timed_label(duration),
        }
    }

    /// The stable command-line suffix, in whole minutes.
    pub fn command_suffix(self) -> String {
        match self {
            Self::UntilStopped => "until-stopped".to_owned(),
            Self::For(duration) => {
                let seconds = duration.as_secs();
                let minutes = seconds / 60 + u64::from(!seconds.is_multiple_of(60));
                if minutes.is_multiple_of(60) {
                    format!("{}h", minutes / 60)
                } else {
                    format!("{minutes}m")
                }
            }
        }
    }

    /// Reads a command-line suffix back, within the supported range.
    pub fn parse_command_suffix(text: &str) -> Option<Self> {
        if text == "until-stopped" {
            return Some(Self::UntilStopped);
        }
        let (amount, unit) = text.split_at(text.len().checked_sub(1)?);
        if amount.is_empty() || !amount.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let value: u64 = amount.parse().ok()?;
        let minutes = match unit {
            "h" => value.checked_mul(60)?,
            "m" => value,
            _ => return None,
        };
        let seconds = minutes.checked_mul(60)?;
        if seconds == 0 {
            return None;
        }
        Self::timed(Duration::from_secs(seconds)).ok()
    }
}

fn timed_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let minutes = seconds / 60 + u64::from(!seconds.is_multiple_of(60));
    let hours = minutes / 60;
    let rest = minutes % 60;
    if hours > 0 && rest > 0 {
        format!("{hours} h {rest} min")
    } else if hours == 1 {
        "1 hour".to_owned()
    } else if hours > 1 {
        format!("{hours} hours")
    } else if minutes == 1 {
        "1 minute".to_owned()
    } else {
        format!("{minutes} minutes")
    }
}

/// A timed choice outside [`KEEP_AWAKE_MIN`]–[`KEEP_AWAKE_MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAwakeDurationError {
    requested: Duration,
}

impl KeepAwakeDurationError {
    /// The rejected length.
    pub fn requested(&self) -> Duration {
        self.requested
    }
}

impl fmt::Display for KeepAwakeDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Choose between 1 minute and 24 hours.")
    }
}

impl std::error::Error for KeepAwakeDurationError {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{KEEP_AWAKE_MAX, KEEP_AWAKE_MIN, KeepAwakeDuration};

    #[test]
    fn timed_choices_stay_within_the_bounds() {
        for (duration, accepted) in [
            (Duration::from_secs(0), false),
            (Duration::from_secs(59), false),
            (KEEP_AWAKE_MIN, true),
            (Duration::from_secs(90 * 60), true),
            (KEEP_AWAKE_MAX, true),
            (KEEP_AWAKE_MAX + Duration::from_secs(1), false),
        ] {
            assert_eq!(
                KeepAwakeDuration::timed(duration).is_ok(),
                accepted,
                "{duration:?}"
            );
        }
        let error = KeepAwakeDuration::timed(Duration::ZERO).expect_err("zero is too short");
        assert_eq!(error.requested(), Duration::ZERO);
    }

    #[test]
    fn command_suffixes_round_trip() {
        for (duration, suffix) in [
            (KeepAwakeDuration::UntilStopped, "until-stopped"),
            (KeepAwakeDuration::For(Duration::from_secs(15 * 60)), "15m"),
            (KeepAwakeDuration::For(Duration::from_secs(30 * 60)), "30m"),
            (KeepAwakeDuration::For(Duration::from_secs(60 * 60)), "1h"),
            (KeepAwakeDuration::For(Duration::from_secs(90 * 60)), "90m"),
            (
                KeepAwakeDuration::For(Duration::from_secs(2 * 60 * 60)),
                "2h",
            ),
            (
                KeepAwakeDuration::For(Duration::from_secs(24 * 60 * 60)),
                "24h",
            ),
        ] {
            assert_eq!(duration.command_suffix(), suffix);
            assert_eq!(
                KeepAwakeDuration::parse_command_suffix(suffix),
                Some(duration)
            );
        }
        for rejected in ["", "0m", "0h", "25h", "1441m", "2d", "m", "hm"] {
            assert_eq!(
                KeepAwakeDuration::parse_command_suffix(rejected),
                None,
                "{rejected}"
            );
        }
    }
}
