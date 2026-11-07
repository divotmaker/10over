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
//! let config = Config { range: Some(Distance::Feet(7.0)) };
//! config.validate()?;
//! assert_eq!(config.tee_distance_yd(), Some(7.0 / 3.0));
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
}

impl Config {
    /// The tee distance in yards, the unit the R10 takes; `None` when
    /// [`range`](Self::range) is unset.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // a tee distance fits an f32
    pub fn tee_distance_yd(&self) -> Option<f32> {
        self.range.map(|d| d.as_yards() as f32)
    }
}

impl DriverConfig for Config {
    fn validate(&self) -> Result<(), ConfigError> {
        match self.range {
            Some(d) if !(d.value().is_finite() && d.value() > 0.0) => Err(ConfigError::field(
                "range",
                "must be a distance greater than 0",
            )),
            _ => Ok(()),
        }
    }

    fn example() -> Self {
        Self {
            range: Some(Distance::Feet(7.0)),
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
    fn example_lists_the_optional_range() {
        assert!(Config::default().to_map().is_empty());
        assert!(Config::example().to_map().contains_key("range"));
    }
}
