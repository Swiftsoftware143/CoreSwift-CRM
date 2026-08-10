-- 019_seed_data.sql
-- Default seed data for new tenants (pipelines, tags, score rules, lists)

-- Insert default pipelines
INSERT INTO pipelines (id, tenant_id, name, description, is_default)
SELECT 
    uuid_generate_v4(), t.id, 'Sales Pipeline', 'Standard sales process from lead to won/lost', true
FROM tenants t
WHERE NOT EXISTS (
    SELECT 1 FROM pipelines p WHERE p.tenant_id = t.id AND p.is_default = true
);

INSERT INTO pipelines (id, tenant_id, name, description, is_default)
SELECT 
    uuid_generate_v4(), t.id, 'Client Onboarding', 'New client setup and activation process', false
FROM tenants t
WHERE NOT EXISTS (
    SELECT 1 FROM pipelines p WHERE p.tenant_id = t.id AND p.name = 'Client Onboarding'
);

-- Insert default stages for Sales Pipeline
-- New Lead → Contacted → Qualified → Appointment Set → Proposal Sent → Won/Lost
INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'New Lead', '#6B7280', 0, 5
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 0);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Contacted', '#3B82F6', 1, 10
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 1);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Qualified', '#8B5CF6', 2, 25
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 2);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Appointment Set', '#F59E0B', 3, 40
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 3);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Proposal Sent', '#10B981', 4, 60
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 4);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability, is_won)
SELECT 
    uuid_generate_v4(), p.id, 'Won', '#059669', 5, 100, true
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 5);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability, is_lost)
SELECT 
    uuid_generate_v4(), p.id, 'Lost', '#DC2626', 6, 0, true
FROM pipelines p WHERE p.name = 'Sales Pipeline'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 6);

-- Insert default stages for Client Onboarding
INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'New Client', '#6B7280', 0, 10
FROM pipelines p WHERE p.name = 'Client Onboarding'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 0);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Documents Received', '#3B82F6', 1, 25
FROM pipelines p WHERE p.name = 'Client Onboarding'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 1);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Setup In Progress', '#8B5CF6', 2, 50
FROM pipelines p WHERE p.name = 'Client Onboarding'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 2);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Active', '#10B981', 3, 100
FROM pipelines p WHERE p.name = 'Client Onboarding'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 3);

INSERT INTO pipeline_stages (id, pipeline_id, name, color, sort_order, probability)
SELECT 
    uuid_generate_v4(), p.id, 'Retention', '#059669', 4, 100
FROM pipelines p WHERE p.name = 'Client Onboarding'
AND NOT EXISTS (SELECT 1 FROM pipeline_stages ps WHERE ps.pipeline_id = p.id AND ps.sort_order = 4);

-- Insert default tag categories
INSERT INTO tag_categories (id, tenant_id, name, color)
SELECT uuid_generate_v4(), t.id, 'Sources', '#3B82F6'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Sources');

INSERT INTO tag_categories (id, tenant_id, name, color)
SELECT uuid_generate_v4(), t.id, 'Status', '#F59E0B'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Status');

INSERT INTO tag_categories (id, tenant_id, name, color)
SELECT uuid_generate_v4(), t.id, 'Behavior', '#8B5CF6'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Behavior');

-- Insert default tags
INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id, 
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Sources'),
    'Facebook', '#1877F2'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Facebook');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Sources'),
    'Google', '#4285F4'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Google');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Sources'),
    'TikTok', '#000000'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'TikTok');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Status'),
    'Hot Lead', '#DC2626'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Hot Lead');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Status'),
    'Cold Lead', '#6B7280'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Cold Lead');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Behavior'),
    'Engaged', '#10B981'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Engaged');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id,
    (SELECT tc.id FROM tag_categories tc WHERE tc.tenant_id = t.id AND tc.name = 'Behavior'),
    'Unresponsive', '#DC2626'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Unresponsive');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id, NULL, 'Booked Call', '#F59E0B'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Booked Call');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id, NULL, 'Newsletter', '#3B82F6'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Newsletter');

INSERT INTO tags (id, tenant_id, category_id, name, color)
SELECT uuid_generate_v4(), t.id, NULL, 'Client', '#059669'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM tags tg WHERE tg.tenant_id = t.id AND tg.name = 'Client');

-- Insert default score rules
INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Email Opened', 'email.opened', 'Contact opened an email', 5, 'add'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'email.opened');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Email Clicked', 'email.clicked', 'Contact clicked a link in email', 15, 'add'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'email.clicked');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Email Replied', 'email.replied', 'Contact replied to an email', 25, 'add'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'email.replied');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Booked Call', 'call.booked', 'Contact booked a sales call', 50, 'add'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'call.booked');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Proposal Requested', 'proposal.requested', 'Contact requested a proposal', 75, 'add'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'proposal.requested');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'No Activity 30 Days', 'inactivity.30days', 'No activity for 30 days', 20, 'subtract'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'inactivity.30days');

INSERT INTO score_rules (id, tenant_id, name, event_type, description, points, direction)
SELECT uuid_generate_v4(), t.id, 'Unsubscribed', 'email.unsubscribed', 'Contact unsubscribed from emails', 100, 'subtract'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM score_rules sr WHERE sr.tenant_id = t.id AND sr.event_type = 'email.unsubscribed');

-- Insert default lists
INSERT INTO lists (id, tenant_id, name, description, list_type)
SELECT uuid_generate_v4(), t.id, 'Newsletter Subscribers', 'Contacts subscribed to newsletter', 'static'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM lists l WHERE l.tenant_id = t.id AND l.name = 'Newsletter Subscribers');

INSERT INTO lists (id, tenant_id, name, description, list_type)
SELECT uuid_generate_v4(), t.id, 'Cold Leads', 'Contacts with cold lead score', 'dynamic'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM lists l WHERE l.tenant_id = t.id AND l.name = 'Cold Leads');

INSERT INTO lists (id, tenant_id, name, description, list_type)
SELECT uuid_generate_v4(), t.id, 'Hot Leads', 'Contacts with high lead score', 'dynamic'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM lists l WHERE l.tenant_id = t.id AND l.name = 'Hot Leads');

INSERT INTO lists (id, tenant_id, name, description, list_type)
SELECT uuid_generate_v4(), t.id, 'Retargeting Audience', 'Contacts for ad retargeting campaigns', 'dynamic'
FROM tenants t
WHERE NOT EXISTS (SELECT 1 FROM lists l WHERE l.tenant_id = t.id AND l.name = 'Retargeting Audience');
