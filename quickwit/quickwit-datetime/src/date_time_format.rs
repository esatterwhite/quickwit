// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::OnceLock;

use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use time::error::Format;
use time::format_description::modifier::{Day, Month as MonthModifier, Padding, Year, YearRepr};
use time::format_description::well_known::{Iso8601, Rfc2822, Rfc3339};
use time::format_description::{Component, OwnedFormatItem};
use time::parsing::Parsed;
use time::{Month, OffsetDateTime, PrimitiveDateTime};
use time_fmt::parse::time_format_item::parse_to_format_item;

use crate::{RegexTokenizer, TantivyDateTime};

fn literal(s: &[u8]) -> OwnedFormatItem {
    // builds a boxed slice from a slice
    let boxed_slice: Box<[u8]> = s.to_vec().into_boxed_slice();
    OwnedFormatItem::Literal(boxed_slice)
}

fn build_day_item(ptn: &str) -> Option<OwnedFormatItem> {
    let mut day = Day::default();
    if ptn.len() == 2 {
        day.padding = Padding::Zero;
    } else {
        day.padding = Padding::None;
    };
    Some(OwnedFormatItem::Component(Component::Day(day)))
}

fn build_month_item(ptn: &str) -> Option<OwnedFormatItem> {
    let mut month: MonthModifier = Default::default();
    if ptn.len() == 2 {
        month.padding = Padding::Zero;
    } else {
        month.padding = Padding::None;
    }
    Some(OwnedFormatItem::Component(Component::Month(month)))
}

fn build_year_item(ptn: &str) -> Option<OwnedFormatItem> {
    let year_repr = if ptn.len() == 4 {
        YearRepr::Full
    } else {
        YearRepr::LastTwo
    };
    let mut year = Year::default();
    year.repr = year_repr;
    Some(OwnedFormatItem::Component(Component::Year(year)))
}

