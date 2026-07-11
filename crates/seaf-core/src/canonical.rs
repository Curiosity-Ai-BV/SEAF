use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(value)?;
    sort_object_keys(&mut value);
    serde_json::to_vec_pretty(&value)
}

pub fn canonical_sha256_digest<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let canonical = canonical_json_bytes(value)?;
    Ok(hex::encode(Sha256::digest(canonical)))
}

fn sort_object_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = std::mem::take(map).into_iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (key, mut value) in entries {
                sort_object_keys(&mut value);
                map.insert(key, value);
            }
        }
        Value::Array(items) => {
            for item in items {
                sort_object_keys(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}
