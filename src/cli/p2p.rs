use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};

use crate::cache::CacheDb;
use crate::config::Config;
use crate::p2p::FriendStatus;
use crate::p2p::client::PeerClient;

// ---------------------------------------------------------------------------
// Invite code payload
// ---------------------------------------------------------------------------

/// The JSON payload embedded in a base64 invite code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvitePayload {
    pub peer_id: String,
    pub display_name: String,
    pub public_addr: String,
}

/// Encode an invite payload as a base64 string.
pub fn encode_invite(payload: &InvitePayload) -> Result<String> {
    let json = serde_json::to_string(payload).context("Serialising invite payload")?;
    Ok(B64.encode(json.as_bytes()))
}

/// Decode a base64 invite code back into an `InvitePayload`.
///
/// Tolerates missing `=` padding (common when copy-pasting from terminals).
pub fn decode_invite(code: &str) -> Result<InvitePayload> {
    let trimmed = code.trim();
    // Re-add stripped padding so the base64 decoder doesn't reject it.
    let padded;
    let to_decode = match trimmed.len() % 4 {
        2 => {
            padded = format!("{}==", trimmed);
            padded.as_str()
        }
        3 => {
            padded = format!("{}=", trimmed);
            padded.as_str()
        }
        _ => trimmed,
    };
    let bytes = B64
        .decode(to_decode)
        .context("Decoding invite code (not valid base64)")?;
    let json = std::str::from_utf8(&bytes).context("Invite code is not valid UTF-8")?;
    serde_json::from_str(json).context("Invite code JSON is malformed")
}

/// Returns `true` when `s` looks like a base64 invite code rather than a
/// raw peer_id (peer IDs are bare UUID-ish strings without `=` or `/`).
fn looks_like_invite_code(s: &str) -> bool {
    // Unambiguous check: try to decode and parse as a valid InvitePayload.
    // If it succeeds, it's an invite code; raw peer_ids are not valid invite codes.
    s.len() > 40 && decode_invite(s).is_ok()
}

// ---------------------------------------------------------------------------
// Helper: open DB (must already exist)
// ---------------------------------------------------------------------------

async fn open_db() -> Result<CacheDb> {
    let db_path = Config::data_dir().join("cache.db");
    if !db_path.exists() {
        bail!("Cache database not found. Run `dja init` first.");
    }
    CacheDb::open(&db_path).await
}

// ---------------------------------------------------------------------------
// Sub-command handlers
// ---------------------------------------------------------------------------

/// `dja p2p invite` — print the invite code for this node.
pub async fn run_invite() -> Result<()> {
    let config = Config::load()?;

    if config.p2p.peer_id.is_empty() {
        bail!(
            "P2P peer_id is not configured. \
             Run `dja init` or set [p2p] peer_id in your config."
        );
    }
    if config.p2p.public_addr.is_empty() {
        bail!(
            "P2P public_addr is not configured. \
             Set [p2p] public_addr in ~/.config/dja/config.toml."
        );
    }

    let payload = InvitePayload {
        peer_id: config.p2p.peer_id.clone(),
        display_name: config.p2p.display_name.clone(),
        public_addr: config.p2p.public_addr.clone(),
    };

    let code = encode_invite(&payload)?;

    // ── Identity box ────────────────────────────────────────────────────────
    let label_w = 14usize; // "display name:" width
    let value_col = |label: &str, value: &str| {
        format!("  {:<label_w$} {}", label, value, label_w = label_w)
    };
    let rows = vec![
        value_col("peer id:", &payload.peer_id),
        value_col("name:", &payload.display_name),
        value_col("address:", &payload.public_addr),
    ];
    let inner_w = rows.iter().map(|r| r.len()).max().unwrap_or(40).max(40);
    let border_top = format!("╭─ Your P2P invite {}╮", "─".repeat(inner_w - 19));
    let border_bot = format!("╰{}╯", "─".repeat(inner_w + 2));

    println!();
    println!("{}", border_top);
    for row in &rows {
        println!("│{:<width$}│", row, width = inner_w + 2);
    }
    println!("{}", border_bot);

    // ── Invite code block ───────────────────────────────────────────────────
    println!();
    println!("  Invite code:");
    println!();
    println!("    {}", code);
    println!();
    println!("  Share with your friend — they run:");
    println!();
    println!("    dja p2p add {}", code);
    println!();
    Ok(())
}

