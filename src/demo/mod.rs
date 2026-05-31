pub mod canned_chat;
pub mod fixture;
pub mod seeder;

use uuid::{uuid, Uuid};

/// Canonical demo workspace UUID — matches ione-complete-contract.md.
pub const DEMO_WORKSPACE_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d30");

/// Demo loopback peer + its trust issuer. The peer's mcp_url points at the
/// node's own `/demo/mcp` route so federation-only Map/Document panels render.
pub const DEMO_TRUST_ISSUER_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d31");
pub const DEMO_PEER_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d32");
