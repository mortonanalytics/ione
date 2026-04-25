/// Typed fixture descriptors for demo workspace seed data.
///
/// All UUIDs are fixed constants so the seeder is deterministic and
/// re-entrant. Timestamps are expressed as minutes-before-now offsets;
/// the seeder converts them using `chrono::Utc::now() - Duration::minutes(N)`.
use uuid::{uuid, Uuid};

use crate::models::{
    ActorKind, ArtifactKind, CriticVerdict, MessageRole, RoutingTarget, Severity, SignalSource,
};

// ─── Fixed UUIDs ─────────────────────────────────────────────────────────────

pub const DEMO_ORG_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d01");
pub const DEMO_USER_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000d02");

// Connector IDs
pub const CONN_NWS: Uuid = uuid!("00000000-0000-0000-0000-000000000c01");
pub const CONN_FIRMS: Uuid = uuid!("00000000-0000-0000-0000-000000000c02");
pub const CONN_SLACK: Uuid = uuid!("00000000-0000-0000-0000-000000000c03");
pub const CONN_IRWIN: Uuid = uuid!("00000000-0000-0000-0000-000000000c04");

// Stream IDs (one per connector)
pub const STREAM_NWS: Uuid = uuid!("00000000-0000-0000-0000-000000000e01");
pub const STREAM_FIRMS: Uuid = uuid!("00000000-0000-0000-0000-000000000e02");
pub const STREAM_SLACK: Uuid = uuid!("00000000-0000-0000-0000-000000000e03");
pub const STREAM_IRWIN: Uuid = uuid!("00000000-0000-0000-0000-000000000e04");

// Role IDs
pub const ROLE_IC: Uuid = uuid!("00000000-0000-0000-0000-000000000a01");
pub const ROLE_FIELD_LEAD: Uuid = uuid!("00000000-0000-0000-0000-000000000a02");
pub const ROLE_ANALYST: Uuid = uuid!("00000000-0000-0000-0000-000000000a03");

// Signal IDs
pub const SIG_1: Uuid = uuid!("00000000-0000-0000-0000-000000000f01");
pub const SIG_2: Uuid = uuid!("00000000-0000-0000-0000-000000000f02");
pub const SIG_3: Uuid = uuid!("00000000-0000-0000-0000-000000000f03");
pub const SIG_4: Uuid = uuid!("00000000-0000-0000-0000-000000000f04");
pub const SIG_5: Uuid = uuid!("00000000-0000-0000-0000-000000000f05");
pub const SIG_6: Uuid = uuid!("00000000-0000-0000-0000-000000000f06");
pub const SIG_7: Uuid = uuid!("00000000-0000-0000-0000-000000000f07");
pub const SIG_8: Uuid = uuid!("00000000-0000-0000-0000-000000000f08");

// Survivor IDs (5 — each on a distinct signal)
pub const SURV_1: Uuid = uuid!("00000000-0000-0000-0000-000000000b01");
pub const SURV_2: Uuid = uuid!("00000000-0000-0000-0000-000000000b02");
pub const SURV_3: Uuid = uuid!("00000000-0000-0000-0000-000000000b03");
pub const SURV_4: Uuid = uuid!("00000000-0000-0000-0000-000000000b04");
pub const SURV_5: Uuid = uuid!("00000000-0000-0000-0000-000000000b05");

// Routing decision IDs
pub const ROUTE_1: Uuid = uuid!("00000000-0000-0000-0000-000000000d11");
pub const ROUTE_2: Uuid = uuid!("00000000-0000-0000-0000-000000000d12");
pub const ROUTE_3: Uuid = uuid!("00000000-0000-0000-0000-000000000d13");
pub const ROUTE_4: Uuid = uuid!("00000000-0000-0000-0000-000000000d14");
pub const ROUTE_5: Uuid = uuid!("00000000-0000-0000-0000-000000000d15");

