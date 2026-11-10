//! The R10 driver's settings (`config` feature).
//!
//! [`Config`] is a plain serde struct that implements flightrelay's
//! [`DriverConfig`]. The feature needs no BLE stack, so a settings form or a
//! WebAssembly UI can use it on its own.
//!
//! ```
//! use flightrelay::config::DriverConfig;
//! use flightrelay::units::Distance;
//! use tenover::config::Config;
//!
//! let config = Config {
//!     range: Some(Distance::Feet(7.0)),
//!     aim_offset_deg: Some(-1.5),
//! };
//! config.validate()?;
//! assert_eq!(config.tee_distance_yd(), Some(7.0 / 3.0));
//! assert_eq!(config.aim_offset(), -1.5);
//! // A default config writes nothing.
//! assert!(Config::default().to_map().is_empty());
//! # Ok::<(), flightrelay::config::ConfigError>(())
//! ```

use flightrelay::config::{ConfigError, DriverConfig};
use flightrelay::units::Distance;
use serde::{Deserialize, Serialize};

/// Settings of the R10 driver.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Distance from the device to the tee, sent to the R10 as its tee
    /// distance once it wakes up. Unset leaves the device's own setting
    /// alone. Garmin recommends placing the R10 6 to 8 ft behind the ball.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Distance>,
    /// Correction for a unit that is not aimed exactly at the target
    /// (**beta**), in degrees, within ±10. The R10 measures horizontal
    /// angles from its own centerline; the offset is **added** to the launch
    /// direction, and to the face angle and path when the device tracked the
    /// club. Positive turns shots to the right.
    ///
    /// To set it, hit a short straight chip and enter the negative of the
    /// launch direction it shows (+ = right): a chip that reads 2.0° right
    /// takes `-2.0`. Unset is the same as 0. Applied to the next shot, no new
    /// session needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_offset_deg: Option<f64>,
}

/// The largest aim offset, in degrees, either way.
pub const MAX_AIM_OFFSET_DEG: f64 = 10.0;

impl Config {
    /// The tee distance in yards, the unit the R10 takes; `None` when
    /// [`range`](Self::range) is unset.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // a tee distance fits an f32
    pub fn tee_distance_yd(&self) -> Option<f32> {
        self.range.map(|d| d.as_yards() as f32)
    }

    /// The aim offset in degrees, 0 when [`aim_offset_deg`](Self::aim_offset_deg)
    /// is unset.
    #[must_use]
    pub fn aim_offset(&self) -> f64 {
        self.aim_offset_deg.unwrap_or(0.0)
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(d) = self.range
            && !(d.value().is_finite() && d.value() > 0.0)
        {
            return Err(ConfigError::field(
                "range",
                "must be a distance greater than 0",
            ));
        }
        match self.aim_offset_deg {
            Some(a) if !(a.is_finite() && a.abs() <= MAX_AIM_OFFSET_DEG) => Err(
                ConfigError::field("aim_offset_deg", "must be -10 to 10 degrees"),
            ),
            _ => Ok(()),
        }
    }

    /// Only the range needs a new session: the aim offset applies to the
    /// next shot.
    fn restart_needed(old: &Self, new: &Self) -> bool {
        let new = Self {
            aim_offset_deg: old.aim_offset_deg,
            ..new.clone()
        };
        *old != new
    }

    fn example() -> Self {
        Self {
            range: Some(Distance::Feet(7.0)),
            aim_offset_deg: Some(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};

    fn map(v: &Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_default_config_round_trips_as_nothing() {
        let config = Config::default();
        assert!(config.to_map().is_empty());
        assert_eq!(Config::from_map(Map::new()).unwrap(), config);
        assert_eq!(config.tee_distance_yd(), None);
    }

    #[test]
    fn range_reads_and_writes_a_unit_tagged_distance() {
        let config = Config::from_map(map(&json!({ "range": "8ft" }))).unwrap();
        assert_eq!(config.range, Some(Distance::Feet(8.0)));
        assert_eq!(config.to_map(), map(&json!({ "range": "8ft" })));
        let yd = config.tee_distance_yd().unwrap();
        assert!((yd - 8.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = Config::from_map(map(&json!({ "name": "R10", "range": "2.5m" }))).unwrap();
        assert_eq!(config.range, Some(Distance::Meters(2.5)));
    }

    #[test]
    fn bad_values_name_the_field() {
        for bad in [json!({ "range": "0ft" }), json!({ "range": "-2m" })] {
            let err = Config::from_map(map(&bad)).unwrap_err();
            assert_eq!(err.field.as_deref(), Some("range"), "{err}");
        }
        let err = Config::from_map(map(&json!({ "range": "eight feet" }))).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("range"), "{err}");
        let err = Config::from_map(map(&json!({ "range": 8 }))).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("range"), "{err}");
    }

    #[test]
    fn example_lists_the_optional_keys() {
        assert!(Config::default().to_map().is_empty());
        let example = Config::example().to_map();
        assert!(example.contains_key("range"));
        assert!(example.contains_key("aim_offset_deg"));
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact values through a round trip
    fn the_aim_offset_round_trips() {
        let config = Config::from_map(map(&json!({ "aim_offset_deg": -1.5 }))).unwrap();
        assert_eq!(config.aim_offset_deg, Some(-1.5));
        assert_eq!(config.aim_offset(), -1.5);
        assert_eq!(config.to_map(), map(&json!({ "aim_offset_deg": -1.5 })));
        // Unset writes nothing and reads as 0.
        assert_eq!(Config::default().aim_offset(), 0.0);
        assert!(!Config::default().to_map().contains_key("aim_offset_deg"));
        // An integer is a number too.
        let config = Config::from_map(map(&json!({ "aim_offset_deg": 2 }))).unwrap();
        assert_eq!(config.aim_offset_deg, Some(2.0));
    }

    #[test]
    fn the_aim_offset_is_limited_to_ten_degrees() {
        for ok in [-10.0, -0.5, 0.0, 3.25, 10.0] {
            let config = Config {
                aim_offset_deg: Some(ok),
                ..Config::default()
            };
            assert!(config.validate().is_ok(), "{ok}");
        }
        for bad in [
            10.5,
            -10.01,
            45.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let config = Config {
                aim_offset_deg: Some(bad),
                ..Config::default()
            };
            let err = config.validate().unwrap_err();
            assert_eq!(err.field.as_deref(), Some("aim_offset_deg"), "{bad}: {err}");
        }
        let err = Config::from_map(map(&json!({ "aim_offset_deg": 12 }))).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("aim_offset_deg"), "{err}");
        let err = Config::from_map(map(&json!({ "aim_offset_deg": "left" }))).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("aim_offset_deg"), "{err}");
    }

    #[test]
    fn only_the_range_needs_a_new_session() {
        let old = Config {
            range: Some(Distance::Feet(7.0)),
            ..Config::default()
        };
        let aimed = Config {
            aim_offset_deg: Some(1.0),
            ..old.clone()
        };
        assert!(!Config::restart_needed(&old, &aimed));
        assert!(!Config::restart_needed(&old, &old));
        let moved = Config {
            range: Some(Distance::Feet(9.0)),
            ..aimed
        };
        assert!(Config::restart_needed(&old, &moved));
    }
}