// Elasticsearch/OpenSearch uses a set of preconfigured formats, more information could be found
// here https://www.elastic.co/guide/en/elasticsearch/reference/current/mapping-date-format.html
fn java_date_format_tokenizer() -> &'static RegexTokenizer<OwnedFormatItem> {
    static JAVA_DATE_FORMAT_TOKENIZER: OnceLock<RegexTokenizer<OwnedFormatItem>> = OnceLock::new();
    JAVA_DATE_FORMAT_TOKENIZER.get_or_init(|| {
        RegexTokenizer::new(vec![
            (r#"yy(yy)?"#, build_year_item),
            (r#"MM?"#, build_month_item),
            (r#"dd?"#, build_day_item),
            (r#"''"#, |_| Some(literal(b"'"))),
            (r#"'[^']+'"#, |s| {
                Some(literal(s[1..s.len() - 1].as_bytes()))
            }),
            (r#"[^\w\[\]{}]"#, |s| Some(literal(s.as_bytes()))),
        ])
        .unwrap()
    })
}

// Check if the given date time format is a common alias and replace it with the
// Java simple date format it is mapped to, if any.
// If the java_datetime_format is not an alias, it is expected to be a
// java simple date time format and should be returned as is.
fn resolve_java_datetime_format_alias(java_datetime_format: &str) -> &str {
    static JAVA_DATE_FORMAT_ALIASES: OnceLock<HashMap<&'static str, &'static str>> =
        OnceLock::new();
    let java_datetime_format_map = JAVA_DATE_FORMAT_ALIASES.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("date_optional_time", "yyyy-MM-dd'T'HH:mm:ss.SSSZ");
        m.insert("strict_date_optional_time", "yyyy-MM-dd'T'HH:mm:ss.SSSZ");
        m.insert(
            "strict_date_optional_time_nanos",
            "yyyy-MM-dd'T'HH:mm:ss.SSSSSSZ",
        );
        m.insert("basic_date", "yyyyMMdd");
        m
    });
    java_datetime_format_map
        .get(java_datetime_format)
        .copied()
        .unwrap_or(java_datetime_format)
}

/// A date time parser that holds the format specification `Vec<FormatItem>`.
#[derive(Clone)]
pub struct StrptimeParser {
    strptime_format: String,
    with_timezone: bool,
    items: Vec<OwnedFormatItem>,
}

impl StrptimeParser {
    /// Parse a given date according to the datetime format specified during the StrptimeParser
    /// creation. If the date format does not provide a specific a time, the time will be set to
    /// 00:00:00.
    fn parse_primitive_date_time(&self, date_time_str: &str) -> anyhow::Result<PrimitiveDateTime> {
        let mut parsed = Parsed::new();
        if !parsed
            .parse_items(date_time_str.as_bytes(), &self.items)?
            .is_empty()
        {
            anyhow::bail!(
                "datetime string `{}` does not match strptime format `{}`",
                date_time_str,
                &self.strptime_format
            );
        }
        // The parsed datetime contains a date but seems to be missing "time".
        // We complete it artificially with 00:00:00.
        if parsed.hour_24().is_none()
            && !(parsed.hour_12().is_some() && parsed.hour_12_is_pm().is_some())
        {
            parsed.set_hour_24(0u8);
            parsed.set_minute(0u8);
            parsed.set_second(0u8);
        }
        if parsed.year().is_none() {
            let now = OffsetDateTime::now_utc();
            let year = infer_year(parsed.month(), now.month(), now.year());
            parsed.set_year(year);
        }
        let date_time = parsed.try_into()?;
        Ok(date_time)
    }

    pub fn parse_date_time(&self, date_time_str: &str) -> Result<OffsetDateTime, String> {
        if self.with_timezone {
            OffsetDateTime::parse(date_time_str, &self.items).map_err(|err| err.to_string())
        } else {
            self.parse_primitive_date_time(date_time_str)
                .map(|date_time| date_time.assume_utc())
                .map_err(|err| err.to_string())
        }
    }

    pub fn format_date_time(&self, date_time: &OffsetDateTime) -> Result<String, Format> {
        date_time.format(&self.items)
    }

    pub fn from_strptime(strptime_format: &str) -> Result<StrptimeParser, String> {
        let items: Vec<OwnedFormatItem> = parse_to_format_item(strptime_format)
            .map_err(|err| format!("invalid strptime format `{strptime_format}`: {err}"))?
            .into_iter()
            .map(|item| item.into())
            .collect();
        Ok(StrptimeParser {
            strptime_format: strptime_format.to_string(),
            with_timezone: strptime_format.to_lowercase().contains("%z"),
            items,
        })
    }

    pub fn from_java_datetime_format(java_datetime_format: &str) -> Result<StrptimeParser, String> {
        let java_datetime_format_resolved = resolve_java_datetime_format_alias(java_datetime_format);
        let items = java_date_format_tokenizer()
            .tokenize(java_datetime_format_resolved)
            .map_err(|pos| {
                format!(
                    "failed to parse date format `{java_datetime_format}`. Pattern at pos {pos} \
                     is not recognized."
                )
            })?;
        Ok(StrptimeParser {
            strptime_format: java_datetime_format_resolved.to_string(),
            with_timezone: false,
            items,
        })
    }
}

impl PartialEq for StrptimeParser {
    fn eq(&self, other: &Self) -> bool {
        self.strptime_format == other.strptime_format
    }
}

impl Eq for StrptimeParser {}

impl std::fmt::Debug for StrptimeParser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StrptimeParser")
            .field("format", &self.strptime_format)
            .finish()
    }
}

impl std::hash::Hash for StrptimeParser {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.strptime_format.hash(state);
    }
}

// `Strftime` format special characters.
// These characters are taken from the parsing crate we use for compatibility.
const STRFTIME_FORMAT_MARKERS: [&str; 36] = [
    "%a", "%A", "%b", "%B", "%c", "%C", "%d", "%D", "%e", "%f", "%F", "%h", "%H", "%I", "%j", "%k",
    "%l", "%m", "%M", "%n", "%p", "%P", "%r", "%R", "%S", "%t", "%T", "%U", "%w", "%W", "%x", "%X",
    "%y", "%Y", "%z", "%Z",
];

// Checks if a format contains `strftime` special characters.
fn is_strftime_formatting(format_str: &str) -> bool {
    STRFTIME_FORMAT_MARKERS
        .iter()
        .any(|marker| format_str.contains(marker))
}

