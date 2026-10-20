//! Time-varying rate-card selection: which published rate applies *when*.
//!
//! Some upstreams price by wall clock — DeepSeek's off-peak discount halves
//! every factor outside its weekday peak windows ([ADR-0080]). Rather than
//! teaching the resolver a provider-specific `if`, the window set is a typed
//! value attached to the rate specification, so a rate card stays a pure price
//! table and the calendar stays pure data.
//!
//! Two properties are structural, not conventional:
//!
//! - Selection is a pure function of an epoch-millisecond instant. Nothing in
//!   this module reads a system clock or the user's time zone, so re-rating a
//!   persisted request reproduces the money it was settled at.
//! - The anchor is a fixed UTC offset, not a time-zone database. Every window
//!   an upstream publishes today is stated in a zone without daylight saving
//!   (Asia/Shanghai is UTC+08:00 permanently), so an offset is exact — and it
//!   stays exact when tzdata moves.
//!
//! [ADR-0080]: ../../../../docs/adr/0080-time-varying-rate-cards-and-off-peak-rating.md

use serde::{Deserialize, Serialize};

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_DAY: i64 = 86_400_000;

/// Minutes in one day, in the anchor time zone.
pub const MINUTES_PER_DAY: u16 = 24 * 60;

/// A civil (calendar) date in the anchor time zone — no time, no offset.
///
/// Ordering is chronological: fields are ordered year, month, day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CivilDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl CivilDate {
    pub const fn new(year: i32, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }

    /// Days since 1970-01-01 for this date (negative before the epoch).
    pub fn days_since_epoch(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// Monday-based weekday of this date.
    pub fn weekday(self) -> Weekday {
        Weekday::from_days_since_epoch(self.days_since_epoch())
    }
}

/// One day of the week, indexed Monday = 0 to match [`WeekdayMask`] bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// Monday-based index (`0..=6`).
    pub const fn index(self) -> u8 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }

    /// Weekday for a Monday-based index; the index wraps, so any `u8` is valid.
    pub const fn from_index(index: u8) -> Self {
        match index % 7 {
            0 => Self::Monday,
            1 => Self::Tuesday,
            2 => Self::Wednesday,
            3 => Self::Thursday,
            4 => Self::Friday,
            5 => Self::Saturday,
            _ => Self::Sunday,
        }
    }

    /// Weekday for a count of days since 1970-01-01, which was a Thursday.
    pub fn from_days_since_epoch(days: i64) -> Self {
        Self::from_index(days.rem_euclid(7) as u8 + 3)
    }
}

/// A set of weekdays, one bit per [`Weekday::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WeekdayMask(u8);

impl WeekdayMask {
    pub const MONDAY: Self = Self(1 << 0);
    pub const TUESDAY: Self = Self(1 << 1);
    pub const WEDNESDAY: Self = Self(1 << 2);
    pub const THURSDAY: Self = Self(1 << 3);
    pub const FRIDAY: Self = Self(1 << 4);
    pub const SATURDAY: Self = Self(1 << 5);
    pub const SUNDAY: Self = Self(1 << 6);
    /// The working week as every upstream states it: Monday through Friday.
    pub const MON_TO_FRI: Self = Self(0b0001_1111);
    pub const WEEKEND: Self = Self(0b0110_0000);
    pub const ALL: Self = Self(0b0111_1111);

    pub const fn contains(self, weekday: Weekday) -> bool {
        self.0 & (1 << weekday.index()) != 0
    }

    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// A recurring weekly peak window, expressed in the schedule's anchor offset.
///
/// The range is half-open (`start_minute <= m < end_minute`) and must not wrap
/// across midnight: a window that spans midnight is two windows on two weekday
/// masks, which keeps "which day owns this minute" unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeakWindow {
    days: WeekdayMask,
    start_minute: u16,
    end_minute: u16,
}

impl PeakWindow {
    /// Build a window. Invalid ranges fail at construction — at compile time
    /// when the caller is a `const`, otherwise before the schedule is ever
    /// used to price anything.
    pub const fn new(days: WeekdayMask, start_minute: u16, end_minute: u16) -> Self {
        assert!(
            start_minute < end_minute && end_minute <= MINUTES_PER_DAY,
            "peak window must be a non-empty forward range inside one day"
        );
        Self {
            days,
            start_minute,
            end_minute,
        }
    }

    pub const fn days(self) -> WeekdayMask {
        self.days
    }

    pub const fn start_minute(self) -> u16 {
        self.start_minute
    }

    pub const fn end_minute(self) -> u16 {
        self.end_minute
    }

