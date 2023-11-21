use serde::de::{Deserialize, Deserializer, Error as DeError};
use serde::Serializer;

use serde_bytes::ByteBuf;

use crate::torrent_file::PIECES_HASH_LEN;

// // This function is a custom deserializer. It uses serde_bytes to interpret the data as raw bytes, not a UTF-8 string.
// pub fn deserialize_bytes<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
// where
//     D: Deserializer<'de>,
// {
//     let buf: ByteBuf = Deserialize::deserialize(deserializer)?;
//     Ok(buf.into_vec())
// }
//
// pub fn serialize_bytes<S>(pieces: &[u8], serializer: S) -> Result<S::Ok, S::Error>
// where
//     S: Serializer,
// {
//     serializer.serialize_bytes(pieces)
// }

// Custom Pieces deserializer, converts Vec<u8> into Vec<[u8; PIECES_HASH_LEN]>
// being each element a piece hash
pub fn deserialize_pieces<'de, D>(deserializer: D) -> Result<Vec<[u8; PIECES_HASH_LEN]>, D::Error>
where
    D: Deserializer<'de>,
{
    let buf: ByteBuf = Deserialize::deserialize(deserializer)?;

    if buf.len() % PIECES_HASH_LEN != 0 {
        return Err(D::Error::custom(format!(
            "Invalid peers response len: {}",
            buf.len()
        )));
    }

    buf.chunks_exact(PIECES_HASH_LEN)
        .map(|chunk| {
            let mut array = [0u8; PIECES_HASH_LEN];
            array.copy_from_slice(chunk);
            Ok(array)
        })
        .collect::<Result<Vec<_>, D::Error>>()
}

// This Vec<[u8; PIECES_HASH_LEN]> -> Vec<u8>
pub fn serialize_pieces<S>(
    pieces: &[[u8; PIECES_HASH_LEN]],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let flattened: Vec<u8> = pieces.iter().flatten().copied().collect();
    serializer.serialize_bytes(&flattened)
}

// This function is a custom deserializer. It uses serde_bytes to interpret the data as raw bytes, not a UTF-8 string.
pub fn deserialize_tracker_response<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let buf: ByteBuf = Deserialize::deserialize(deserializer)?;
    if buf.len() % 6 != 0 {
        return Err(D::Error::custom(format!(
            "Invalid peers response len: {}",
            buf.len()
        )));
    }
    Ok(buf.into_vec())
}

pub fn urlencode(t: &[u8]) -> String {
    let mut encoded = String::with_capacity(3 * t.len());
    for &byte in t {
        encoded.push('%');
        encoded.push_str(&hex::encode([byte]));
    }
    encoded
}
