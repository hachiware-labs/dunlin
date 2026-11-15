//! Configuration values that keep the order of mapping keys.
//!
//! Configuration identifiers are digests of JSON, so key order must survive a round trip
//! through Rust. `serde_json::Value` sorts keys unless its `preserve_order` feature is on, and
//! turning that on would change other JSON this crate returns.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// JavaScript sends NaN and infinities as `{ "$dunlin_nonfinite": "NaN" }`, since JSON has no
/// such numbers.
const NON_FINITE: &str = "$dunlin_nonfinite";

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    Null,
    Bool(bool),
    Number(f64),
    /// NaN or an infinity, kept by name.
    NonFinite(String),
    String(String),
    Array(Vec<ConfigValue>),
    Object(Vec<(String, ConfigValue)>),
}

impl ConfigValue {
    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Self::Number(_) | Self::NonFinite(_))
    }
}

impl Serialize for ConfigValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            // Whole numbers print without a fraction, as JavaScript does.
            // Negative zero prints as -0.0 so that JavaScript reads it back as -0.
            Self::Number(value)
                if value.fract() == 0.0
                    && value.abs() < 9.007_199_254_740_992e15
                    && !(*value == 0.0 && value.is_sign_negative()) =>
            {
                serializer.serialize_i64(*value as i64)
            }
            Self::Number(value) => serializer.serialize_f64(*value),
            Self::NonFinite(name) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(NON_FINITE, name)?;
                map.end()
            }
            Self::String(text) => serializer.serialize_str(text),
            Self::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ConfigValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = ConfigValue;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_unit<E: de::Error>(self) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Null)
            }

            fn visit_none<E: de::Error>(self) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Null)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Number(value as f64))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Number(value as f64))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<ConfigValue, E> {
                Ok(ConfigValue::Number(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ConfigValue, E> {
                Ok(ConfigValue::String(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<ConfigValue, E> {
                Ok(ConfigValue::String(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ConfigValue, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(ConfigValue::Array(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ConfigValue, A::Error> {
                let mut entries: Vec<(String, ConfigValue)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, ConfigValue>()? {
                    // A repeated key keeps its first position, as in JavaScript objects.
                    match entries.iter_mut().find(|(name, _)| *name == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                if let [(key, ConfigValue::String(name))] = entries.as_slice()
                    && key == NON_FINITE
                {
                    return Ok(ConfigValue::NonFinite(name.clone()));
                }
                Ok(ConfigValue::Object(entries))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_key_order_and_non_finite_numbers() {
        let text = r#"{"b":1,"a":[1.5,"x",null,true],"n":{"$dunlin_nonfinite":"NaN"}}"#;
        let value: ConfigValue = serde_json::from_str(text).unwrap();
        assert_eq!(value.get("n"), Some(&ConfigValue::NonFinite("NaN".into())));
        assert_eq!(serde_json::to_string(&value).unwrap(), text);
    }
}