    /// Whether this window claims the given anchor-local minute.
    pub const fn contains(&self, weekday: Weekday, minute: u16) -> bool {
        self.days.contains(weekday) && minute >= self.start_minute && minute < self.end_minute
    }
}

/// An inclusive span of anchor-local dates on which peak pricing is suspended.
///
/// Ranges mirror the upstream publication (a State Council holiday notice lists
/// periods, not individual days), so the table can be diffed against the source
/// line for line instead of being re-derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayRange {
    pub from: CivilDate,
    pub through: CivilDate,
}

impl HolidayRange {
    pub const fn new(from: CivilDate, through: CivilDate) -> Self {
        Self { from, through }
    }

    pub fn contains(&self, date: CivilDate) -> bool {
        self.from <= date && date <= self.through
    }
}

/// The instant a request is rated at.
///
/// `UNKNOWN` is a first-class state rather than an epoch sentinel: a record
/// persisted before dispatch stamps existed must never buy the off-peak
/// discount, because an under-estimated spend weakens the circuit breaker while
/// an over-estimate only makes it trip early.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RatingInstant(u64);

impl RatingInstant {
    pub const UNKNOWN: Self = Self(0);

    pub const fn from_epoch_ms(epoch_ms: u64) -> Self {
        Self(epoch_ms)
    }

    pub const fn is_known(self) -> bool {
        self.0 != 0
    }

    pub const fn epoch_ms(self) -> Option<u64> {
        if self.0 == 0 { None } else { Some(self.0) }
    }
}

/// Which published rate card a [`RatingInstant`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateWindow {
    /// The base card: what the upstream quotes as its standard price.
    #[default]
    Peak,
    /// The discounted card an upstream grants outside its peak windows.
    OffPeak,
}

impl RateWindow {
    pub const fn is_off_peak(self) -> bool {
        matches!(self, Self::OffPeak)
    }

    /// Stable label for logs, ledgers, and cost surfaces.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Peak => "peak",
            Self::OffPeak => "off-peak",
        }
    }
}

/// Weekly peak windows plus the anchor offset and holiday table that make them
/// absolute.
///
/// Construction is private so an unvalidated window set cannot reach a price
/// calculation: use [`RateSchedule::new`] and [`RateSchedule::with_holidays`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateSchedule {
    anchor_offset_minutes: i16,
    peak_windows: Vec<PeakWindow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    holidays: Vec<HolidayRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    holidays_covered_until: Option<CivilDate>,
}

impl RateSchedule {
    /// A schedule with no holiday exclusions.
    pub fn new(anchor_offset_minutes: i16, peak_windows: Vec<PeakWindow>) -> Self {
        assert!(
            !peak_windows.is_empty(),
            "a rate schedule without peak windows would price everything off-peak"
        );
        assert!(
            (-12 * 60..=14 * 60).contains(&anchor_offset_minutes),
            "anchor offset must be a plausible civil UTC offset"
        );
        Self {
            anchor_offset_minutes,
            peak_windows,
            holidays: Vec::new(),
            holidays_covered_until: None,
        }
    }

    /// Suspend peak pricing across `holidays`; `covered_until` is the last date
    /// the table is known to describe (inclusive).
    pub fn with_holidays(mut self, holidays: Vec<HolidayRange>, covered_until: CivilDate) -> Self {
        self.holidays = holidays;
        self.holidays_covered_until = Some(covered_until);
        self
    }

    pub fn anchor_offset_minutes(&self) -> i16 {
        self.anchor_offset_minutes
    }

    pub fn peak_windows(&self) -> &[PeakWindow] {
        &self.peak_windows
    }

    pub fn holidays(&self) -> &[HolidayRange] {
        &self.holidays
    }

    /// Last anchor-local date the holiday table claims to describe.
    ///
    /// Past this date the table is stale: the schedule keeps working, but every
    /// statutory holiday it does not know about is priced at peak (the
    /// conservative direction).
    pub fn holidays_covered_until(&self) -> Option<CivilDate> {
        self.holidays_covered_until
    }

    /// Whether `date` is an excluded holiday. Answers from the table alone;
    /// consult [`Self::holidays_covered_until`] to tell "not a holiday" from
    /// "not covered".
    pub fn is_holiday(&self, date: CivilDate) -> bool {
        self.holidays.iter().any(|range| range.contains(date))
    }

    /// The anchor-local date an instant falls on, and its weekday.
    pub fn anchor_date(&self, instant: RatingInstant) -> Option<(CivilDate, Weekday)> {
        let epoch_ms = instant.epoch_ms()?;
        let days = self.anchor_days(epoch_ms);
        Some((civil_from_days(days), Weekday::from_days_since_epoch(days)))
    }