/// Specifies the datetime and unix timestamp formats to use when parsing date strings.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum DateTimeInputFormat {
    Iso8601,
    Rfc2822,
    #[default]
    Rfc3339,
    Strptime(StrptimeParser),
    Timestamp,
}

impl DateTimeInputFormat {
    pub fn as_str(&self) -> &str {
        match self {
            DateTimeInputFormat::Iso8601 => "iso8601",
            DateTimeInputFormat::Rfc2822 => "rfc2822",
            DateTimeInputFormat::Rfc3339 => "rfc3339",
            DateTimeInputFormat::Strptime(parser) => parser.strptime_format.as_str(),
            DateTimeInputFormat::Timestamp => "unix_timestamp",
        }
    }
}

impl Display for DateTimeInputFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DateTimeInputFormat {
    type Err = String;

    fn from_str(date_time_format_str: &str) -> Result<Self, Self::Err> {
        let date_time_format = match date_time_format_str.to_lowercase().as_str() {
            "iso8601" => DateTimeInputFormat::Iso8601,
            "rfc2822" => DateTimeInputFormat::Rfc2822,
            "rfc3339" => DateTimeInputFormat::Rfc3339,
            "unix_timestamp" => DateTimeInputFormat::Timestamp,
            _ => {
                if !is_strftime_formatting(date_time_format_str) {
                    return Err(format!(
                        "unknown input format: `{date_time_format_str}`. a custom date time \
                         format must contain at least one `strftime` special characters"
                    ));
                }
                DateTimeInputFormat::Strptime(StrptimeParser::from_strptime(date_time_format_str)?)
            }
        };
        Ok(date_time_format)
    }
}

impl Serialize for DateTimeInputFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::Serializer {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DateTimeInputFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        let date_time_format_str: String = Deserialize::deserialize(deserializer)?;
        let date_time_format = date_time_format_str.parse().map_err(D::Error::custom)?;
        Ok(date_time_format)
    }
}

/// Specifies the datetime format to use when displaying datetime values.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum DateTimeOutputFormat {
    Iso8601,
    Rfc2822,
    #[default]
    Rfc3339,
    Strptime(StrptimeParser),
    TimestampSecs,
    TimestampMillis,
    TimestampMicros,
    TimestampNanos,
}

impl DateTimeOutputFormat {
    pub fn as_str(&self) -> &str {
        match self {
            DateTimeOutputFormat::Iso8601 => "iso8601",
            DateTimeOutputFormat::Rfc2822 => "rfc2822",
            DateTimeOutputFormat::Rfc3339 => "rfc3339",
            DateTimeOutputFormat::Strptime(parser) => parser.strptime_format.as_str(),
            DateTimeOutputFormat::TimestampSecs => "unix_timestamp_secs",
            DateTimeOutputFormat::TimestampMillis => "unix_timestamp_millis",
            DateTimeOutputFormat::TimestampMicros => "unix_timestamp_micros",
            DateTimeOutputFormat::TimestampNanos => "unix_timestamp_nanos",
        }
    }

    pub fn format_to_json(&self, date_time: TantivyDateTime) -> Result<JsonValue, String> {
        let date = date_time.into_utc();
        let format_result = match &self {
            DateTimeOutputFormat::Rfc3339 => date.format(&Rfc3339).map(JsonValue::String),
            DateTimeOutputFormat::Iso8601 => date.format(&Iso8601::DEFAULT).map(JsonValue::String),
            DateTimeOutputFormat::Rfc2822 => date.format(&Rfc2822).map(JsonValue::String),
            DateTimeOutputFormat::Strptime(strftime_parser) => strftime_parser
                .format_date_time(&date)
                .map(JsonValue::String),
            DateTimeOutputFormat::TimestampSecs => {
                Ok(JsonValue::Number(date_time.into_timestamp_secs().into()))
            }
            DateTimeOutputFormat::TimestampMillis => {
                Ok(JsonValue::Number(date_time.into_timestamp_millis().into()))
            }
            DateTimeOutputFormat::TimestampMicros => {
                Ok(JsonValue::Number(date_time.into_timestamp_micros().into()))
            }
            DateTimeOutputFormat::TimestampNanos => {
                Ok(JsonValue::Number(date_time.into_timestamp_nanos().into()))
            }
        };
        format_result.map_err(|error| error.to_string())
    }
}