/// `dja p2p add <code|peer_id> [--addr <addr>]` — add a friend.
pub async fn run_add(code_or_peer_id: &str, addr: Option<&str>) -> Result<()> {
    let db = open_db().await?;

    let (peer_id, display_name, public_addr) = if looks_like_invite_code(code_or_peer_id) {
        // Treat argument as an invite code.
        let payload = decode_invite(code_or_peer_id)
            .context("Failed to parse invite code")?;
        (payload.peer_id, payload.display_name, payload.public_addr)
    } else {
        // Raw peer_id — require --addr.
        let addr = addr.ok_or_else(|| {
            anyhow!(
                "When adding by peer_id you must supply --addr <public_addr>. \
                 Example: dja p2p add <peer_id> --addr host:9843"
            )
        })?;
        (
            code_or_peer_id.to_string(),
            String::new(),
            addr.to_string(),
        )
    };

    // Add as PendingSent initially — we'll upgrade to Active only after the
    // remote peer acknowledges the invite.
    db.add_friend(&peer_id, &display_name, &public_addr, FriendStatus::PendingSent)
        .await
        .context("Saving friend to database")?;

    println!("  peer_id:   {}", peer_id);
    if !display_name.is_empty() {
        println!("  name:      {}", display_name);
    }
    println!("  addr:      {}", public_addr);

    // Call peer's POST /p2p/invite to perform mutual registration.
    // If successful, upgrade local status to Active; otherwise leave as PendingSent.
    let config = Config::load().unwrap_or_default();
    let self_peer_id = &config.p2p.peer_id;
    let own_display_name = &config.p2p.display_name;
    let own_public_addr = &config.p2p.public_addr;

    if !self_peer_id.is_empty() && !own_public_addr.is_empty() {
        let peer_client = PeerClient::new();
        match peer_client
            .send_invite(&public_addr, self_peer_id, own_display_name, own_public_addr)
            .await
        {
            Ok(()) => {
                db.update_friend_status(&peer_id, FriendStatus::Active)
                    .await
                    .context("Upgrading friend status to active")?;
                println!("Friend added!");
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not reach peer to send invite (saved as pending_sent): {e}"
                );
                println!("Friend saved as pending (invite not yet delivered).");
                println!("Re-run `dja p2p add` when the peer is reachable to complete the handshake.");
            }
        }
    } else {
        // No self identity configured — just leave as pending_sent.
        println!("Friend saved as pending (P2P identity not fully configured).");
    }

    Ok(())
}

/// `dja p2p remove <peer_id>` — remove a friend.
pub async fn run_remove(peer_id: &str) -> Result<()> {
    let db = open_db().await?;
    db.remove_friend(peer_id)
        .await
        .context("Removing friend from database")?;
    println!("Friend {} removed.", peer_id);
    Ok(())
}