// Artifact IDs
pub const ART_1: Uuid = uuid!("00000000-0000-0000-0000-000000000dd1");
pub const ART_2: Uuid = uuid!("00000000-0000-0000-0000-000000000dd2");
pub const ART_3: Uuid = uuid!("00000000-0000-0000-0000-000000000dd3");

// Approval IDs
pub const APPR_1: Uuid = uuid!("00000000-0000-0000-0000-000000000aa1");
pub const APPR_2: Uuid = uuid!("00000000-0000-0000-0000-000000000aa2");
pub const APPR_3: Uuid = uuid!("00000000-0000-0000-0000-000000000aa3");
pub const APPR_4: Uuid = uuid!("00000000-0000-0000-0000-000000000aa4");

// Conversation ID
pub const CONV_1: Uuid = uuid!("00000000-0000-0000-0000-000000000cc1");

// ─── Descriptor types ─────────────────────────────────────────────────────────

pub struct ConnectorFixture {
    pub id: Uuid,
    pub stream_id: Uuid,
    pub name: &'static str,
    pub config: serde_json::Value,
}

pub struct StreamEventFixture {
    pub stream_id: Uuid,
    /// Minutes before now.
    pub offset_minutes: i64,
    pub payload: serde_json::Value,
}

pub struct SignalFixture {
    pub id: Uuid,
    pub source: SignalSource,
    pub title: &'static str,
    pub body: &'static str,
    pub severity: Severity,
    pub evidence: serde_json::Value,
}

pub struct SurvivorFixture {
    pub id: Uuid,
    pub signal_id: Uuid,
    pub verdict: CriticVerdict,
    pub rationale: &'static str,
    pub confidence: f32,
}

pub struct RoutingDecisionFixture {
    pub id: Uuid,
    pub survivor_id: Uuid,
    pub target_kind: RoutingTarget,
    pub target_ref: serde_json::Value,
}

pub struct ArtifactFixture {
    pub id: Uuid,
    pub survivor_id: Uuid,
    pub kind: ArtifactKind,
    pub content: serde_json::Value,
}

pub struct ApprovalFixture {
    pub id: Uuid,
    pub artifact_id: Uuid,
    /// None = pending; Some(true) = approved; Some(false) = rejected.
    pub decided: Option<bool>,
    pub comment: Option<&'static str>,
}

pub struct AuditEventFixture {
    pub workspace_id: Uuid,
    pub actor_kind: ActorKind,
    pub actor_ref: &'static str,
    pub verb: &'static str,
    pub object_kind: &'static str,
    pub object_id: Uuid,
}

pub struct MessageFixture {
    pub role: MessageRole,
    pub content: &'static str,
}

// ─── Fixture data ─────────────────────────────────────────────────────────────

pub fn connectors() -> Vec<ConnectorFixture> {
    use serde_json::json;
    vec![
        ConnectorFixture {
            id: CONN_NWS,
            stream_id: STREAM_NWS,
            name: "nws",
            config: json!({"kind":"nws","lat":46.87,"lon":-113.99}),
        },
        ConnectorFixture {
            id: CONN_FIRMS,
            stream_id: STREAM_FIRMS,
            name: "firms",
            config: json!({"kind":"firms","mapKey":"demo","country":"USA"}),
        },
        ConnectorFixture {
            id: CONN_SLACK,
            stream_id: STREAM_SLACK,
            name: "slack",
            config: json!({"kind":"slack","webhook_url":"https://hooks.slack.com/demo"}),
        },
        ConnectorFixture {
            id: CONN_IRWIN,
            stream_id: STREAM_IRWIN,
            name: "irwin",
            config: json!({"kind":"irwin","base_url":"mock://irwin"}),
        },
    ]
}

