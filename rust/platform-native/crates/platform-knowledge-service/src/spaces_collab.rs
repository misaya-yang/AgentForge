//! Hocuspocus-compatible Yjs sync for one space item.
//! Frames are `varstring(document) + y-sync message`. Ping is the single byte 9.
use ai_platform_knowledge_domain::spaces::{SpaceError, Spaces};
use axum::{
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use yrs::{
    encoding::write::Write,
    updates::{
        decoder::{Decode, DecoderV1},
        encoder::{Encode, Encoder, EncoderV1},
    },
    Doc, GetString, ReadTxn, Text, Transact, Update,
};
use yrs::sync::{
    awareness::Awareness,
    protocol::{DefaultProtocol, Message as SyncMessageFrame, Protocol, SyncMessage},
};

use crate::migration_knowledge_query::types::UserContext;

pub struct FrameEffect {
    pub replies: Vec<Vec<u8>>,
    pub broadcast: Option<Vec<u8>>,
    pub changed: bool,
}

pub struct CollabRoom {
    awareness: Mutex<Awareness>,
    peers: Mutex<Vec<mpsc::UnboundedSender<Vec<u8>>>>,
}

impl CollabRoom {
    pub fn from_state(bytes: &[u8]) -> Self {
        let doc = Doc::new();
        if !bytes.is_empty()
            && let Ok(update) = Update::decode_v1(bytes)
        {
            let mut txn = doc.transact_mut();
            let _ = txn.apply_update(update);
        }
        Self {
            awareness: Mutex::new(Awareness::new(doc)),
            peers: Mutex::new(Vec::new()),
        }
    }

    pub fn state(&self) -> Vec<u8> {
        let awareness = self.awareness.lock().unwrap_or_else(|poison| poison.into_inner());
        let txn = awareness.doc().transact();
        txn.encode_state_as_update_v1(&empty_state())
    }

    fn publish(&self, frame: &[u8], origin: &mpsc::UnboundedSender<Vec<u8>>) {
        let mut peers = self.peers.lock().unwrap_or_else(|poison| poison.into_inner());
        peers.retain(|peer| !peer.same_channel(origin) && peer.send(frame.to_vec()).is_ok());
    }
}

pub fn apply_frame(room: &CollabRoom, can_write: bool, frame: &[u8]) -> Result<FrameEffect, String> {
    if frame == [9] {
        return Ok(FrameEffect { replies: vec![vec![10]], broadcast: None, changed: false });
    }
    let (name, rest) = split_name(frame).ok_or_else(|| "invalid collab frame".to_owned())?;
    let (tag, tag_len) = read_var(rest).ok_or_else(|| "invalid collab tag".to_owned())?;
    if tag == 2 {
        return Ok(FrameEffect {
            replies: vec![auth_granted(name)],
            broadcast: None,
            changed: false,
        });
    }
    if !can_write && blocks_write(tag, &rest[tag_len..]) {
        return Ok(FrameEffect { replies: Vec::new(), broadcast: None, changed: false });
    }
    let mut awareness = room.awareness.lock().unwrap_or_else(|poison| poison.into_inner());
    let replies = DefaultProtocol
        .handle(&mut awareness, rest)
        .map_err(|error| error.to_string())?;
    drop(awareness);
    let mut encoded = Vec::new();
    for reply in replies {
        encoded.push(prefix(name, &reply));
    }
    let broadcast = if relays(tag, &rest[tag_len..]) { Some(frame.to_vec()) } else { None };
    let changed = can_write && tag == 0 && write_sync(&rest[tag_len..]);
    Ok(FrameEffect { replies: encoded, broadcast, changed })
}

fn blocks_write(tag: u64, after_tag: &[u8]) -> bool {
    if tag != 0 {
        return false;
    }
    write_sync(after_tag)
}

fn write_sync(after_tag: &[u8]) -> bool {
    matches!(read_var(after_tag).map(|(inner, _)| inner), Some(1 | 2))
}

fn relays(tag: u64, after_tag: &[u8]) -> bool {
    tag == 1 || (tag == 0 && matches!(read_var(after_tag).map(|(inner, _)| inner), Some(2)))
}

fn auth_granted(name: &str) -> Vec<u8> {
    let mut encoder = EncoderV1::new();
    encoder.write_string(name);
    encoder.write_var(2u64);
    encoder.write_var(2u64);
    encoder.write_string("read-write");
    encoder.to_vec()
}

fn prefix(name: &str, message: &SyncMessageFrame) -> Vec<u8> {
    let mut encoder = EncoderV1::new();
    encoder.write_string(name);
    message.encode(&mut encoder);
    encoder.to_vec()
}

fn split_name(bytes: &[u8]) -> Option<(&str, &[u8])> {
    let (len, offset) = read_var(bytes)?;
    let end = offset.checked_add(usize::try_from(len).ok()?)?;
    if end > bytes.len() {
        return None;
    }
    let name = std::str::from_utf8(&bytes[offset..end]).ok()?;
    Some((name, &bytes[end..]))
}

fn empty_state() -> yrs::StateVector {
    Doc::new().transact().state_vector()
}

fn read_var(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    for (index, byte) in bytes.iter().copied().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

struct CollabHub {
    rooms: AsyncMutex<HashMap<String, Arc<CollabRoom>>>,
}

impl CollabHub {
    async fn room(&self, spaces: &Spaces, user: &UserContext, item_id: &str) -> Result<Arc<CollabRoom>, SpaceError> {
        {
            let rooms = self.rooms.lock().await;
            if let Some(room) = rooms.get(item_id) {
                return Ok(room.clone());
            }
        }
        let bytes = spaces.load_ydoc(user, item_id).await?;
        let room = CollabRoom::from_state(&bytes);
        let mut rooms = self.rooms.lock().await;
        Ok(rooms.entry(item_id.to_owned()).or_insert_with(|| Arc::new(room)).clone())
    }
}

fn hub() -> &'static CollabHub {
    static HUB: OnceLock<CollabHub> = OnceLock::new();
    HUB.get_or_init(|| CollabHub { rooms: AsyncMutex::new(HashMap::new()) })
}

pub async fn connect(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let can_write = match spaces.can_edit(&user, &item_id).await {
        Ok(value) => value,
        Err(error) => return crate::spaces_http::fail(error),
    };
    let room = match hub().room(&spaces, &user, &item_id).await {
        Ok(room) => room,
        Err(error) => return crate::spaces_http::fail(error),
    };
    upgrade.on_upgrade(move |socket| session(socket, spaces, user, item_id, room, can_write))
}

async fn session(
    mut socket: WebSocket,
    spaces: Spaces,
    user: UserContext,
    item_id: String,
    room: Arc<CollabRoom>,
    can_write: bool,
) {
    let (origin, mut incoming) = mpsc::unbounded_channel();
    room.peers.lock().unwrap_or_else(|poison| poison.into_inner()).push(origin.clone());
    let mut dirty = false;
    loop {
        tokio::select! {
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                let Message::Binary(bytes) = message else { continue };
                let effect = match apply_frame(&room, can_write, &bytes) {
                    Ok(effect) => effect,
                    Err(error) => {
                        tracing::warn!(error, "space collab frame skipped");
                        continue;
                    }
                };
                let mut closed = false;
                for reply in effect.replies {
                    if socket.send(Message::Binary(reply.into())).await.is_err() {
                        closed = true;
                        break;
                    }
                }
                if closed { break; }
                if let Some(frame) = effect.broadcast {
                    room.publish(&frame, &origin);
                }
                dirty |= effect.changed;
            }
            relayed = incoming.recv() => {
                let Some(frame) = relayed else { break };
                if socket.send(Message::Binary(frame.into())).await.is_err() { break; }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)), if dirty => {
                let state = room.state();
                if let Err(error) = spaces.store_ydoc(&user, &item_id, &state).await {
                    tracing::warn!(?error, "space collab state was not stored");
                }
                dirty = false;
            }
        }
    }
    let mut peers = room.peers.lock().unwrap_or_else(|poison| poison.into_inner());
    peers.retain(|peer| !peer.same_channel(&origin));
}