impl Display for DateTimeOutputFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DateTimeOutputFormat {
    type Err = String;

    fn from_str(date_time_format_str: &str) -> Result<Self, Self::Err> {
        let date_time_format = match date_time_format_str.to_lowercase().as_str() {
            "iso8601" => DateTimeOutputFormat::Iso8601,
            "rfc2822" => DateTimeOutputFormat::Rfc2822,
            "rfc3339" => DateTimeOutputFormat::Rfc3339,
            "unix_timestamp_secs" => DateTimeOutputFormat::TimestampSecs,
            "unix_timestamp_millis" => DateTimeOutputFormat::TimestampMillis,
            "unix_timestamp_micros" => DateTimeOutputFormat::TimestampMicros,
            "unix_timestamp_nanos" => DateTimeOutputFormat::TimestampNanos,
            _ => {
                if !is_strftime_formatting(date_time_format_str) {
                    return Err(format!(
                        "unknown output format: `{date_time_format_str}`. a custom date time \
                         format must contain at least one `strftime` special characters"
                    ));
                }
                DateTimeOutputFormat::Strptime(StrptimeParser::from_strptime(date_time_format_str)?)
            }
        };
        Ok(date_time_format)
    }
}

impl Serialize for DateTimeOutputFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::Serializer {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DateTimeOutputFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        let date_time_format_str: String = Deserialize::deserialize(deserializer)?;
        let date_time_format = date_time_format_str.parse().map_err(D::Error::custom)?;
        Ok(date_time_format)
    }
}

