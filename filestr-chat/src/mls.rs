//! Thin wrapper over `mdk-core` (Marmot/MLS) exposing the hub lifecycle as
//! "give me nostr events to publish / hand me events to process".
//!
//! All MDK calls are synchronous and guarded by the storage's internal
//! `RwLock`, so an `Mls` is safe to share behind a `Mutex` in the daemon.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mdk_core::prelude::*;
use mdk_sqlite_storage::{EncryptionConfig, MdkSqliteStorage};
use nostr::{Event, EventBuilder, Keys, Kind, PublicKey, RelayUrl, UnsignedEvent};

/// MLS key package event kind (Marmot, addressable).
pub const KIND_KEY_PACKAGE: u16 = 30443;
/// Application message kind carried inside the MLS message (Marmot uses 9 for
/// chat). The outer wrapper is kind:445, produced by MDK.
pub const KIND_CHAT: u16 = 9;

pub struct Mls {
    mdk: MDK<MdkSqliteStorage>,
    pub keys: Keys,
}

/// A decrypted application message handed back from [`Mls::process`].
#[derive(Debug, Clone)]
pub struct DecryptedMessage {
    pub id: String,
    pub group_id_hex: String,
    pub author: String,
    pub content: String,
    pub created_at: u64,
}

/// What an inbound MLS event turned into.
pub enum Processed {
    Message(DecryptedMessage),
    /// A commit/proposal/welcome-less control event — state advanced, nothing
    /// to show.
    Control,
    /// Not for us / unprocessable.
    Ignored,
}

impl Mls {
    /// Open (or create) the persistent, at-rest-encrypted MLS store at
    /// `db_path`, keyed by `db_key` (derive it from the node's root key).
    pub fn open(keys: Keys, db_path: &Path, db_key: [u8; 32]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let storage = MdkSqliteStorage::new_with_key(db_path, EncryptionConfig::new(db_key))
            .map_err(|e| anyhow!("open MLS store at {}: {e}", db_path.display()))?;
        Ok(Self { mdk: MDK::new(storage), keys })
    }

    pub fn pubkey(&self) -> PublicKey {
        self.keys.public_key()
    }

    /// Build a signed key-package event (kind:30443) advertising us as
    /// addable to a group.
    pub fn key_package_event(&self, relays: &[RelayUrl]) -> Result<Event> {
        let data = self
            .mdk
            .create_key_package_for_event(&self.keys.public_key(), relays.iter().cloned())
            .map_err(|e| anyhow!("create key package: {e}"))?;
        let event = EventBuilder::new(Kind::Custom(KIND_KEY_PACKAGE), data.content)
            .tags(data.tags_30443)
            .sign_with_keys(&self.keys)
            .context("sign key package event")?;
        Ok(event)
    }

    /// Create a new group owned by us (we are the sole admin/member). Returns
    /// the MLS group id as hex.
    pub fn create_group(&self, name: &str, relays: &[RelayUrl]) -> Result<String> {
        let config = NostrGroupConfigData::new(
            name.to_string(),
            String::new(),
            None,
            None,
            None,
            relays.to_vec(),
            vec![self.keys.public_key()],
        );
        let result = self
            .mdk
            .create_group(&self.keys.public_key(), Vec::new(), config)
            .map_err(|e| anyhow!("create group: {e}"))?;
        Ok(group_id_hex(&result.group.mls_group_id))
    }

    /// Add a member from their key-package event. Returns the welcome rumor to
    /// hand back to the joiner and the evolution (commit) event to broadcast to
    /// existing members.
    pub fn add_member(
        &self,
        group_id_hex: &str,
        key_package_event: &Event,
    ) -> Result<(UnsignedEvent, Event)> {
        let gid = parse_group_id(group_id_hex)?;
        let result = self
            .mdk
            .add_members(&gid, std::slice::from_ref(key_package_event))
            .map_err(|e| anyhow!("add member: {e}"))?;
        self.mdk
            .merge_pending_commit(&gid)
            .map_err(|e| anyhow!("merge commit: {e}"))?;
        let welcome = result
            .welcome_rumors
            .and_then(|mut v| v.drain(..).next())
            .ok_or_else(|| anyhow!("add_members produced no welcome"))?;
        Ok((welcome, result.evolution_event))
    }

