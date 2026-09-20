-- Hub Integration Center — catalogue rows + indexes the hub's own center renders.
--
-- WHY: the hub's Integration Center must render its native sister connectors from
-- `available_providers` (the catalogue is the single source of truth) instead of a
-- hardcoded array in the SPA. Keys are the connector slugs that `/api/native/apps`
-- already uses, so the catalogue row and the live connection status join 1:1.
--
-- IDEMPOTENT: re-runnable (ON CONFLICT (key) DO UPDATE, CREATE INDEX IF NOT EXISTS).

INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
VALUES
    ('adaswift',            'AdaSwift Console',        'Client portal — scan reports, proposals and client leads flow into CoreSwift.',        true, '[]'::jsonb, 'briefcase'),
    ('cheatlayer',          'CheatLayer',              'RPA automation engine — browser automation captures leads into CoreSwift.',            true, '[]'::jsonb, 'robot'),
    ('funnelswift',         'FunnelSwift',             'Sales funnels — Kinetic card / funnel leads flow into CoreSwift.',                     true, '[]'::jsonb, 'filter'),
    ('workflowswift',       'WorkflowSwift Automation','n8n workflow automation — workflow-captured leads flow into CoreSwift.',               true, '[]'::jsonb, 'workflow'),
    ('missedcall-responder','MissedCall Responder',    'Missed-call answering — every recovered caller becomes a CoreSwift contact.',          true, '[]'::jsonb, 'phone'),
    ('multi-directory',     'Multi-Directory',         'Business directory engine — directory-sourced leads flow into CoreSwift.',             true, '[]'::jsonb, 'list')
ON CONFLICT (key) DO UPDATE
    SET name        = EXCLUDED.name,
        description = EXCLUDED.description,
        icon        = EXCLUDED.icon;

-- Source attribution reporting: "leads by capture app" filters on the source app.
-- The canonical value lives in contacts.metadata->>'source_app' (written by
-- src/external_api.rs); contacts.source carries the same slug for the plain column path.
CREATE INDEX IF NOT EXISTS idx_contacts_source_app
    ON contacts ((metadata ->> 'source_app'));

-- The hub's per-source rollup filters tenant + recency.
CREATE INDEX IF NOT EXISTS idx_contacts_tenant_source_created
    ON contacts (tenant_id, source, created_at DESC);