pub fn stream_events() -> Vec<StreamEventFixture> {
    use serde_json::json;
    vec![
        // NWS — 5 events (offsets 200-240)
        StreamEventFixture {
            stream_id: STREAM_NWS,
            offset_minutes: 200,
            payload: json!({"event":"Red Flag Warning","area":"Missoula County","expires":"2h"}),
        },
        StreamEventFixture {
            stream_id: STREAM_NWS,
            offset_minutes: 210,
            payload: json!({"event":"Wind Advisory","area":"Bitterroot Valley","speed_mph":45}),
        },
        StreamEventFixture {
            stream_id: STREAM_NWS,
            offset_minutes: 220,
            payload: json!({"event":"Air Quality Alert","area":"Western MT","aqi":185}),
        },
        StreamEventFixture {
            stream_id: STREAM_NWS,
            offset_minutes: 230,
            payload: json!({"event":"Red Flag Warning","area":"Ravalli County","expires":"4h"}),
        },
        StreamEventFixture {
            stream_id: STREAM_NWS,
            offset_minutes: 240,
            payload: json!({"event":"Excessive Heat Watch","area":"Clark Fork","peak_f":107}),
        },
        // FIRMS — 3 events (offsets 150-170)
        StreamEventFixture {
            stream_id: STREAM_FIRMS,
            offset_minutes: 150,
            payload: json!({"hotspots":5,"area":"Lolo NF","confidence":"high","source":"VIIRS"}),
        },
        StreamEventFixture {
            stream_id: STREAM_FIRMS,
            offset_minutes: 160,
            payload: json!({"hotspots":3,"area":"Bitterroot NF","confidence":"nominal","source":"MODIS"}),
        },
        StreamEventFixture {
            stream_id: STREAM_FIRMS,
            offset_minutes: 170,
            payload: json!({"hotspots":1,"area":"Clearwater NF","confidence":"low","source":"VIIRS"}),
        },
        // Slack — 3 events (offsets 100-120)
        StreamEventFixture {
            stream_id: STREAM_SLACK,
            offset_minutes: 100,
            payload: json!({"channel":"#fire-ops","user":"dispatch","text":"Engine 4 en route to Lolo NF"}),
        },
        StreamEventFixture {
            stream_id: STREAM_SLACK,
            offset_minutes: 110,
            payload: json!({"channel":"#fire-ops","user":"incident_cmd","text":"IC declares Type 3 incident — Lolo SE"}),
        },
        StreamEventFixture {
            stream_id: STREAM_SLACK,
            offset_minutes: 120,
            payload: json!({"channel":"#air-ops","user":"air_ops","text":"Tanker 80 retardant drop complete — grid 4422"}),
        },
        // IRWIN — 2 events (offsets 80-90)
        StreamEventFixture {
            stream_id: STREAM_IRWIN,
            offset_minutes: 80,
            payload: json!({"incidentId":"MT-LNF-123","name":"Lolo SE Fire","acres":340,"containment":0}),
        },
        StreamEventFixture {
            stream_id: STREAM_IRWIN,
            offset_minutes: 90,
            payload: json!({"incidentId":"MT-LNF-123","name":"Lolo SE Fire","acres":520,"containment":5}),
        },
    ]
}

