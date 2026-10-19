//! How values are written into columns and read back out of rows.
//!
//! Ids and counts are SQLite integers, times are nanoseconds since the Unix epoch in UTC, validated
//! text is stored as its text and parsed again on the way out, and enums and documents are stored
//! in the form their serde derive gives them, so the names live in one place: the domain type.
//! Whatever a row holds that does not parse is reported as a corrupt record, naming the table and
//! the column but never the value.

use std::fmt::Display;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{StoreError, StoreResult};

use crate::error::{corrupt, unstorable};

/// An id or another count as the integer SQLite keeps; above `i64::MAX` it does not fit.
pub(crate) fn int(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| unstorable("a number above the largest stored integer"))
}

/// A time as nanoseconds since the Unix epoch.
pub(crate) fn nanos(at: OffsetDateTime) -> StoreResult<i64> {
    i64::try_from(at.unix_timestamp_nanos()).map_err(|_| unstorable("a time that far from 1970"))
}

/// An optional time as nanoseconds since the Unix epoch.
pub(crate) fn opt_nanos(at: Option<OffsetDateTime>) -> StoreResult<Option<i64>> {
    at.map(nanos).transpose()
}

/// A value as the JSON text its serde derive writes.
pub(crate) fn json<T: Serialize + ?Sized>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(|error| StoreError::Backend(Box::new(error)))
}

/// A unit enum as the name its serde derive gives it.
pub(crate) fn name<T: Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => Ok(name),
        Ok(_) => Err(unstorable("a value without a name")),
        Err(error) => Err(StoreError::Backend(Box::new(error))),
    }
}

/// The unit enum of this name.
pub(crate) fn from_name<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_value(Value::String(text.to_owned()))
}

/// The columns of one row of one table, read into the values they hold.
pub(crate) struct Fields<'r> {
    row: &'r SqliteRow,
    table: &'static str,
}

impl<'r> Fields<'r> {
    /// The row, from the table named for the error a bad column reports.
    pub(crate) fn of(row: &'r SqliteRow, table: &'static str) -> Self {
        Self { row, table }
    }

    /// The error for a column that does not hold a valid value.
    pub(crate) fn bad(&self, column: &str, detail: impl Display) -> StoreError {
        corrupt(format!("{}.{column}: {detail}", self.table))
    }

    /// An integer column.
    pub(crate) fn int(&self, column: &str) -> StoreResult<i64> {
        self.row
            .try_get::<i64, _>(column)
            .map_err(|error| self.bad(column, error))
    }

    /// An integer column that may be null.
    pub(crate) fn opt_int(&self, column: &str) -> StoreResult<Option<i64>> {
        self.row
            .try_get::<Option<i64>, _>(column)
            .map_err(|error| self.bad(column, error))
    }

    /// An id or a count: an integer that is never negative.
    pub(crate) fn number(&self, column: &str) -> StoreResult<u64> {
        let value = self.int(column)?;
        u64::try_from(value).map_err(|_| self.bad(column, "a negative number"))
    }

    /// An id that may be null.
    pub(crate) fn opt_number(&self, column: &str) -> StoreResult<Option<u64>> {
        self.opt_int(column)?
            .map(|value| u64::try_from(value).map_err(|_| self.bad(column, "a negative number")))
            .transpose()
    }

    /// A text column.
    pub(crate) fn text(&self, column: &str) -> StoreResult<String> {
        self.row
            .try_get::<String, _>(column)
            .map_err(|error| self.bad(column, error))
    }

    /// A text column that may be null.
    pub(crate) fn opt_text(&self, column: &str) -> StoreResult<Option<String>> {
        self.row
            .try_get::<Option<String>, _>(column)
            .map_err(|error| self.bad(column, error))
    }

    /// A time column.
    pub(crate) fn time(&self, column: &str) -> StoreResult<OffsetDateTime> {
        let value = self.int(column)?;
        self.at(column, value)
    }

    /// A time column that may be null.
    pub(crate) fn opt_time(&self, column: &str) -> StoreResult<Option<OffsetDateTime>> {
        self.opt_int(column)?
            .map(|value| self.at(column, value))
            .transpose()
    }

    /// A column holding 0 or 1.
    pub(crate) fn flag(&self, column: &str) -> StoreResult<bool> {
        match self.int(column)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.bad(column, "neither 0 nor 1")),
        }
    }

    /// A text column parsed into the value it holds.
    pub(crate) fn parse<T, E: Display>(
        &self,
        column: &str,
        parse: impl FnOnce(&str) -> Result<T, E>,
    ) -> StoreResult<T> {
        let text = self.text(column)?;
        parse(&text).map_err(|error| self.bad(column, error))
    }

    /// A text column that may be null, parsed into the value it holds.
    pub(crate) fn opt_parse<T, E: Display>(
        &self,
        column: &str,
        parse: impl FnOnce(&str) -> Result<T, E>,
    ) -> StoreResult<Option<T>> {
        self.opt_text(column)?
            .map(|text| parse(&text).map_err(|error| self.bad(column, error)))
            .transpose()
    }

    /// A unit enum stored by its name.
    pub(crate) fn named<T: DeserializeOwned>(&self, column: &str) -> StoreResult<T> {
        self.parse(column, from_name)
    }

    /// A JSON column read into the value it holds.
    pub(crate) fn json<T: DeserializeOwned>(&self, column: &str) -> StoreResult<T> {
        self.parse(column, |text| serde_json::from_str::<T>(text))
    }

    fn at(&self, column: &str, nanos: i64) -> StoreResult<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
            .map_err(|error| self.bad(column, error))
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::item::ItemStatus;
    use dibs_domain::member::Role;

    use super::*;

    #[test]
    fn numbers_above_the_largest_integer_do_not_fit() {
        assert_eq!(int(42).unwrap(), 42);
        assert_eq!(int(u64::try_from(i64::MAX).unwrap()).unwrap(), i64::MAX);
        assert!(matches!(int(u64::MAX), Err(StoreError::Backend(_))));
    }

    #[test]
    fn times_are_kept_to_the_nanosecond() {
        let at = OffsetDateTime::UNIX_EPOCH + Duration::nanoseconds(1_700_000_000_123_456_789);
        assert_eq!(nanos(at).unwrap(), 1_700_000_000_123_456_789);
        assert_eq!(
            nanos(OffsetDateTime::UNIX_EPOCH - Duration::seconds(1)).unwrap(),
            -1_000_000_000
        );
        assert_eq!(opt_nanos(None).unwrap(), None);
        let far = OffsetDateTime::UNIX_EPOCH + Duration::days(365 * 400);
        assert!(matches!(nanos(far), Err(StoreError::Backend(_))));
    }

    #[test]
    fn enums_are_stored_by_their_serde_names() {
        assert_eq!(name(&ItemStatus::Dropped).unwrap(), "dropped");
        assert_eq!(name(&Role::Importer).unwrap(), "importer");
        assert_eq!(
            from_name::<ItemStatus>("claimed").unwrap(),
            ItemStatus::Claimed
        );
        assert!(from_name::<Role>("owner").is_err());
        assert!(matches!(name(&1_u8), Err(StoreError::Backend(_))));
    }
}
