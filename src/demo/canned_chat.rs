//! Canned-response layer for the demo workspace.
//!
//! When a conversation's workspace is the demo workspace, chat POSTs are
//! answered from a static map instead of calling Ollama. Allows demo to
//! work without any LLM infrastructure.

use phf::phf_map;

/// Exact-match canned responses for the 4 suggested demo prompts.
/// Keys must be pre-normalized via [`normalize`].
static CANNED: phf::Map<&'static str, &'static str> = phf_map! {
    "what wildfires are active near populated areas right now"
        => "Three fire detections in the last 12h intersect with populated Census blocks: Lolo Complex (MT, S-0142, 2,400 ac, 18 structures within 2mi), Hayes Creek (ID, S-0143, 620 ac, 4 structures within 1mi), and Sapphire Ridge (MT, S-0144, 180 ac, 0 structures within 5mi). Generator surfaced all three as command-severity; critic downgraded Sapphire Ridge to routine after comparing fuel-model projections. See the Survivors tab for each one's reasoning trace.",
    "which nws alerts in the last 24h need field response"
        => "Two of nine NWS alerts in the last 24h were routed to 'notification' by the classifier and are pending field response: a Red Flag Warning for Missoula County (expiring 18:00 local, low RH + 25mph gusts) and a Flash Flood Watch for the Bitterroot drainage downstream of Lolo Creek burn scar. Three alerts were rule-floored to 'routine' and suppressed.",
    "why did the critic reject survivor s-0142"
        => "Survivor S-0142 was not rejected — the critic marked it 'survive' with high confidence. You may be thinking of S-0145 (draft briefing for Sapphire Ridge), which the critic rejected. Its generator claim was 'imminent structure threat'; the critic cited the 5-mile separation and dominant downslope winds as disconfirming evidence and flipped the verdict to 'reject'. The exchange is in the audit trail for the Sapphire Ridge routing decision.",
    "what approvals are pending and why"
        => "Two approvals are pending: (1) draft evacuation advisory for the Lolo Creek corridor — waiting on incident commander sign-off because auto-exec's severity cap is set to 'flagged' and this is 'command'; and (2) a Slack notification to the Bitterroot field team about the Flash Flood Watch, held pending field-lead confirmation that the team is on-shift. Both will auto-notify the next on-call if not decided within 30 minutes.",
};

const STOCK_REPLY: &str = "I can answer a handful of questions in demo mode. Try one of the suggested prompts above, or switch to your workspace to ask freely.";

/// Returns the canned assistant response for a prompt, or the stock reply if
/// no canned prompt matches. Matching is case-insensitive and strips
/// non-alphanumeric characters (except whitespace).
pub fn canned_response(prompt: &str) -> &'static str {
    let key = normalize(prompt);
    CANNED.get(key.as_str()).copied().unwrap_or(STOCK_REPLY)
}

fn normalize(s: &str) -> String {
    let filtered: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .collect();
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canned_match_ignores_punctuation_and_case() {
        assert!(
            canned_response("What WILDFIRES are active near populated areas, RIGHT NOW?")
                .starts_with("Three fire detections")
        );
    }

    #[test]
    fn unmatched_returns_stock() {
        assert!(canned_response("something random").starts_with("I can answer"));
    }
}