pub fn signals() -> Vec<SignalFixture> {
    use serde_json::json;
    vec![
        SignalFixture {
            id: SIG_1,
            source: SignalSource::Rule,
            title: "Red Flag Warning: Missoula County",
            body: "NWS issued Red Flag Warning for Missoula County. High fire weather conditions expected through the evening.",
            severity: Severity::Flagged,
            evidence: json!([{"source":"nws","event":"Red Flag Warning","area":"Missoula County"}]),
        },
        SignalFixture {
            id: SIG_2,
            source: SignalSource::Rule,
            title: "FIRMS High Hotspot Count: Lolo NF",
            body: "VIIRS detected 5 hotspots in the Lolo NF area in the last 24h, exceeding the threshold for immediate review.",
            severity: Severity::Flagged,
            evidence: json!([{"source":"firms","hotspots":5,"area":"Lolo NF","confidence":"high"}]),
        },
        SignalFixture {
            id: SIG_3,
            source: SignalSource::Generator,
            title: "Incident escalation: Lolo SE Fire now Type 3",
            body: "Cross-referencing IRWIN incident data and Slack comms: IC has declared Type 3 incident. Recommend routing to command approval.",
            severity: Severity::Command,
            evidence: json!([{"source":"irwin","incidentId":"MT-LNF-123","acres":520},{"source":"slack","text":"IC declares Type 3 incident"}]),
        },
        SignalFixture {
            id: SIG_4,
            source: SignalSource::Rule,
            title: "Air Quality Alert: Western MT AQI 185",
            body: "NWS Air Quality Alert for Western Montana. AQI 185 — Unhealthy. Field crews must use respirators.",
            severity: Severity::Routine,
            evidence: json!([{"source":"nws","event":"Air Quality Alert","aqi":185}]),
        },
        SignalFixture {
            id: SIG_5,
            source: SignalSource::Generator,
            title: "Wind advisory may drive fire toward populated areas",
            body: "Wind advisory (45 mph gusts) in Bitterroot Valley combined with existing Lolo SE fire track. Analysis: 60% probability of spotting toward Stevensville within 6h.",
            severity: Severity::Flagged,
            evidence: json!([{"source":"nws","event":"Wind Advisory","speed_mph":45},{"source":"irwin","acres":520}]),
        },
        SignalFixture {
            id: SIG_6,
            source: SignalSource::Rule,
            title: "FIRMS Bitterroot NF hotspot cluster",
            body: "MODIS detected 3 hotspots in Bitterroot NF. Confidence nominal. Recommend field check.",
            severity: Severity::Routine,
            evidence: json!([{"source":"firms","hotspots":3,"area":"Bitterroot NF","confidence":"nominal"}]),
        },
        SignalFixture {
            id: SIG_7,
            source: SignalSource::Generator,
            title: "Tanker 80 drop effectiveness uncertain",
            body: "Retardant drop logged at grid 4422, but satellite hot-spot count unchanged 30 min later. Recommend ground truth recon.",
            severity: Severity::Flagged,
            evidence: json!([{"source":"slack","text":"Tanker 80 retardant drop complete"},{"source":"firms","hotspots":5}]),
        },
        SignalFixture {
            id: SIG_8,
            source: SignalSource::Rule,
            title: "Clearwater NF low-confidence hotspot",
            body: "VIIRS low-confidence hotspot in Clearwater NF. Below threshold for autonomous escalation. Flagged for analyst review.",
            severity: Severity::Routine,
            evidence: json!([{"source":"firms","hotspots":1,"area":"Clearwater NF","confidence":"low"}]),
        },
    ]
}

pub fn survivors() -> Vec<SurvivorFixture> {
    vec![
        SurvivorFixture {
            id: SURV_1,
            signal_id: SIG_1,
            verdict: CriticVerdict::Survive,
            rationale: "Red Flag Warning with dry, windy conditions in an active fire district warrants routing to field lead.",
            confidence: 0.91,
        },
        SurvivorFixture {
            id: SURV_2,
            signal_id: SIG_2,
            verdict: CriticVerdict::Survive,
            rationale: "Five high-confidence VIIRS hotspots exceed the density threshold. Multiple independent sensors agree. Signal is actionable.",
            confidence: 0.95,
        },
        SurvivorFixture {
            id: SURV_3,
            signal_id: SIG_3,
            verdict: CriticVerdict::Survive,
            rationale: "Type 3 IC declaration is a high-authority event. Cross-source corroboration from IRWIN and Slack is strong.",
            confidence: 0.98,
        },
        SurvivorFixture {
            id: SURV_4,
            signal_id: SIG_5,
            verdict: CriticVerdict::Reject,
            rationale: "Wind direction analysis does not support the 60% spotting probability claim. Model did not account for terrain channeling in the Bitterroot Valley. Recommend re-run with corrected wind vector data.",
            confidence: 0.72,
        },
        SurvivorFixture {
            id: SURV_5,
            signal_id: SIG_7,
            verdict: CriticVerdict::Defer,
            rationale: "Tanker effectiveness assessment requires ground truth. Deferring pending recon report expected within 2h.",
            confidence: 0.61,
        },
    ]
}

