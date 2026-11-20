//! How long a claim has been held, in the columns of the age grid: a claim in review has a column
//! of its own whatever its age; any other is placed by the time since it was claimed.

use time::Duration;

/// The longest a claim of the first column of days has been held.
const A_DAY: Duration = Duration::days(1);

/// The longest a claim of the column of days has been held.
const A_WEEK: Duration = Duration::days(7);

/// The longest a claim of the second column of weeks has been held.
const TWO_WEEKS: Duration = Duration::days(14);

/// The longest a claim of the third column of weeks has been held.
const THREE_WEEKS: Duration = Duration::days(21);

/// A column of the age grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Age {
    /// In review, whatever its age.
    Review,
    /// Held for less than a day.
    UnderADay,
    /// Held for one day to seven days.
    Days,
    /// Held for more than a week, up to two.
    OneToTwoWeeks,
    /// Held for more than two weeks, up to three.
    TwoToThreeWeeks,
    /// Held for more than three weeks.
    OverThreeWeeks,
}

impl Age {
    /// The columns, in the order the grid shows them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Review,
        Self::UnderADay,
        Self::Days,
        Self::OneToTwoWeeks,
        Self::TwoToThreeWeeks,
        Self::OverThreeWeeks,
    ];

    /// The column of a claim, in review or not, claimed `held` ago.
    pub(crate) fn of(in_review: bool, held: Duration) -> Self {
        if in_review {
            Self::Review
        } else if held < A_DAY {
            Self::UnderADay
        } else if held <= A_WEEK {
            Self::Days
        } else if held <= TWO_WEEKS {
            Self::OneToTwoWeeks
        } else if held <= THREE_WEEKS {
            Self::TwoToThreeWeeks
        } else {
            Self::OverThreeWeeks
        }
    }

    /// Whether the column holds claims held for more than a week.
    pub(crate) const fn old(self) -> bool {
        matches!(
            self,
            Self::OneToTwoWeeks | Self::TwoToThreeWeeks | Self::OverThreeWeeks
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_in_review_has_its_own_column_whatever_its_age() {
        for held in [Duration::ZERO, Duration::days(3), Duration::days(40)] {
            assert_eq!(Age::of(true, held), Age::Review, "{held}");
        }
    }

    #[test]
    fn each_column_ends_where_the_next_begins() {
        let second = Duration::seconds(1);
        let cases = [
            (Duration::ZERO, Age::UnderADay),
            (A_DAY - second, Age::UnderADay),
            (A_DAY, Age::Days),
            (Duration::days(3), Age::Days),
            (A_WEEK, Age::Days),
            (A_WEEK + second, Age::OneToTwoWeeks),
            (TWO_WEEKS, Age::OneToTwoWeeks),
            (TWO_WEEKS + second, Age::TwoToThreeWeeks),
            (Duration::days(16), Age::TwoToThreeWeeks),
            (THREE_WEEKS, Age::TwoToThreeWeeks),
            (THREE_WEEKS + second, Age::OverThreeWeeks),
            (Duration::days(400), Age::OverThreeWeeks),
        ];
        for (held, column) in cases {
            assert_eq!(Age::of(false, held), column, "{held}");
        }
    }

    #[test]
    fn a_claim_the_clock_places_after_now_is_under_a_day() {
        assert_eq!(Age::of(false, -Duration::minutes(5)), Age::UnderADay);
    }

    #[test]
    fn only_the_columns_past_a_week_are_old() {
        let old: Vec<Age> = Age::ALL.into_iter().filter(|age| age.old()).collect();
        assert_eq!(
            old,
            [
                Age::OneToTwoWeeks,
                Age::TwoToThreeWeeks,
                Age::OverThreeWeeks
            ]
        );
    }
}