    /// Join a group from a welcome rumor. Returns the joined group id hex.
    pub fn join_from_welcome(&self, welcome: &UnsignedEvent) -> Result<String> {
        let zero = nostr::EventId::all_zeros();
        self.mdk
            .process_welcome(&zero, welcome)
            .map_err(|e| anyhow!("process welcome: {e}"))?;
        let pending = self
            .mdk
            .get_pending_welcomes(None)
            .map_err(|e| anyhow!("get pending welcomes: {e}"))?;
        let welcome = pending.first().ok_or_else(|| anyhow!("no pending welcome after process"))?;
        let gid = welcome.mls_group_id.clone();
        self.mdk.accept_welcome(welcome).map_err(|e| anyhow!("accept welcome: {e}"))?;
        Ok(group_id_hex(&gid))
    }

    /// Encrypt `text` as an MLS application message; returns the signed
    /// kind:445 event to publish.
    pub fn create_message(&self, group_id_hex: &str, text: &str) -> Result<Event> {
        let gid = parse_group_id(group_id_hex)?;
        let rumor = EventBuilder::new(Kind::Custom(KIND_CHAT), text).build(self.keys.public_key());
        self.mdk
            .create_message(&gid, rumor, None)
            .map_err(|e| anyhow!("create message: {e}"))
    }

    /// Process an inbound kind:445 event (message or commit).
    pub fn process(&self, event: &Event) -> Result<Processed> {
        match self.mdk.process_message(event) {
            Ok(MessageProcessingResult::ApplicationMessage(m)) => {
                Ok(Processed::Message(DecryptedMessage {
                    id: m.id.to_hex(),
                    group_id_hex: group_id_hex(&m.mls_group_id),
                    author: m.pubkey.to_hex(),
                    content: m.content,
                    created_at: m.created_at.as_secs(),
                }))
            }
            Ok(_) => Ok(Processed::Control),
            Err(e) => {
                tracing::debug!("mls process_message: {e}");
                Ok(Processed::Ignored)
            }
        }
    }

    /// All decrypted chat messages MDK has stored for a group, oldest first.
    pub fn get_messages(&self, group_id_hex: &str) -> Result<Vec<DecryptedMessage>> {
        let gid = parse_group_id(group_id_hex)?;
        let messages =
            self.mdk.get_messages(&gid, None).map_err(|e| anyhow!("get messages: {e}"))?;
        let mut out: Vec<DecryptedMessage> = messages
            .into_iter()
            .filter(|m| m.kind == Kind::Custom(KIND_CHAT))
            .map(|m| DecryptedMessage {
                id: m.id.to_hex(),
                group_id_hex: group_id_hex.to_string(),
                author: m.pubkey.to_hex(),
                content: m.content,
                created_at: m.created_at.as_secs(),
            })
            .collect();
        out.sort_by_key(|m| m.created_at);
        Ok(out)
    }

    /// Member pubkeys (hex) of a group.
    pub fn members(&self, group_id_hex: &str) -> Result<Vec<String>> {
        let gid = parse_group_id(group_id_hex)?;
        let members = self.mdk.get_members(&gid).map_err(|e| anyhow!("get members: {e}"))?;
        Ok(members.iter().map(|p| p.to_hex()).collect())
    }

