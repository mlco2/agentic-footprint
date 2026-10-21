use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::Value;

/// A body with more log records than this is refused whole: real exporters
/// batch a few hundred, and decoding is the step a hostile body amplifies.
pub const MAX_LOG_RECORDS: usize = 10_000;

/// An attribute string longer than this is dropped at decode. Every field a
/// normalizer reads is an id, a name or a number; a megabyte value is
/// either a prompt we never keep or an attack.
pub const MAX_ATTRIBUTE_BYTES: usize = 64 * 1024;

/// An attribute whose *name* is longer than this is dropped at decode:
/// names reach reject metadata, which must stay small.
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 256;

pub(crate) type Attributes = HashMap<String, Value>;

#[derive(Debug)]
pub(crate) struct LogRecord {
    pub body: Option<String>,
    pub time_unix_nano: Option<i128>,
    /// This record's own attributes. Scope and resource attributes are
    /// shared, not copied: the lookup falls back record → scope → resource,
    /// the precedence the old per-record merge had.
    pub attributes: Attributes,
    pub scope: Arc<Attributes>,
    pub resource: Arc<Attributes>,
    pub scope_name: Option<String>,
}

pub(crate) struct Decoded {
    pub records: Vec<LogRecord>,
    pub over_capacity: bool,
}

impl LogRecord {
    pub fn attribute(&self, key: &str) -> Option<&Value> {
        self.attributes
            .get(key)
            .or_else(|| self.scope.get(key))
            .or_else(|| self.resource.get(key))
    }

    pub fn has(&self, key: &str) -> bool {
        self.attribute(key).is_some()
    }

    /// Every attribute name visible on this record — for reject metadata,
    /// which records names only, never values.
    pub fn keys(&self) -> BTreeSet<&str> {
        self.attributes
            .keys()
            .chain(self.scope.keys())
            .chain(self.resource.keys())
            .map(String::as_str)
            .collect()
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.attribute(key).and_then(attr_string)
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.attribute(key).and_then(attr_u64)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.attribute(key)
            .and_then(|value| value.get("boolValue"))
            .and_then(Value::as_bool)
    }
}

pub(crate) fn decode_logs(body: &Value) -> Decoded {
    let mut records = Vec::new();
    let Some(resource_logs) = body.get("resourceLogs").and_then(Value::as_array) else {
        return Decoded {
            records,
            over_capacity: false,
        };
    };
    let total: usize = resource_logs
        .iter()
        .filter_map(|resource| resource.get("scopeLogs").and_then(Value::as_array))
        .flatten()
        .filter_map(|scope| scope.get("logRecords").and_then(Value::as_array))
        .map(Vec::len)
        .sum();
    if total > MAX_LOG_RECORDS {
        return Decoded {
            records,
            over_capacity: true,
        };
    }
    for resource_log in resource_logs {
        let resource = Arc::new(
            resource_log
                .pointer("/resource/attributes")
                .and_then(Value::as_array)
                .map(|values| attribute_map(values))
                .unwrap_or_default(),
        );
        let Some(scope_logs) = resource_log.get("scopeLogs").and_then(Value::as_array) else {
            continue;
        };
        for scope_log in scope_logs {
            let scope_name = scope_log
                .pointer("/scope/name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let scope = Arc::new(
                scope_log
                    .pointer("/scope/attributes")
                    .and_then(Value::as_array)
                    .map(|values| attribute_map(values))
                    .unwrap_or_default(),
            );
            let Some(log_records) = scope_log.get("logRecords").and_then(Value::as_array) else {
                continue;
            };
            for record in log_records {
                records.push(LogRecord {
                    body: record
                        .pointer("/body/stringValue")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    time_unix_nano: record.get("timeUnixNano").and_then(record_time_unix_nano),
                    attributes: record
                        .get("attributes")
                        .and_then(Value::as_array)
                        .map(|values| attribute_map(values))
                        .unwrap_or_default(),
                    scope: Arc::clone(&scope),
                    resource: Arc::clone(&resource),
                    scope_name: scope_name.clone(),
                });
            }
        }
    }
    Decoded {
        records,
        over_capacity: false,
    }
}

