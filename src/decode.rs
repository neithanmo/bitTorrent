use std::collections::HashMap;

use anyhow::Result as AnyResult;
use serde_bencode::value::Value as BencValue;
use serde_json::Value as JsonValue;

pub(crate) fn decode_bencoded_value(encoded_value: &[u8]) -> AnyResult<JsonValue> {
    let value = serde_bencode::de::from_bytes::<BencValue>(encoded_value)?;

    decode_value(value)
}

fn decode_value(value: BencValue) -> AnyResult<JsonValue> {
    match value {
        BencValue::Int(i) => Ok(JsonValue::Number(i.into())),
        BencValue::Bytes(bytes) => {
            // bytes could be either a string or just raw bytes
            // handle both cases
            if let Ok(s) = String::from_utf8(bytes.clone()) {
                Ok(s.into())
            } else {
                Ok(serde_json::from_slice(&bytes)?)
            }
        }
        BencValue::List(list) => decode_list(list),
        BencValue::Dict(map) => decode_dict(map),
    }
}

fn decode_list(encoded: Vec<BencValue>) -> AnyResult<JsonValue> {
    let mut vec = vec![];
    for v in encoded.into_iter() {
        vec.push(decode_value(v)?);
    }

    Ok(JsonValue::Array(vec))
}

fn decode_dict(encoded: HashMap<Vec<u8>, BencValue>) -> AnyResult<JsonValue> {
    let mut map = serde_json::map::Map::new();

    for (key, val) in encoded.into_iter() {
        let key = String::from_utf8(key)?;
        let value = decode_value(val)?;
        map.insert(key, value);
    }

    Ok(JsonValue::Object(map))
}
