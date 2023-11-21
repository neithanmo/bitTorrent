use std::net::Ipv4Addr;

use anyhow::Context;

use crate::{torrent_file::SHA1_LEN, utils::deserialize_tracker_response};

pub const TRACKER_PORT: u16 = 6881;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct TrackerRequest {
    #[serde(skip_serializing)]
    pub info_hash: [u8; SHA1_LEN],
    pub peer_id: String,
    pub port: u16,
    pub uploaded: usize,
    pub downloaded: usize,
    pub left: usize,
    pub compact: u8,
}

#[derive(Debug, serde::Deserialize)]
pub struct TrackerResponse {
    pub interval: Option<u64>,
    #[serde(deserialize_with = "deserialize_tracker_response")]
    pub peers: Vec<u8>, // TODO: better check at deserializing stage that peers.len() % 6 == 0
}

impl TrackerRequest {
    pub fn new(info_hash: [u8; SHA1_LEN], peer_id: &str, left: usize) -> Self {
        Self {
            info_hash,
            peer_id: peer_id.to_owned(),
            port: crate::tracker::TRACKER_PORT,
            uploaded: 0,
            downloaded: 0,
            left,
            compact: 1,
        }
    }

    pub async fn send_request(&self, url: &str) -> anyhow::Result<TrackerResponse> {
        let url_params =
            serde_urlencoded::to_string(self).context("url-encode tracker parameters")?;

        let tracker_url = format!(
            "{}?{}&info_hash={}",
            url,
            url_params,
            &crate::utils::urlencode(&self.info_hash)
        );

        let client = reqwest::Client::new();
        let res = client.get(tracker_url).send().await?;

        if !res.status().is_success() {
            return Err(anyhow::anyhow!("Request error: {:?}", res.status()));
        }

        let text = res.bytes().await?;

        serde_bencode::from_bytes(&text).map_err(|e| anyhow::anyhow!(e))
    }
}

impl TrackerResponse {
    pub fn peers(&self) -> anyhow::Result<Vec<(Ipv4Addr, u16)>> {
        if self.peers.len() % 6 != 0 {
            return Err(anyhow::anyhow!("Invalid peers list"));
        }

        // The first 4-bytes are the peer's ip address
        // and the last two are the port.
        // They use the big-endian representation?
        let peers = self
            .peers
            .chunks_exact(6)
            .map(|bytes| {
                let ip = Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
                let port = u16::from_be_bytes([bytes[4], bytes[5]]);
                (ip, port)
            })
            .collect::<Vec<_>>();

        Ok(peers)
    }
}