pub fn routing_decisions() -> Vec<RoutingDecisionFixture> {
    use serde_json::json;
    vec![
        RoutingDecisionFixture {
            id: ROUTE_1,
            survivor_id: SURV_1,
            target_kind: RoutingTarget::Feed,
            target_ref: json!({"role_id": ROLE_FIELD_LEAD}),
        },
        RoutingDecisionFixture {
            id: ROUTE_2,
            survivor_id: SURV_2,
            target_kind: RoutingTarget::Notification,
            target_ref: json!({"channel": "fire-ops", "priority": "high"}),
        },
        RoutingDecisionFixture {
            id: ROUTE_3,
            survivor_id: SURV_3,
            target_kind: RoutingTarget::Draft,
            target_ref: json!({"artifact_kind": "briefing", "audience": "incident_commander"}),
        },
        RoutingDecisionFixture {
            id: ROUTE_4,
            survivor_id: SURV_4,
            target_kind: RoutingTarget::Feed,
            target_ref: json!({"role_id": ROLE_ANALYST}),
        },
        RoutingDecisionFixture {
            id: ROUTE_5,
            survivor_id: SURV_5,
            target_kind: RoutingTarget::Peer,
            target_ref: json!({"peer_id": "00000000-0000-0000-0000-000000000000", "tool": "recon_status"}),
        },
    ]
}

pub fn artifacts() -> Vec<ArtifactFixture> {
    use serde_json::json;
    vec![
        ArtifactFixture {
            id: ART_1,
            survivor_id: SURV_2,
            kind: ArtifactKind::NotificationDraft,
            content: json!({
                "title": "FIRMS Alert: Lolo NF High Hotspot Count",
                "body": "5 high-confidence VIIRS hotspots detected in Lolo National Forest. Immediate review recommended.",
                "recipients": ["dispatch", "field_lead"],
                "priority": "high"
            }),
        },
        ArtifactFixture {
            id: ART_2,
            survivor_id: SURV_3,
            kind: ArtifactKind::Briefing,
            content: json!({
                "title": "Incident Briefing: Lolo SE Fire — Type 3 Declaration",
                "summary": "IC declared Type 3 incident at 14:32. Current: 520 acres, 5% contained. Tanker 80 retardant drop completed grid 4422.",
                "situation": "IRWIN confirms MT-LNF-123 growth from 340 to 520 acres in 2h. Wind advisory (45 mph) active.",
                "recommended_action": "Convene IC staff meeting. Review resource order. Confirm tanker effectiveness via ground recon.",
                "prepared_by": "IONe Generator"
            }),
        },
        ArtifactFixture {
            id: ART_3,
            survivor_id: SURV_1,
            kind: ArtifactKind::NotificationDraft,
            content: json!({
                "title": "Red Flag Warning Active — Missoula County",
                "body": "NWS Red Flag Warning in effect. Extreme fire weather through 20:00. All crews review pre-suppression posture.",
                "recipients": ["all_field_crews"],
                "priority": "medium"
            }),
        },
    ]
}

pub fn approvals() -> Vec<ApprovalFixture> {
    vec![
        ApprovalFixture {
            id: APPR_1,
            artifact_id: ART_1,
            decided: None,
            comment: None,
        },
        ApprovalFixture {
            id: APPR_2,
            artifact_id: ART_2,
            decided: None,
            comment: None,
        },
        ApprovalFixture {
            id: APPR_3,
            artifact_id: ART_2,
            decided: Some(true),
            comment: Some("Confirmed. Briefing distributed to IC staff."),
        },
        ApprovalFixture {
            id: APPR_4,
            artifact_id: ART_3,
            decided: Some(false),
            comment: Some(
                "Notification copy needs revision — Red Flag expired 30 min ago. Regenerate.",
            ),
        },
    ]
}