    /// Remove a member by pubkey hex; returns the evolution event to broadcast.
    pub fn remove_member(&self, group_id_hex: &str, pubkey_hex: &str) -> Result<Event> {
        let gid = parse_group_id(group_id_hex)?;
        let pk = PublicKey::from_hex(pubkey_hex).context("parse member pubkey")?;
        let result = self
            .mdk
            .remove_members(&gid, &[pk])
            .map_err(|e| anyhow!("remove member: {e}"))?;
        self.mdk.merge_pending_commit(&gid).map_err(|e| anyhow!("merge commit: {e}"))?;
        Ok(result.evolution_event)
    }

    /// The nostr group id (32-byte routing id) for a group, hex-encoded, used
    /// to filter group events on the relay.
    pub fn nostr_group_id_hex(&self, group_id_hex: &str) -> Result<String> {
        let gid = parse_group_id(group_id_hex)?;
        let group = self
            .mdk
            .get_group(&gid)
            .map_err(|e| anyhow!("get group: {e}"))?
            .ok_or_else(|| anyhow!("group not found"))?;
        Ok(data_encoding::HEXLOWER.encode(&group.nostr_group_id))
    }
}

fn group_id_hex(id: &GroupId) -> String {
    data_encoding::HEXLOWER.encode(id.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_mls_roundtrip() {
        let relays = [RelayUrl::parse("ws://localhost:8080").unwrap()];
        let dir = std::env::temp_dir().join(format!("filestr-mls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let owner = Mls::open(Keys::generate(), &dir.join("owner.sqlite"), [1u8; 32]).unwrap();
        let member = Mls::open(Keys::generate(), &dir.join("member.sqlite"), [2u8; 32]).unwrap();

        // owner creates the hub, member publishes a key package, owner adds them
        let gid = owner.create_group("test hub", &relays).unwrap();
        let kp = member.key_package_event(&relays).unwrap();
        let (welcome, _evolution) = owner.add_member(&gid, &kp).unwrap();
        let member_gid = member.join_from_welcome(&welcome).unwrap();
        assert_eq!(member_gid, gid, "both sides share one MLS group id");

        // owner -> member, MLS-encrypted
        let msg = owner.create_message(&gid, "hi member").unwrap();
        match member.process(&msg).unwrap() {
            Processed::Message(m) => {
                assert_eq!(m.content, "hi member");
                assert_eq!(m.author, owner.pubkey().to_hex());
            }
            _ => panic!("expected application message"),
        }

        // member -> owner
        let reply = member.create_message(&member_gid, "hi owner").unwrap();
        match owner.process(&reply).unwrap() {
            Processed::Message(m) => assert_eq!(m.content, "hi owner"),
            _ => panic!("expected application message"),
        }

        // membership reflects both
        assert_eq!(owner.members(&gid).unwrap().len(), 2);

        // MDK stores both sent and received on each side, so get_messages is
        // the single source of truth for the chat log.
        assert_eq!(owner.get_messages(&gid).unwrap().len(), 2);
        assert_eq!(member.get_messages(&gid).unwrap().len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn state_survives_reopen() {
        let relays = [RelayUrl::parse("ws://localhost:8080").unwrap()];
        let dir =
            std::env::temp_dir().join(format!("filestr-mls-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("owner.sqlite");
        let keys = Keys::generate();

        let gid = {
            let owner = Mls::open(keys.clone(), &db, [9u8; 32]).unwrap();
            let gid = owner.create_group("persistent", &relays).unwrap();
            owner.create_message(&gid, "remember me").unwrap();
            gid
        };
        // reopen the same encrypted db — the group and its messages persist
        let reopened = Mls::open(keys, &db, [9u8; 32]).unwrap();
        assert_eq!(reopened.members(&gid).unwrap().len(), 1);
        let log = reopened.get_messages(&gid).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].content, "remember me");

        std::fs::remove_dir_all(&dir).ok();
    }
}

fn parse_group_id(hex: &str) -> Result<GroupId> {
    let bytes = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .map_err(|_| anyhow!("bad group id hex"))?;
    Ok(GroupId::from_slice(&bytes))
}
