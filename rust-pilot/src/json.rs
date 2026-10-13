//! ECMAScript object enumeration and the shared canonical harness format.
use serde_json::Value;
use std::cmp::Ordering;

fn array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}
fn index_order(a: &str, b: &str) -> Ordering {
    match (array_index(a), array_index(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
fn order(value: &mut Value, canonical: bool) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|v| order(v, canonical)),
        Value::Object(map) => {
            let mut entries: Vec<_> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| {
                index_order(a, b).then_with(|| {
                    if canonical {
                        a.encode_utf16().cmp(b.encode_utf16())
                    } else {
                        Ordering::Equal
                    }
                })
            });
            for (key, mut value) in entries {
                order(&mut value, canonical);
                map.insert(key, value);
            }
        }
        _ => {}
    }
}
pub fn enumeration_order(value: &mut Value) {
    order(value, false);
}
pub fn canonical(value: &Value) -> String {
    let mut value = value.clone();
    order(&mut value, true);
    let mut out = String::new();
    write_json(&value, &mut out);
    out
}

fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Number(number) => {
            let number = number.as_f64().expect("JSON number");
            if number == 0.0 {
                out.push('0');
            } else {
                out.push_str(ryu_js::Buffer::new().format(number));
            }
        }
        Value::Array(values) => {
            out.push('[');
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(v, out);
            }
            out.push(']');
        }
        Value::Object(values) => {
            out.push('{');
            for (i, (key, v)) in values.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap());
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}
fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(v) => v.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let s = crate::text::js_trim(s);
            if s.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(digits) = s.strip_prefix(prefix) {
                    if digits.is_empty() {
                        return f64::NAN;
                    }
                    return digits
                        .chars()
                        .try_fold(0.0, |n, c| {
                            c.to_digit(radix).map(|d| n * radix as f64 + d as f64)
                        })
                        .unwrap_or(f64::NAN);
                }
            }
            if s == "Infinity" || s == "+Infinity" {
                return f64::INFINITY;
            }
            if s == "-Infinity" {
                return f64::NEG_INFINITY;
            }
            if !crate::text::test(
                r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$",
                s,
            ) {
                return f64::NAN;
            }
            s.parse().unwrap_or(f64::NAN)
        }
        Value::Array(v) if v.is_empty() => 0.0,
        Value::Array(v) if v.len() == 1 => match &v[0] {
            Value::Null => 0.0,
            Value::Bool(_) | Value::Object(_) => f64::NAN,
            other => to_number(other),
        },
        _ => f64::NAN,
    }
}
pub fn has_positive_length(value: &Value) -> bool {
    match value {
        Value::Array(v) => !v.is_empty(),
        Value::String(v) => !v.is_empty(),
        Value::Object(v) => v.get("length").is_some_and(|n| to_number(n) > 0.0),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_keeps_insertion_order_except_array_indices() {
        let mut v: Value =
            serde_json::from_str(r#"{"z":0,"10":0,"2":0,"a":0,"02":0,"4294967295":0}"#).unwrap();
        enumeration_order(&mut v);
        assert_eq!(
            v.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["2", "10", "z", "a", "02", "4294967295"]
        );
    }
    #[test]
    fn canonical_uses_numeric_then_utf16_keys() {
        let v: Value = serde_json::from_str(r#"{"ｚ":0,"😀":0,"10":0,"2":0}"#).unwrap();
        assert_eq!(canonical(&v), r#"{"2":0,"10":0,"😀":0,"ｚ":0}"#);
    }
}

/// A JSON string can contain unpaired UTF-16 units which Rust String cannot hold.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct JsonString(pub Vec<u16>);
impl<'de> serde::Deserialize<'de> for JsonString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl serde::de::Visitor<'_> for Bytes {
            type Value = JsonString;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON string")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                // serde_json's byte deserializer preserves surrogates as WTF-8.
                let mut units = Vec::new();
                let mut i = 0;
                while i < bytes.len() {
                    let first = bytes[i];
                    let (width, mut cp) = if first < 128 {
                        (1, first as u32)
                    } else if first < 224 {
                        (2, (first & 31) as u32)
                    } else if first < 240 {
                        (3, (first & 15) as u32)
                    } else {
                        (4, (first & 7) as u32)
                    };
                    for &b in &bytes[i + 1..i + width] {
                        cp = (cp << 6) | (b & 63) as u32;
                    }
                    if cp <= 0xffff {
                        units.push(cp as u16);
                    } else {
                        let cp = cp - 0x10000;
                        units.extend([0xd800 + (cp >> 10) as u16, 0xdc00 + (cp & 1023) as u16]);
                    }
                    i += width;
                }
                Ok(JsonString(units))
            }
        }
        deserializer.deserialize_bytes(Bytes)
    }
}
impl JsonString {
    fn write(&self, out: &mut String) {
        out.push('"');
        let mut scalar = String::new();
        let flush = |scalar: &mut String, out: &mut String| {
            let encoded = serde_json::to_string(scalar).unwrap();
            out.push_str(&encoded[1..encoded.len() - 1]);
            scalar.clear();
        };
        for c in char::decode_utf16(self.0.iter().copied()) {
            match c {
                Ok(c) => scalar.push(c),
                Err(e) => {
                    flush(&mut scalar, out);
                    use std::fmt::Write;
                    write!(out, "\\u{:04x}", e.unpaired_surrogate()).unwrap();
                }
            }
        }
        flush(&mut scalar, out);
        out.push('"');
    }
    fn index(&self) -> Option<u32> {
        array_index(&String::from_utf16(&self.0).ok()?)
    }
}
pub type RawObject = indexmap::IndexMap<JsonString, Box<serde_json::value::RawValue>>;

