mod decode;
mod peer;
mod torrent_file;
mod tracker;
mod utils;
mod worker;

use std::{collections::HashMap, net::Ipv4Addr, sync::Arc};

use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};

use decode::decode_bencoded_value;
use sha1::{Digest, Sha1};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use torrent_file::{parse_torrent_file, Torrent};
use tracker::TrackerRequest;
use worker::{PiecesQueue, Worker};

use crate::peer::{MsgFramed, PeerMessage, Piece, Request, Tag};

const PEER_ID: &str = "10112233445366778898";

// following specs
const BLOCK_SIZE: usize = 1 << 14;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    sub_cmd: SubCommand,
}

#[derive(Subcommand, Debug)]
#[clap(rename_all = "snake_case")]
enum SubCommand {
    Decode {
        /// The encoded data to be decoded
        encoded: String,
    },
    Info {
        /// The encoded data to be decoded
        path: String,
    },
    Peers {
        /// The encoded data to be decoded
        path: String,
    },
    Handshake {
        path: String,
        peer: String,
    },
    DownloadPiece {
        #[arg(short)]
        out_path: String,
        torrent: String,
        piece_i: usize,
    },

    Download {
        #[arg(short)]
        out_path: String,
        torrent: String,
    },
}
#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.sub_cmd {
        SubCommand::Decode { encoded } => {
            // print for testing purpose
            let decoded_value = decode_bencoded_value(encoded.as_bytes())?;
            println!("{}", decoded_value);
        }
        SubCommand::Info { path } => {
            let torrent_info = parse_torrent_file(path)?;
            // print for testing purpose
            println!("Tracker URL: {}", torrent_info.announce);
            println!(
                "Length: {}",
                torrent_info
                    .info
                    .file_length()
                    .expect("Multifile not implemented yet")
            );
            let info_hash = torrent_info.info_hash()?;
            println!("Info Hash: {}", hex::encode(info_hash));
            println!("Piece Length: {}", torrent_info.info.piece_length);
            println!("Piece Hashes:");
            torrent_info
                .piece_hashes()
                .into_iter()
                .for_each(|hash| println!("{hash}"));
        }

        SubCommand::Peers { path } => {
            let torrent_info = parse_torrent_file(path)?;

            let peers = peers(&torrent_info, PEER_ID).await?;

            for (ip, port) in peers {
                println!("{}:{}", ip, port);
            }
        }

        SubCommand::Handshake { path, peer } => {
            let torrent_info = parse_torrent_file(path)?;
            let info_hash = torrent_info.info_hash()?;

            // Create a handshake message, when send, the response will be written directly into
            // the same handshake struct.
            let mut handshake = peer::Handshake::new(info_hash, *b"10112233445366778898");
            handshake.send(&peer).await?;

            println!("Peer ID: {}", hex::encode(handshake.peer_id));
        }
        SubCommand::DownloadPiece {
            out_path,
            torrent,
            piece_i,
        } => {
            let torrent_info = parse_torrent_file(torrent)?;
            eprintln!(
                "Downloading piece: {} of file: {}",
                piece_i, torrent_info.info.name
            );

            let info_hash = torrent_info.info_hash()?;

            let file_len = torrent_info
                .info
                .file_length()
                .expect("Multifile not implemented yet");

            // check that the requested piece is valid
            if piece_i > torrent_info.num_pieces() {
                return Err(anyhow::anyhow!("Piece_i {} does not exist", piece_i));
            }

            let peers = peers(&torrent_info, PEER_ID).await?;

            let peer_addr = format!("{}:{}", peers[0].0, peers[0].1);

            // Create a handshake message, when send, the response will be written directly into
            // the same handshake struct.
            let mut handshake = peer::Handshake::new(info_hash, *b"10112233445366778898");
            let peer = handshake.send(&peer_addr).await?;
            eprintln!("handshake done!");

            // gets connected to a TCP stream and use it to parse responses into frames.
            let mut peer = tokio_util::codec::Framed::new(peer, MsgFramed);
            let bitfield = peer.next().await.expect("Unexpected peer message")?;

            assert_eq!(bitfield.tag, Tag::Bitfield);

            // now lets send the interested message!
            peer.send(PeerMessage {
                tag: Tag::Interested,
                payload: Vec::new(),
            })
            .await?;
            eprintln!("Interested done!");

            // choke message
            let unchoke = peer.next().await.expect("Unexpected peer msga")?;
            eprintln!("Unchoke done!");

            assert_eq!(unchoke.tag, Tag::Unchoke);
            assert!(unchoke.payload.is_empty());

            let num_pieces = torrent_info.num_pieces();
            let piece_hash = torrent_info.info.pieces[piece_i];

            let piece_size = if piece_i == num_pieces - 1 {
                // here we need to check if piece length is equal to max piece value, or not.
                let base_len = file_len % torrent_info.info.piece_length;
                if base_len == 0 {
                    torrent_info.info.piece_length
                } else {
                    base_len
                }
            } else {
                torrent_info.info.piece_length
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

                peer.send(PeerMessage {
                    tag: Tag::Request,
                    payload: request.as_bytes().to_vec(),
                })
                .await?;
                eprintln!("Request done!");

                // now read response
                let piece = peer.next().await.expect("Unexpected peer message")?;
                eprintln!("Request response done!");
                // assert_eq!(piece.tag, Tag::Piece);
                assert!(!piece.payload.is_empty());

                let piece = Piece::ref_from_payload(&piece.payload)
                    .ok_or(anyhow::anyhow!("Invalid piece from peer"))?;
                assert_eq!(piece.index as usize, piece_i);
                assert_eq!(piece.begin as usize, block * BLOCK_SIZE);
                assert_eq!(piece.piece.len(), block_size);

                // now extend our data
                piece_data.extend_from_slice(piece.piece);
            }
            assert_eq!(piece_data.len(), piece_size);
            // now store into a file
            // first check that hash matches which means
            // data integrity
            let mut hasher = Sha1::new();
            hasher.update(&piece_data);
            let hash: [u8; 20] = hasher.finalize().try_into().expect("Should not happen");
            assert_eq!(hash, piece_hash);

            tokio::fs::write(&out_path, piece_data).await?;
            println!("Piece {} downloaded to {}.", piece_i, out_path);
        }

        SubCommand::Download { out_path, torrent } => {
            let torrent = Arc::new(parse_torrent_file(torrent)?);
            let peers = peers(&torrent, PEER_ID)
                .await?
                .into_iter()
                .map(|(ip, port)| format!("{}:{}", ip, port))
                .collect::<Vec<String>>();

            let pieces_queue = PiecesQueue::new(0..torrent.num_pieces());

            let mut rx = {
                // Bounded channel which can hold up to num_pieces chunks
                let (tx, rx) = tokio::sync::mpsc::channel::<(usize, Vec<u8>)>(torrent.num_pieces());

                // spawn tasks
                for peer in peers.into_iter() {
                    let torrent = torrent.clone();
                    let tx = tx.clone();
                    let queue = pieces_queue.clone();

                    tokio::spawn(async move {
                        let worker = Worker::new(queue, torrent, peer, tx);
                        _ = worker.start().await;
                    });
                }
                rx
            };

            let mut map = HashMap::new();

            while let Some((piece_i, piece_data)) = rx.recv().await {
                if map.insert(piece_i, piece_data).is_some() {
                    return Err(anyhow::anyhow!("Unexpected repeated piece_i: {}", piece_i));
                }
            }

            if map.len() != torrent.num_pieces() {
                return Err(anyhow::anyhow!(
                    "Missing pieces got: {} but require: {}",
                    map.len(),
                    torrent.num_pieces(),
                ));
            }

            write_pieces(&map, &out_path).await?;

            println!("Downloaded {} to {}.", torrent.info.name, out_path);
        }
    }

    Ok(())
}

pub async fn peers(torrent: &Torrent, peer_id: &str) -> anyhow::Result<Vec<(Ipv4Addr, u16)>> {
    let info_hash = torrent.info_hash()?;
    let file_len = torrent
        .info
        .file_length()
        .ok_or(anyhow::anyhow!("Multifile not implemented yet"))?;

    let request = TrackerRequest::new(info_hash, peer_id, file_len as _);
    let response = request.send_request(&torrent.announce).await?;
    response.peers()
}

pub async fn write_pieces(chunks: &HashMap<usize, Vec<u8>>, file_path: &str) -> anyhow::Result<()> {
    let mut file = File::create(file_path).await?;
    // Write each chunk in order, we can do this as the keys are consecutive
    // and goes from 0..num_pieces
    for key in 0..chunks.len() {
        if let Some(chunk) = chunks.get(&key) {
            file.write_all(chunk).await?;
        }
    }

    file.flush().await?;

    Ok(())
}