pub fn audit_events() -> Vec<AuditEventFixture> {
    vec![
        AuditEventFixture {
            workspace_id: crate::demo::DEMO_WORKSPACE_ID,
            actor_kind: ActorKind::System,
            actor_ref: "ione-router",
            verb: "routed",
            object_kind: "routing_decision",
            object_id: ROUTE_1,
        },
        AuditEventFixture {
            workspace_id: crate::demo::DEMO_WORKSPACE_ID,
            actor_kind: ActorKind::System,
            actor_ref: "ione-router",
            verb: "routed",
            object_kind: "routing_decision",
            object_id: ROUTE_2,
        },
        AuditEventFixture {
            workspace_id: crate::demo::DEMO_WORKSPACE_ID,
            actor_kind: ActorKind::System,
            actor_ref: "ione-router",
            verb: "routed",
            object_kind: "routing_decision",
            object_id: ROUTE_3,
        },
        AuditEventFixture {
            workspace_id: crate::demo::DEMO_WORKSPACE_ID,
            actor_kind: ActorKind::System,
            actor_ref: "ione-router",
            verb: "routed",
            object_kind: "routing_decision",
            object_id: ROUTE_4,
        },
        AuditEventFixture {
            workspace_id: crate::demo::DEMO_WORKSPACE_ID,
            actor_kind: ActorKind::System,
            actor_ref: "ione-router",
            verb: "routed",
            object_kind: "routing_decision",
            object_id: ROUTE_5,
        },
    ]
}

pub fn conversation_messages() -> Vec<MessageFixture> {
    vec![
        MessageFixture {
            role: MessageRole::User,
            content: "What wildfires are active near populated areas right now?",
        },
        MessageFixture {
            role: MessageRole::Assistant,
            content: "Based on current IRWIN and FIRMS data, the **Lolo SE Fire** (MT-LNF-123) is the primary concern — 520 acres with 5% containment, located within 12 miles of Stevensville (pop. ~1,900). NWS Wind Advisory at 45 mph active in the Bitterroot Valley increases spotting risk toward populated areas. Two other hotspot clusters in Bitterroot NF and Clearwater NF are below escalation threshold. Recommend reviewing the Lolo SE briefing artifact.",
        },
        MessageFixture {
            role: MessageRole::User,
            content: "Why did the critic reject survivor S-0142?",
        },
        MessageFixture {
            role: MessageRole::Assistant,
            content: "Survivor S-0142 was **rejected** by the critic (confidence 0.72). The generator claimed a 60% probability of fire spotting toward Stevensville within 6 hours based on the Wind Advisory. The critic disagreed: the wind direction analysis did not account for terrain channeling in the Bitterroot Valley, which historically redirects surface winds away from Stevensville at that speed. The critic recommended re-running the model with a corrected wind vector dataset before re-submitting.",
        },
        MessageFixture {
            role: MessageRole::User,
            content: "What approvals are pending and why?",
        },
        MessageFixture {
            role: MessageRole::Assistant,
            content: "Two approvals are currently **pending**:\n\n1. **FIRMS Alert: Lolo NF High Hotspot Count** — notification draft awaiting dispatch sign-off before distribution to field crews.\n2. **Lolo SE Fire — Type 3 Briefing** — IC briefing document awaiting final IC review (a prior version was already approved; this is an updated draft after the 520-acre growth update).\n\nOne approval was **rejected**: the Missoula County Red Flag Warning notification — the warning expired before approval was granted. The approver flagged it as stale and requested regeneration.",
        },
    ]
}