/// Canonicalize serialized values without forcing JSON strings through UTF-8.
pub fn canonical_text(text: &str) -> serde_json::Result<String> {
    let value: Box<serde_json::value::RawValue> = serde_json::from_str(text)?;
    let mut out = String::new();
    write_raw(value.get(), &mut out)?;
    Ok(out)
}
fn write_raw(text: &str, out: &mut String) -> serde_json::Result<()> {
    match text.as_bytes()[0] {
        b'{' => {
            let map: RawObject = serde_json::from_str(text)?;
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| match (a.index(), b.index()) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => a.0.cmp(&b.0),
            });
            out.push('{');
            for (i, (key, value)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                key.write(out);
                out.push(':');
                write_raw(value.get(), out)?;
            }
            out.push('}');
        }
        b'[' => {
            let values: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(text)?;
            out.push('[');
            for (i, value) in values.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_raw(value.get(), out)?;
            }
            out.push(']');
        }
        b'"' => serde_json::from_str::<JsonString>(text)?.write(out),
        b'n' | b't' | b'f' => out.push_str(text),
        _ => {
            let number: f64 = text.parse().expect("validated JSON number");
            if !number.is_finite() {
                out.push_str("null");
            } else if number == 0.0 {
                out.push('0');
            } else {
                out.push_str(ryu_js::Buffer::new().format(number));
            }
        }
    }
    Ok(())
}
pub fn raw_has_positive_length(text: &str) -> bool {
    use serde_json::value::RawValue;
    let text = text.trim_start();
    match text.as_bytes().first() {
        Some(b'"') => serde_json::from_str::<JsonString>(text).is_ok_and(|s| !s.0.is_empty()),
        Some(b'[') => serde_json::from_str::<Vec<Box<RawValue>>>(text).is_ok_and(|v| !v.is_empty()),
        Some(b'{') => serde_json::from_str::<RawObject>(text)
            .ok()
            .and_then(|mut v| v.swap_remove(&JsonString("length".encode_utf16().collect())))
            .is_some_and(|v| raw_number(v.get()) > 0.0),
        _ => false,
    }
}
fn raw_number(text: &str) -> f64 {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return to_number(&value);
    }
    if let Ok(values) = serde_json::from_str::<Vec<Box<serde_json::value::RawValue>>>(text)
        && values.len() == 1
    {
        return raw_number(values[0].get());
    }
    text.parse().unwrap_or(f64::NAN)
}