fn attribute_map(values: &[Value]) -> Attributes {
    values
        .iter()
        .filter_map(|attribute| {
            let value = attribute.get("value")?;
            let oversized = value
                .get("stringValue")
                .and_then(Value::as_str)
                .is_some_and(|text| text.len() > MAX_ATTRIBUTE_BYTES);
            if oversized {
                return None;
            }
            let key = attribute.get("key")?.as_str()?;
            if key.len() > MAX_ATTRIBUTE_KEY_BYTES {
                return None;
            }
            Some((key.to_string(), value.clone()))
        })
        .collect()
}

fn attr_string(value: &Value) -> Option<String> {
    value.get("stringValue")?.as_str().map(str::to_string)
}

fn attr_u64(value: &Value) -> Option<u64> {
    if let Some(int_value) = value.get("intValue") {
        return int_value
            .as_u64()
            .or_else(|| int_value.as_str().and_then(|value| value.parse().ok()));
    }
    if let Some(string_value) = value.get("stringValue").and_then(Value::as_str) {
        return string_value.parse().ok();
    }
    value
        .get("doubleValue")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0 && value.fract() == 0.0)
        .map(|value| value as u64)
}

fn record_time_unix_nano(value: &Value) -> Option<i128> {
    if let Some(value) = value.as_str() {
        return value.parse().ok();
    }
    if let Some(value) = value.as_i64() {
        return Some(i128::from(value));
    }
    value.as_u64().map(i128::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(records: usize, resource_value: &str) -> Value {
        json!({"resourceLogs": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": resource_value}}]},
            "scopeLogs": [{
                "scope": {"name": "s", "attributes": [{"key": "scope.k", "value": {"stringValue": "scoped"}}]},
                "logRecords": (0..records).map(|i| json!({
                    "attributes": [{"key": "i", "value": {"intValue": i}}]
                })).collect::<Vec<_>>()
            }]
        }]})
    }

    #[test]
    fn records_share_their_resource_and_scope_attributes() {
        let decoded = decode_logs(&body(3, "svc"));
        assert_eq!(decoded.records.len(), 3);
        assert!(Arc::ptr_eq(
            &decoded.records[0].resource,
            &decoded.records[2].resource
        ));
        assert!(Arc::ptr_eq(
            &decoded.records[0].scope,
            &decoded.records[2].scope
        ));
        assert_eq!(
            decoded.records[1].string("service.name").as_deref(),
            Some("svc")
        );
        assert_eq!(
            decoded.records[1].string("scope.k").as_deref(),
            Some("scoped")
        );
        assert_eq!(decoded.records[1].u64("i"), Some(1));
    }

    #[test]
    fn a_record_attribute_overrides_scope_and_resource() {
        let value = json!({"resourceLogs": [{
            "resource": {"attributes": [{"key": "k", "value": {"stringValue": "resource"}}]},
            "scopeLogs": [{
                "scope": {"attributes": [{"key": "k", "value": {"stringValue": "scope"}}]},
                "logRecords": [
                    {"attributes": [{"key": "k", "value": {"stringValue": "record"}}]},
                    {}
                ]
            }]
        }]});
        let decoded = decode_logs(&value);
        assert_eq!(decoded.records[0].string("k").as_deref(), Some("record"));
        assert_eq!(decoded.records[1].string("k").as_deref(), Some("scope"));
    }

    #[test]
    fn more_than_the_record_cap_is_refused_whole() {
        let decoded = decode_logs(&body(MAX_LOG_RECORDS + 1, "svc"));
        assert!(decoded.over_capacity);
        assert!(decoded.records.is_empty(), "nothing is half-processed");
    }

    #[test]
    fn an_oversized_attribute_value_is_dropped_not_truncated() {
        let big = "x".repeat(MAX_ATTRIBUTE_BYTES + 1);
        let decoded = decode_logs(&body(1, &big));
        assert_eq!(decoded.records[0].string("service.name"), None);
        assert_eq!(decoded.records[0].u64("i"), Some(0));
    }
}
