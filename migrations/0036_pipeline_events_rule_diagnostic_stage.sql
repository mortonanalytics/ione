ALTER TABLE pipeline_events DROP CONSTRAINT pipeline_events_stage_check;

ALTER TABLE pipeline_events ADD CONSTRAINT pipeline_events_stage_check
    CHECK (stage IN (
        'publish_started',
        'first_event',
        'first_signal',
        'first_survivor',
        'first_decision',
        'stall',
        'error',
        'rule_diagnostic'
    ));