/// Infers the year of a parsed date time. It assumes that events appear more often delayed than in
/// the future and, as a result, skews towards the past year.
pub(super) fn infer_year(
    parsed_month_opt: Option<Month>,
    this_month: Month,
    this_year: i32,
) -> i32 {
    let Some(parsed_month) = parsed_month_opt else {
        return this_year;
    };
    if parsed_month as u8 > this_month as u8 + 3 {
        return this_year - 1;
    }
    this_year
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;
    use time::Month;

    use super::*;

    #[test]
    fn test_date_time_input_format_ser() {
        let date_time_formats_json = serde_json::to_value(&[
            DateTimeInputFormat::Iso8601,
            DateTimeInputFormat::Rfc2822,
            DateTimeInputFormat::Rfc3339,
            DateTimeInputFormat::Timestamp,
        ])
        .unwrap();

        let expected_date_time_formats =
            serde_json::json!(["iso8601", "rfc2822", "rfc3339", "unix_timestamp",]);
        assert_eq!(date_time_formats_json, expected_date_time_formats);
    }

    #[test]
    fn test_date_time_input_format_deser() {
        let date_time_formats_json = r#"
            [
                "iso8601",
                "rfc2822",
                "rfc3339",
                "unix_timestamp"
            ]
            "#;
        let date_time_formats: Vec<DateTimeInputFormat> =
            serde_json::from_str(date_time_formats_json).unwrap();
        let expected_date_time_formats = [
            DateTimeInputFormat::Iso8601,
            DateTimeInputFormat::Rfc2822,
            DateTimeInputFormat::Rfc3339,
            DateTimeInputFormat::Timestamp,
        ];
        assert_eq!(date_time_formats, &expected_date_time_formats);
    }

    #[test]
    fn test_date_time_output_format_ser() {
        let date_time_formats_json = serde_json::to_value(&[
            DateTimeOutputFormat::Iso8601,
            DateTimeOutputFormat::Rfc2822,
            DateTimeOutputFormat::Rfc3339,
            DateTimeOutputFormat::TimestampSecs,
            DateTimeOutputFormat::TimestampMillis,
            DateTimeOutputFormat::TimestampMicros,
            DateTimeOutputFormat::TimestampNanos,
        ])
        .unwrap();

        let expected_date_time_formats = serde_json::json!([
            "iso8601",
            "rfc2822",
            "rfc3339",
            "unix_timestamp_secs",
            "unix_timestamp_millis",
            "unix_timestamp_micros",
            "unix_timestamp_nanos",
        ]);
        assert_eq!(date_time_formats_json, expected_date_time_formats);
    }

    #[test]
    fn test_date_time_output_format_deser() {
        let date_time_formats_json = r#"
            [
                "iso8601",
                "rfc2822",
                "rfc3339",
                "unix_timestamp_secs",
                "unix_timestamp_millis",
                "unix_timestamp_micros",
                "unix_timestamp_nanos"
            ]
            "#;
        let date_time_formats: Vec<DateTimeOutputFormat> =
            serde_json::from_str(date_time_formats_json).unwrap();
        let expected_date_time_formats = [
            DateTimeOutputFormat::Iso8601,
            DateTimeOutputFormat::Rfc2822,
            DateTimeOutputFormat::Rfc3339,
            DateTimeOutputFormat::TimestampSecs,
            DateTimeOutputFormat::TimestampMillis,
            DateTimeOutputFormat::TimestampMicros,
            DateTimeOutputFormat::TimestampNanos,
        ];
        assert_eq!(date_time_formats, &expected_date_time_formats);
    }

    #[test]
    fn test_fail_date_time_input_format_from_str_with_unknown_format() {
        let formats = vec![
            "test%",
            "test-%v",
            "test-%q",
            "unix_timestamp_secs",
            "unix_timestamp_seconds",
        ];
        for format in formats {
            let error_str = DateTimeInputFormat::from_str(format)
                .unwrap_err()
                .to_string();
            assert!(error_str.contains(&format!("unknown input format: `{format}`")));
        }
    }

    #[test]
    fn test_fail_date_time_output_format_from_str_with_unknown_format() {
        let formats = vec!["test%", "test-%v", "test-%q", "unix_timestamp_seconds"];
        for format in formats {
            let error_str = DateTimeOutputFormat::from_str(format)
                .unwrap_err()
                .to_string();
            assert!(error_str.contains(&format!("unknown output format: `{format}`")));
        }
    }

    #[test]
    fn test_strictly_parse_datetime_format() {
        let parser = StrptimeParser::from_strptime("%Y-%m-%d").unwrap();
        assert_eq!(
            parser.parse_date_time("2021-01-01").unwrap(),
            datetime!(2021-01-01 00:00:00 UTC)
        );
        let error = parser.parse_date_time("2021-01-01TABC").unwrap_err();
        assert_eq!(
            error,
            "datetime string `2021-01-01TABC` does not match strptime format `%Y-%m-%d`"
        );
    }

    #[test]
    fn test_parse_java_datetime_format() {
        let parser = StrptimeParser::from_java_datetime_format("yyyy MM dd").unwrap();
        assert_eq!(
            parser.parse_date_time("2021 01 01").unwrap(),
            datetime!(2021-01-01 00:00:00 UTC)
        );

        let parser = StrptimeParser::from_java_datetime_format("yyyy!MM?dd").unwrap();
        assert_eq!(
            parser.parse_date_time("2021!01?01").unwrap(),
            datetime!(2021-01-01 00:00:00 UTC)
        );
    }

    #[test]
    fn test_infer_year() {
        let inferred_year = infer_year(None, Month::January, 2024);
        assert_eq!(inferred_year, 2024);

        let inferred_year = infer_year(Some(Month::December), Month::January, 2024);
        assert_eq!(inferred_year, 2023);

        let inferred_year = infer_year(Some(Month::January), Month::January, 2024);
        assert_eq!(inferred_year, 2024);

        let inferred_year = infer_year(Some(Month::February), Month::January, 2024);
        assert_eq!(inferred_year, 2024);

        let inferred_year = infer_year(Some(Month::March), Month::January, 2024);
        assert_eq!(inferred_year, 2024);

        let inferred_year = infer_year(Some(Month::April), Month::January, 2024);
        assert_eq!(inferred_year, 2024);

        let inferred_year = infer_year(Some(Month::May), Month::January, 2024);
        assert_eq!(inferred_year, 2023);
    }
}
