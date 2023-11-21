use crate::peer::{Handshake, MsgFramed, PeerMessage, Piece, Request, Tag};
use crate::torrent_file::Torrent;
use crate::{BLOCK_SIZE, PEER_ID};

use futures_util::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::ops::Range;
use std::sync::Arc;
use std::{collections::vec_deque::VecDeque, sync::Mutex};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tokio_util::codec::Framed;

#[derive(Clone, Debug)]
pub struct PiecesQueue(Arc<Mutex<VecDeque<usize>>>);

impl PiecesQueue {
    pub fn new(pieces: Range<usize>) -> Self {
        let queue = pieces.collect::<VecDeque<usize>>();
        let pieces = Arc::new(Mutex::new(queue));
        Self(pieces)
    }

    pub fn take_piece(&self) -> Option<usize> {
        self.0.lock().expect("Poisson state!?").pop_front()
    }

    pub fn push_piece(&self, piece: usize) {
        self.0.lock().expect("Poissoned state").push_back(piece)
    }
}

pub struct Worker {
    torrent: Arc<Torrent>,
    queue: PiecesQueue,
    peer: String,
    result: Sender<(usize, Vec<u8>)>,
}

impl Worker {
    pub fn new(
        queue: PiecesQueue,
        torrent: Arc<Torrent>,
        peer: String,
        result: Sender<(usize, Vec<u8>)>,
    ) -> Self {
        Self {
            torrent,
            queue,
            peer,
            result,
            // shutdown,
        }
    }

    async fn connect(&self) -> anyhow::Result<TcpStream> {
        let info_hash = self.torrent.info_hash()?;
        let mut handshake = Handshake::new(info_hash, PEER_ID.as_bytes().try_into()?);
        let stream = handshake.send(&self.peer).await?;

        Ok(stream)
    }

    async fn initialize_frame(
        &self,
        stream: TcpStream,
    ) -> anyhow::Result<Framed<TcpStream, MsgFramed>> {
        // gets connected to a TCP stream and use it to parse responses into frames.
        let mut frame = tokio_util::codec::Framed::new(stream, MsgFramed);

        // initialize messages with peer
        let bitfield = frame.next().await.expect("Unexpected peer message")?;

        assert_eq!(bitfield.tag, Tag::Bitfield);

        // now lets send the interested message!
        frame
            .send(PeerMessage {
                tag: Tag::Interested,
                payload: Vec::new(),
            })
            .await?;

        // choke message
        let unchoke = frame.next().await.expect("Unexpected peer msga")?;

        assert_eq!(unchoke.tag, Tag::Unchoke);
        assert!(unchoke.payload.is_empty());

        Ok(frame)
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        // first connect to a node
        let stream = self.connect().await?;
        let mut frame = self.initialize_frame(stream).await?;

        let file_len = self
            .torrent
            .info
            .file_length()
            .ok_or(anyhow::anyhow!("Multifile not implemented yet"))?;

        let num_pieces = self.torrent.num_pieces();

        'start: loop {
            // get piece
            let Some(piece_i) = self.queue.take_piece() else {
                println!("no more pieces, exiting");
                // we are done no more pieces at this time
                break;
            };

            println!("Downloading piece: {} ", piece_i);

            let piece_hash = self.torrent.info.pieces[piece_i];

            let piece_size = if piece_i == num_pieces - 1 {
                // here we need to check if piece length is equal to max piece value, or not.
                let base_len = file_len % self.torrent.info.piece_length;
                if base_len == 0 {
                    self.torrent.info.piece_length
                } else {
                    base_len
                }
            } else {
                self.torrent.info.piece_length
            };

            // we need to figure out what block it is, for that we have:
            // piece_i
            // num_pieces
            // piece_size
            // blocks start at 0, that it is why the -1,
            // if piece_i = 0, them we will have 2B/1B -> 1 - 1 = 0;
            let n_blocks = (piece_size + (BLOCK_SIZE - 1)) / BLOCK_SIZE;
            let mut piece_data = Vec::with_capacity(piece_size);

            // now need to set index, begin, length.
            // index = piece_i
            // begin would depend of n_block
            for block in 0..n_blocks {
                let block_size = if block == n_blocks - 1 {
                    // it turns out this piece is equal to max_block_len?
                    let base_len = piece_size % BLOCK_SIZE;
                    if base_len == 0 {
                        BLOCK_SIZE
                    } else {
                        base_len
                    }
                } else {
                    BLOCK_SIZE
                };

                let request = Request {
                    index: piece_i as u32,
                    begin: (block * BLOCK_SIZE) as u32,
                    length: block_size as u32,
                };

                if frame
                    .send(PeerMessage {
                        tag: Tag::Request,
                        payload: request.as_bytes().to_vec(),
                    })
                    .await
                    .is_err()
                {
                    self.queue.push_piece(piece_i);
                    break 'start;
                }

                // now read response
                let Some(Ok(piece)) = frame.next().await else {
                    self.queue.push_piece(piece_i);
                    break;
                };

                if piece.payload.is_empty() {
                    self.queue.push_piece(piece_i);
                    break 'start;
                }
                // assert_eq!(piece.tag, Tag::Piece);

                let Some(piece) = Piece::ref_from_payload(&piece.payload) else {
                    self.queue.push_piece(piece_i);
                    break 'start;
                } ;

                if piece.index as usize != piece_i
                    || piece.begin as usize != (block * BLOCK_SIZE)
                    || piece.piece.len() != block_size
                {
                    // we downloaded an invalid piece
                    self.queue.push_piece(piece_i);
                    break 'start;
                }

                // now extend our data
                piece_data.extend_from_slice(piece.piece);
            }

            if piece_data.len() != piece_size {
                self.queue.push_piece(piece_i);
                break 'start;
            }

            let mut hasher = Sha1::new();
            hasher.update(&piece_data);

            let hash: Result<[u8; 20], _> = hasher.finalize().try_into();
            let Ok(hash) = hash else {
                self.queue.push_piece(piece_i);
                break 'start;
            };

            if hash != piece_hash {
                self.queue.push_piece(piece_i);
                break 'start;
            }

            // This will errors only if receiver was closed before.
            // so no need to push unsuccesful piece id
            self.result.send((piece_i, piece_data)).await?;
        }

        Ok(())
    }
}