/// `dja p2p friends` — list all friends in a table.
pub async fn run_friends() -> Result<()> {
    let db = open_db().await?;
    let friends = db.list_friends().await.context("Listing friends")?;

    if friends.is_empty() {
        println!("No friends yet. Use `dja p2p add <invite_code>` to add one.");
        return Ok(());
    }

    // Column widths (min widths so the header always fits).
    let id_w = friends
        .iter()
        .map(|f| f.peer_id.len())
        .max()
        .unwrap_or(0)
        .max(7);
    let name_w = friends
        .iter()
        .map(|f| f.display_name.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let addr_w = friends
        .iter()
        .map(|f| f.public_addr.len())
        .max()
        .unwrap_or(0)
        .max(7);
    let status_w = 8usize; // "active" / "pending…"

    println!(
        "{:<id_w$}  {:<name_w$}  {:<addr_w$}  {:<status_w$}",
        "PEER ID", "NAME", "ADDRESS", "STATUS",
        id_w = id_w,
        name_w = name_w,
        addr_w = addr_w,
        status_w = status_w,
    );
    println!("{}", "-".repeat(id_w + name_w + addr_w + status_w + 6));

    for f in &friends {
        println!(
            "{:<id_w$}  {:<name_w$}  {:<addr_w$}  {:<status_w$}",
            f.peer_id,
            f.display_name,
            f.public_addr,
            f.status.to_string(),
            id_w = id_w,
            name_w = name_w,
            addr_w = addr_w,
            status_w = status_w,
        );
    }
    Ok(())
}

/// `dja p2p status` — show P2P status.
pub async fn run_status() -> Result<()> {
    let config = Config::load()?;
    let db = open_db().await?;
    let friend_count = db.list_friends().await?.len();

    let enabled_str = if config.p2p.enabled { "enabled" } else { "disabled" };
    println!("P2P status:    {}", enabled_str);
    println!("peer_id:       {}", if config.p2p.peer_id.is_empty() { "(not set)" } else { &config.p2p.peer_id });
    println!("display_name:  {}", if config.p2p.display_name.is_empty() { "(not set)" } else { &config.p2p.display_name });
    println!("public_addr:   {}", if config.p2p.public_addr.is_empty() { "(not set)" } else { &config.p2p.public_addr });
    println!("listen_port:   {}", config.p2p.listen_port);
    println!("friends:       {}", friend_count);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Invite code encode/decode ---

    #[test]
    fn test_invite_encode_decode_roundtrip() {
        let payload = InvitePayload {
            peer_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            display_name: "Alice's Mac".to_string(),
            public_addr: "alice.tail:9843".to_string(),
        };

        let code = encode_invite(&payload).expect("encode failed");
        assert!(!code.is_empty());

        let decoded = decode_invite(&code).expect("decode failed");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_invite_decode_invalid_base64() {
        let result = decode_invite("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_decode_invalid_json() {
        // Valid base64 but not valid JSON.
        let bad_json = B64.encode(b"hello world");
        let result = decode_invite(&bad_json);
        assert!(result.is_err());
    }

    #[test]
    fn test_invite_code_is_stable() {
        // Same payload should always produce the same code (deterministic).
        let payload = InvitePayload {
            peer_id: "abc".to_string(),
            display_name: "Bob".to_string(),
            public_addr: "bob:9843".to_string(),
        };
        let code1 = encode_invite(&payload).unwrap();
        let code2 = encode_invite(&payload).unwrap();
        assert_eq!(code1, code2);
    }

    // --- DB integration tests ---

    #[tokio::test]
    async fn test_add_friend_via_invite_code() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        let payload = InvitePayload {
            peer_id: "peer-alice".to_string(),
            display_name: "Alice".to_string(),
            public_addr: "alice.tailnet:9843".to_string(),
        };

        let code = encode_invite(&payload).unwrap();
        let decoded = decode_invite(&code).unwrap();

        db.add_friend(
            &decoded.peer_id,
            &decoded.display_name,
            &decoded.public_addr,
            FriendStatus::Active,
        )
        .await
        .expect("add_friend failed");

        let friends = db.list_friends().await.unwrap();
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].peer_id, "peer-alice");
        assert_eq!(friends[0].display_name, "Alice");
        assert_eq!(friends[0].public_addr, "alice.tailnet:9843");
        assert_eq!(friends[0].status, FriendStatus::Active);
    }

    #[tokio::test]
    async fn test_remove_friend_via_db() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-bob", "Bob", "bob:9843", FriendStatus::Active)
            .await
            .unwrap();
        assert!(db.is_friend("peer-bob").await.unwrap());

        db.remove_friend("peer-bob").await.unwrap();
        assert!(!db.is_friend("peer-bob").await.unwrap());

        let friends = db.list_friends().await.unwrap();
        assert!(friends.is_empty());
    }

    #[tokio::test]
    async fn test_friends_list_multiple() {
        let db = CacheDb::open_in_memory().await.expect("open failed");

        db.add_friend("peer-1", "Alice", "alice:9843", FriendStatus::Active)
            .await
            .unwrap();
        db.add_friend("peer-2", "Bob", "bob:9843", FriendStatus::PendingSent)
            .await
            .unwrap();

        let friends = db.list_friends().await.unwrap();
        assert_eq!(friends.len(), 2);

        // Verify both are present (ordered by added_at, but both added nearly simultaneously).
        let ids: Vec<&str> = friends.iter().map(|f| f.peer_id.as_str()).collect();
        assert!(ids.contains(&"peer-1"));
        assert!(ids.contains(&"peer-2"));
    }

    // --- looks_like_invite_code ---

    #[test]
    fn test_looks_like_invite_code_vs_peer_id() {
        let payload = InvitePayload {
            peer_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            display_name: "Test".to_string(),
            public_addr: "host:9843".to_string(),
        };
        let code = encode_invite(&payload).unwrap();
        assert!(looks_like_invite_code(&code));

        // A raw UUID peer_id should NOT look like an invite code.
        assert!(!looks_like_invite_code("550e8400-e29b-41d4-a716-446655440000"));
    }
}