pub fn two_peers_converge() -> Result<(), String> {
    let room = CollabRoom::from_state(&[]);
    let update = text_update("space-collab-needle");
    let frame = prefix("item", &SyncMessageFrame::Sync(SyncMessage::Update(update)));
    let wrote = apply_frame(&room, true, &frame)?;
    if !wrote.changed || wrote.broadcast.is_none() {
        return Err("writer update was not broadcast".into());
    }
    let readonly = text_update("hidden");
    let hidden = prefix("item", &SyncMessageFrame::Sync(SyncMessage::Update(readonly)));
    let blocked = apply_frame(&room, false, &hidden)?;
    if blocked.changed || blocked.broadcast.is_some() {
        return Err("viewer update was applied".into());
    }
    let step = prefix(
        "item",
        &SyncMessageFrame::Sync(SyncMessage::SyncStep1(empty_state())),
    );
    let synced = apply_frame(&room, false, &step)?;
    let peer = Doc::new();
    for reply in &synced.replies {
        let (_, rest) = split_name(reply).ok_or_else(|| "reply missing document name".to_owned())?;
        let mut decoder = DecoderV1::from(rest);
        let message = SyncMessageFrame::decode(&mut decoder).map_err(|error| error.to_string())?;
        if let SyncMessageFrame::Sync(SyncMessage::SyncStep2(bytes) | SyncMessage::Update(bytes)) = message {
            let update = Update::decode_v1(&bytes).map_err(|error| error.to_string())?;
            peer.transact_mut().apply_update(update).map_err(|error| error.to_string())?;
        }
    }
    let text = peer_text(&peer);
    if !text.contains("space-collab-needle") || text.contains("hidden") {
        return Err(format!("peers diverged: {text}"));
    }
    let auth = apply_frame(&room, false, &auth_request("item"))?;
    if auth.replies.len() != 1 || !auth.replies[0].windows(b"read-write".len()).any(|window| window == b"read-write") {
        return Err("auth reply was not granted".into());
    }
    let pong = apply_frame(&room, false, &[9])?;
    if pong.replies != vec![vec![10]] {
        return Err("ping was not answered".into());
    }
    Ok(())
}

fn auth_request(name: &str) -> Vec<u8> {
    let mut encoder = EncoderV1::new();
    encoder.write_string(name);
    encoder.write_var(2u64);
    encoder.write_var(0u64);
    encoder.write_string("token");
    encoder.to_vec()
}

fn text_update(value: &str) -> Vec<u8> {
    let doc = Doc::new();
    let text = doc.get_or_insert_text("content");
    let mut txn = doc.transact_mut();
    text.push(&mut txn, value);
    txn.encode_update_v1()
}

fn peer_text(doc: &Doc) -> String {
    let text = doc.get_or_insert_text("content");
    let txn = doc.transact();
    text.get_string(&txn)
}

use axum::Extension;
