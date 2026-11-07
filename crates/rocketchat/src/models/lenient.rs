//! Lenient deserialisers: one malformed nested value must not reject a whole
//! message, room or list. Invalid values are dropped (logged at debug level).

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn convert<T: DeserializeOwned>(v: Value) -> Option<T> {
    match serde_json::from_value(v) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!(error = %e, ty = std::any::type_name::<T>(), "dropping invalid value");
            None
        }
    }
}

/// Elements of a JSON array that deserialise as `T`; anything else yields an empty list.
pub fn vec_from_value<T: DeserializeOwned>(v: Value) -> Vec<T> {
    match v {
        Value::Array(items) => items.into_iter().filter_map(convert).collect(),
        Value::Null => Vec::new(),
        other => {
            tracing::debug!(value = %other, "expected an array");
            Vec::new()
        }
    }
}

/// Result of leniently parsing a top-level list.
#[derive(Debug)]
pub struct Counted<T> {
    pub items: Vec<T>,
    /// Elements in the server response, including dropped ones.
    pub raw_len: usize,
    /// Elements that failed to parse and were skipped.
    pub dropped: usize,
}

/// Parses a top-level list (messages, rooms, …), skipping invalid elements.
/// Drops are logged at warn level with the element `_id` only (never content).
pub fn counted<T: DeserializeOwned>(v: Value) -> Counted<T> {
    let items = match v {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    let raw_len = items.len();
    let mut out = Vec::with_capacity(raw_len);
    for item in items {
        let id = item.get("_id").and_then(Value::as_str).map(str::to_owned);
        match serde_json::from_value(item) {
            Ok(t) => out.push(t),
            Err(e) => tracing::warn!(
                id = id.as_deref().unwrap_or("?"),
                ty = std::any::type_name::<T>(),
                error = %e,
                "skipping invalid list element"
            ),
        }
    }
    Counted {
        dropped: raw_len - out.len(),
        items: out,
        raw_len,
    }
}

/// `Option<T>`: invalid → `None`.
/// `Some(T)` if `v` converts, else `None` (logged at debug).
pub fn try_convert<T: DeserializeOwned>(v: Value) -> Option<T> {
    convert(v)
}

pub fn opt<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(Option::<Value>::deserialize(d)?.and_then(convert))
}

/// `T: Default`: invalid or null → default.
pub fn or_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<Value>::deserialize(d)?
        .and_then(convert)
        .unwrap_or_default())
}

/// `Vec<T>`: invalid elements skipped.
pub fn vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(vec_from_value(Value::deserialize(d)?))
}

/// `Option<Vec<T>>`: invalid elements skipped; null/non-array → `None`.
pub fn opt_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => Some(items.into_iter().filter_map(convert).collect()),
        _ => None,
    })
}

/// `Option<BTreeMap<String, T>>`: invalid entries skipped.
pub fn opt_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Option<BTreeMap<String, T>>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(m) => Some(
            m.into_iter()
                .filter_map(|(k, v)| convert(v).map(|t| (k, t)))
                .collect(),
        ),
        _ => None,
    })
}
