use std::path::Path;

use crate::utils::{deserialize_pieces, serialize_pieces};

pub const SHA1_LEN: usize = 20;

use serde::{Deserialize, Serialize};

// lenght of each piece hash in pieces field
// according to spec, each one is 20-bytes len
pub const PIECES_HASH_LEN: usize = SHA1_LEN;

#[derive(Debug, Deserialize, Serialize)]
pub struct Torrent {
    pub announce: String,
    #[serde(rename = "created by")]
    created_by: Option<String>,
    pub info: Info,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
    pub name: String,
    #[serde(rename = "piece length")]
    pub piece_length: usize,
    #[serde(
        deserialize_with = "deserialize_pieces",
        serialize_with = "serialize_pieces"
    )]
    pub pieces: Vec<[u8; PIECES_HASH_LEN]>,

    #[serde(flatten)]
    pub keys: Keys,
}

impl Info {
    pub fn file_length(&self) -> Option<usize> {
        if let Keys::SingleFile { length } = self.keys {
            Some(length)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Keys {
    SingleFile { length: usize },
    MultiFile { files: Vec<File> },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct File {
    pub length: usize,
    pub path: Vec<String>,
}

impl Torrent {
    pub fn info_hash(&self) -> anyhow::Result<[u8; SHA1_LEN]> {
        use sha1::{Digest, Sha1};

        let info = serde_bencode::to_bytes(&self.info)?;

        let mut hasher = Sha1::new();
        hasher.update(&info);
        let result = hasher.finalize().try_into()?;

        Ok(result)
    }

    pub fn piece_hashes(&self) -> Vec<String> {
        self.info
            .pieces
            .iter()
            .map(hex::encode)
            .collect::<Vec<String>>()
    }

    pub fn num_pieces(&self) -> usize {
        self.info.pieces.len()
    }
}

pub fn parse_torrent_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Torrent> {
    use std::fs;

    let data = fs::read(path)?;

    Ok(serde_bencode::from_bytes(&data)?)
}
