use std::mem::size_of;

use crate::torrent_file::SHA1_LEN;
use bytes::{Buf, BufMut, BytesMut};
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::TcpStream};
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;

const PROTOCOL: &[u8; 19] = b"BitTorrent protocol";

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Handshake {
    pub lenght: u8,
    pub protocol: [u8; 19],
    pub reserve: [u8; 8],
    pub info_hash: [u8; SHA1_LEN],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; SHA1_LEN], peer_id: [u8; 20]) -> Self {
        Self {
            lenght: PROTOCOL.len() as u8,
            protocol: PROTOCOL.to_owned(),
            reserve: Default::default(),
            info_hash,
            peer_id,
        }
    }

    pub fn as_mut_bytes(&mut self) -> &mut [u8; std::mem::size_of::<Self>()] {
        unsafe { std::mem::transmute(self) }
    }

    pub async fn send(&mut self, peer: &str) -> anyhow::Result<TcpStream> {
        let bytes = self.as_mut_bytes();

        let mut tcp_client = TcpStream::connect(&peer).await?;
        {
            tcp_client.write_all(bytes).await?;
        }
        {
            tcp_client.read_exact(&mut bytes[..]).await?;
        }

        Ok(tcp_client)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Request {
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

impl Request {
    pub fn as_bytes(&self) -> [u8; size_of::<Self>()] {
        let mut bytes = [0u8; size_of::<Request>()];
        bytes[0..4].copy_from_slice(&self.index.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.begin.to_be_bytes());
        bytes[8..12].copy_from_slice(&self.length.to_be_bytes());
        bytes
    }
}

#[derive(Debug, Clone)]
#[repr(C)]
pub struct Piece<'a> {
    pub index: u32,
    pub begin: u32,
    pub piece: &'a [u8],
}

impl<'a> Piece<'a> {
    const INDEX_SIZE: usize = std::mem::size_of::<u32>();
    const BEGIN_SIZE: usize = std::mem::size_of::<u32>();

    pub fn ref_from_payload(data: &'a [u8]) -> Option<Self> {
        if data.len() < Self::INDEX_SIZE + Self::BEGIN_SIZE {
            return None;
        }

        let index = u32::from_be_bytes(data[0..Self::INDEX_SIZE].try_into().ok()?);
        let begin = u32::from_be_bytes(
            data[Self::INDEX_SIZE..(Self::INDEX_SIZE + Self::BEGIN_SIZE)]
                .try_into()
                .ok()?,
        );

        // Everything that follows the first two u32 fields is the piece.
        let piece = &data[(Self::INDEX_SIZE + Self::BEGIN_SIZE)..];

        Some(Piece {
            index,
            begin,
            piece,
        })
    }
}
#[derive(Debug, Clone, PartialEq)]
#[repr(u8)]
pub enum Tag {
    _Choke = 0,
    Unchoke = 1,
    Interested = 2,
    _NotInterested = 3,
    _Have = 4,
    Bitfield = 5,
    Request = 6,
    _Cancel = 7,
    Piece = 8,
}

impl TryFrom<u8> for Tag {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > Tag::Piece as u8 {
            return Err(anyhow::anyhow!("Value of {} is not a valid Tag", value));
        }

        // Safety: We check value is in range
        Ok(unsafe { std::mem::transmute::<_, Self>(value) })
    }
}

#[derive(Debug, Clone)]
#[repr(C)]
pub struct PeerMessage {
    pub tag: Tag,
    pub payload: Vec<u8>,
}

impl PeerMessage {
    pub const MAX: usize = 1 << 16;
}

pub struct MsgFramed;

impl Decoder for MsgFramed {
    type Item = PeerMessage;

    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // check we have length prefix
        if src.len() < 4 {
            // tell frame we need more data
            return Ok(None);
        }

        let mut length = [0; 4];
        length.copy_from_slice(&src[..4]);

        let length = u32::from_be_bytes(length) as usize;

        // heart bit message
        if length == 0 {
            src.advance(4);
            return self.decode(src);
        }

        // check for tag at least
        if src.len() < 5 {
            return Ok(None);
        }

        // check for max limit to align with the especification
        if length > PeerMessage::MAX {
            return Err(anyhow::anyhow!("Frame of len {} is too large", length));
        }

        // check src holds all the bytes we need or continue
        if src.len() < 4 + length {
            // lets optimaize by trying to reserve the missing bytes
            src.reserve((4 + length) - src.len());
            return Ok(None);
        }

        // lets parse a complete frame, we do have either a complete frame or 1 and more.
        let tag = Tag::try_from(src[4])?;

        // now get the payload
        let data = if src.len() > 5 {
            src[5..4 + length].to_vec()
        } else {
            Vec::new()
        };
        src.advance(4 + length);

        Ok(Some(PeerMessage { tag, payload: data }))
    }
}

impl Encoder<PeerMessage> for MsgFramed {
    type Error = anyhow::Error;

    fn encode(&mut self, item: PeerMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // check this message is bellow limits
        if item.payload.len() + 1 > PeerMessage::MAX {
            return Err(anyhow::anyhow!(
                "Message of len {} is too large",
                item.payload.len() + 1
            ));
        }

        // lets reserve space in output buffer
        // 4-bytes length prefix
        // 1-byte tag
        // n-bytes payload size
        dst.reserve(4 + 1 + item.payload.len());

        // 4-bytes prefixed length + 1-byte tag + payload
        let len = 1 + item.payload.len();
        let len_bytes = u32::to_be_bytes(len as u32);

        dst.extend_from_slice(&len_bytes[..]);
        dst.put_u8(item.tag as u8);
        dst.extend_from_slice(&item.payload[..]);

        Ok(())
    }
}