    /// Which card applies to `instant`.
    ///
    /// An unknown instant selects the base card. Holiday suspension applies
    /// first: an upstream holiday is off-peak for the whole day, not just
    /// outside its windows.
    pub fn window_at(&self, instant: RatingInstant) -> RateWindow {
        let Some(epoch_ms) = instant.epoch_ms() else {
            return RateWindow::Peak;
        };
        let days = self.anchor_days(epoch_ms);
        let minute = ((epoch_ms as i64)
            .saturating_add(self.anchor_offset_ms())
            .rem_euclid(MS_PER_DAY)
            / MS_PER_MINUTE) as u16;
        if self.is_holiday(civil_from_days(days)) {
            return RateWindow::OffPeak;
        }
        let weekday = Weekday::from_days_since_epoch(days);
        if self
            .peak_windows
            .iter()
            .any(|window| window.contains(weekday, minute))
        {
            RateWindow::Peak
        } else {
            RateWindow::OffPeak
        }
    }

    fn anchor_offset_ms(&self) -> i64 {
        i64::from(self.anchor_offset_minutes) * MS_PER_MINUTE
    }

    fn anchor_days(&self, epoch_ms: u64) -> i64 {
        (epoch_ms as i64)
            .saturating_add(self.anchor_offset_ms())
            .div_euclid(MS_PER_DAY)
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`: exact for every year a provider will
/// ever bill, branch-free arithmetic, no dependency and no leap-second table.
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = if month <= 2 { year - 1 } else { year } as i64;
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_prime = (i64::from(month) + 9) % 12; // [0, 11], March = 0
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> CivilDate {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    CivilDate {
        year: if month <= 2 { year + 1 } else { year } as i32,
        month: month as u8,
        day: day as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(date: &str, hour: u8, minute: u8) -> RatingInstant {
        // Test-only RFC3339-ish parser: `YYYY-MM-DD` at `hour:minute` UTC.
        let year: i32 = date[0..4].parse().unwrap();
        let month: u8 = date[5..7].parse().unwrap();
        let day: u8 = date[8..10].parse().unwrap();
        let epoch_ms = (days_from_civil(year, month, day) * MS_PER_DAY
            + i64::from(hour) * 3_600_000
            + i64::from(minute) * MS_PER_MINUTE) as u64;
        RatingInstant::from_epoch_ms(epoch_ms)
    }

    #[test]
    fn civil_date_arithmetic_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 19), 20_745);
        // Round-trip across leap days and century boundaries.
        for days in [-2_000, -1, 0, 1, 11_016, 20_745, 21_642, 29_000] {
            let date = civil_from_days(days);
            assert_eq!(date.days_since_epoch(), days, "round-trip for {date:?}");
        }
        assert_eq!(
            civil_from_days(days_from_civil(2028, 2, 29)),
            CivilDate::new(2028, 2, 29)
        );
        assert_eq!(civil_from_days(0), CivilDate::new(1970, 1, 1));
    }

    #[test]
    fn weekdays_are_monday_based() {
        assert_eq!(Weekday::from_days_since_epoch(0), Weekday::Thursday);
        assert_eq!(civil_from_days(0).weekday(), Weekday::Thursday);
        // 2026-10-19 is a Monday; 2026-10-24 a Saturday.
        assert_eq!(civil_from_days(20_745).weekday(), Weekday::Monday);
        assert_eq!(civil_from_days(20_750).weekday(), Weekday::Saturday);
        assert!(WeekdayMask::MON_TO_FRI.contains(Weekday::Friday));
        assert!(!WeekdayMask::MON_TO_FRI.contains(Weekday::Saturday));
        assert!(WeekdayMask::WEEKEND.contains(Weekday::Sunday));
        assert_eq!(WeekdayMask::ALL.bits(), 0b0111_1111);
    }

    #[test]
    fn eight_hour_offset_maps_utc_to_asia_shanghai() {
        let schedule = RateSchedule::new(
            480,
            vec![PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60)],
        );
        // 01:00 UTC == 09:00 Beijing, the first peak minute.
        assert_eq!(
            schedule.window_at(instant("2026-10-19", 1, 0)),
            RateWindow::Peak
        );
        assert_eq!(
            schedule.window_at(instant("2026-10-19", 0, 59)),
            RateWindow::OffPeak
        );
        // 04:00 UTC == 12:00 Beijing, the first off-peak minute.
        assert_eq!(
            schedule.window_at(instant("2026-10-19", 4, 0)),
            RateWindow::OffPeak
        );
        assert_eq!(
            schedule.window_at(instant("2026-10-19", 3, 59)),
            RateWindow::Peak
        );
    }

    #[test]
    fn weekends_are_off_peak_without_a_holiday_table() {
        let schedule = RateSchedule::new(
            480,
            vec![PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60)],
        );
        // 2026-10-24 is a Saturday; its 09:00 Beijing is not peak.
        assert_eq!(
            schedule.window_at(instant("2026-10-24", 1, 30)),
            RateWindow::OffPeak
        );
        // The same clock time on the preceding Friday is peak.
        assert_eq!(
            schedule.window_at(instant("2026-10-23", 1, 30)),
            RateWindow::Peak
        );
    }

    #[test]
    fn holiday_ranges_suspend_the_whole_day() {
        let schedule = RateSchedule::new(
            480,
            vec![PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60)],
        )
        .with_holidays(
            vec![HolidayRange::new(
                CivilDate::new(2026, 10, 1),
                CivilDate::new(2026, 10, 7),
            )],
            CivilDate::new(2026, 12, 31),
        );
        // 2026-10-01 is a Thursday inside the National Day span.
        assert_eq!(
            schedule.window_at(instant("2026-10-01", 2, 0)),
            RateWindow::OffPeak
        );
        // 2026-09-30 (Wednesday) is outside it.
        assert_eq!(
            schedule.window_at(instant("2026-09-30", 2, 0)),
            RateWindow::Peak
        );
        // 2026-10-08 (Thursday) is the first day back.
        assert_eq!(
            schedule.window_at(instant("2026-10-08", 2, 0)),
            RateWindow::Peak
        );
        assert!(schedule.is_holiday(CivilDate::new(2026, 10, 7)));
        assert!(!schedule.is_holiday(CivilDate::new(2026, 10, 8)));
        assert_eq!(
            schedule.holidays_covered_until(),
            Some(CivilDate::new(2026, 12, 31))
        );
    }

    #[test]
    fn an_unknown_instant_never_buys_the_discount() {
        let schedule = RateSchedule::new(
            480,
            vec![PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60)],
        );
        assert_eq!(schedule.window_at(RatingInstant::UNKNOWN), RateWindow::Peak);
        assert!(!RatingInstant::UNKNOWN.is_known());
        assert_eq!(RatingInstant::from_epoch_ms(1).epoch_ms(), Some(1));
    }

    #[test]
    fn anchor_dates_are_reported_in_the_anchor_zone() {
        let schedule = RateSchedule::new(480, vec![PeakWindow::new(WeekdayMask::ALL, 0, 60)]);
        // 21:00 UTC on 2026-10-19 is 05:00 Beijing on 2026-10-20.
        let (date, weekday) = schedule.anchor_date(instant("2026-10-19", 21, 0)).unwrap();
        assert_eq!(date, CivilDate::new(2026, 10, 20));
        assert_eq!(weekday, Weekday::Tuesday);
    }

    #[test]
    fn every_minute_of_a_week_selects_exactly_one_window() {
        let schedule = RateSchedule::new(
            480,
            vec![
                PeakWindow::new(WeekdayMask::MON_TO_FRI, 9 * 60, 12 * 60),
                PeakWindow::new(WeekdayMask::MON_TO_FRI, 14 * 60, 18 * 60),
            ],
        );
        // Walk one week from a Monday 00:00 UTC at one-second grain and count
        // peak minutes: two 3h/4h weekday windows over five days.
        let start = instant("2026-10-19", 0, 0);
        let mut peak_minutes = 0usize;
        for minute in 0..(7 * 24 * 60) {
            let epoch_ms = start.epoch_ms().unwrap() + minute * 60_000;
            let window = schedule.window_at(RatingInstant::from_epoch_ms(epoch_ms));
            if window == RateWindow::Peak {
                peak_minutes += 1;
            }
            assert!(matches!(window, RateWindow::Peak | RateWindow::OffPeak));
        }
        assert_eq!(peak_minutes, 5 * (3 * 60 + 4 * 60));
    }

    #[test]
    #[should_panic(expected = "non-empty forward range")]
    fn zero_length_windows_are_rejected() {
        let _ = PeakWindow::new(WeekdayMask::MON_TO_FRI, 540, 540);
    }

    #[test]
    #[should_panic(expected = "non-empty forward range")]
    fn wrapping_windows_are_rejected() {
        let _ = PeakWindow::new(WeekdayMask::MON_TO_FRI, 23 * 60, 25 * 60);
    }

    #[test]
    #[should_panic(expected = "without peak windows")]
    fn empty_window_set_is_rejected() {
        let _ = RateSchedule::new(480, vec![]);
    }
}
