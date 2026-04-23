pub mod canned_chat;
pub mod fixture;
pub mod seeder;

use uuid::{uuid, Uuid};

/// Canonical demo workspace UUID — matches ione-complete-contract.md.
pub const DEMO_WORKSPACE_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d30");
