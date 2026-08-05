use std::collections::HashMap;

use serde_json::Value;

#[derive(Debug)]
pub(crate) struct LogRecord {
    pub body: Option<String>,
    pub time_unix_nano: Option<i128>,
    pub attributes: HashMap<String, Value>,
    pub scope_name: Option<String>,
}

impl LogRecord {
    pub fn string(&self, key: &str) -> Option<String> {
        self.attributes.get(key).and_then(attr_string)
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.attributes.get(key).and_then(attr_u64)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.attributes
            .get(key)
            .and_then(|value| value.get("boolValue"))
            .and_then(Value::as_bool)
    }
}

pub(crate) fn decode_logs(body: &Value) -> Vec<LogRecord> {
    let mut records = Vec::new();
    let Some(resource_logs) = body.get("resourceLogs").and_then(Value::as_array) else {
        return records;
    };
    for resource_log in resource_logs {
        let resource_attrs = resource_log
            .pointer("/resource/attributes")
            .and_then(Value::as_array)
            .map(|values| attribute_map(values))
            .unwrap_or_default();
        let Some(scope_logs) = resource_log.get("scopeLogs").and_then(Value::as_array) else {
            continue;
        };
        for scope_log in scope_logs {
            let scope_name = scope_log
                .pointer("/scope/name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let scope_attrs = scope_log
                .pointer("/scope/attributes")
                .and_then(Value::as_array)
                .map(|values| attribute_map(values))
                .unwrap_or_default();
            let Some(log_records) = scope_log.get("logRecords").and_then(Value::as_array) else {
                continue;
            };
            for record in log_records {
                let mut attributes = resource_attrs.clone();
                attributes.extend(scope_attrs.clone());
                if let Some(values) = record.get("attributes").and_then(Value::as_array) {
                    attributes.extend(attribute_map(values));
                }
                records.push(LogRecord {
                    body: record
                        .pointer("/body/stringValue")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    time_unix_nano: record.get("timeUnixNano").and_then(record_time_unix_nano),
                    attributes,
                    scope_name: scope_name.clone(),
                });
            }
        }
    }
    records
}

fn attribute_map(values: &[Value]) -> HashMap<String, Value> {
    values
        .iter()
        .filter_map(|attribute| {
            Some((
                attribute.get("key")?.as_str()?.to_string(),
                attribute.get("value")?.clone(),
            ))
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
