-- ============================================================================
-- seed-b2b-automation.sql
-- B2B Flawless Follow-Up System — Phase 3
-- Seeds: message_templates, automation_rules, delayed_actions, followup_queue,
--        checklist_templates, checklist_stages
-- Target tenant: CoreSwift (abd8ad22-aa01-4642-9a9f-6bef6a03d85b)
-- ============================================================================

BEGIN;

DO $$
DECLARE
    v_tenant_id       CONSTANT uuid := 'abd8ad22-aa01-4642-9a9f-6bef6a03d85b';
    v_today           CONSTANT timestamptz := now();

    -- Staging IDs
    v_tpl_minute_1         uuid;
    v_tpl_day_2            uuid;
    v_tpl_day_4            uuid;
    v_tpl_booking_conf     uuid;
    v_tpl_no_booking       uuid;
    v_tpl_dir_abandoned    uuid;
    v_tpl_dir_logo         uuid;
    v_tpl_dir_hours        uuid;
    v_tpl_dir_keywords     uuid;
    v_tpl_dir_approve      uuid;
    v_tpl_dir_prepop       uuid;
    v_tpl_saas_no_activity uuid;
    v_tpl_saas_milestone   uuid;
    v_tpl_saas_churn       uuid;

    v_rule_agency_capture     uuid;
    v_rule_agency_d2          uuid;
    v_rule_agency_d4          uuid;
    v_rule_agency_booked      uuid;
    v_rule_agency_nurture     uuid;
    v_rule_dir_abandoned      uuid;
    v_rule_dir_profile_create uuid;
    v_rule_saas_registered    uuid;
    v_rule_saas_completed     uuid;
    v_rule_saas_trial         uuid;
    v_rule_saas_health        uuid;

    v_checklist_dir uuid;
BEGIN
    -- ======================================================================
    -- SEQUENCE A: AGENCY CLIENT ACQUISITION — Message Templates
    -- ======================================================================

    -- agency_minute_1
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'agency_minute_1',
        'email',
        'Thanks for downloading {{ lead_magnet_name }}',
        'Hi {{ first_name }},

Thanks for grabbing "{{ lead_magnet_name }}" — great choice!

The next step is simple: let''s hop on a quick 15-minute discovery call to map out exactly how we can help you {{ desired_outcome }}.

Schedule your call here: {{ booking_link }}

Talk soon,
{{ sender_name }}
',
        '{"lead_magnet_name": {"type": "string", "required": true}, "first_name": {"type": "string", "required": true}, "desired_outcome": {"type": "string", "required": false}, "booking_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_minute_1;

    -- agency_day_2 — Case study
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'agency_day_2',
        'email',
        'How {{ case_study_company }} achieved {{ case_study_result }}',
        'Hi {{ first_name }},

I wanted to share a quick story. {{ case_study_company }} was facing {{ case_study_problem }} — until they automated their follow-up with our system.

Watch the 90-second case study here:
{{ video_link }}

Same approach works for agencies of every size. Want us to run a quick audit on your current pipeline?

Reply or book a slot: {{ booking_link }}

Best,
{{ sender_name }}
',
        '{"first_name": {"type": "string", "required": true}, "case_study_company": {"type": "string", "required": true}, "case_study_result": {"type": "string", "required": true}, "case_study_problem": {"type": "string", "required": true}, "video_link": {"type": "string", "required": true}, "booking_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_day_2;

    -- agency_day_4 — Free audit offer
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'agency_day_4',
        'email',
        'Free audit: discover {{ missed_leads_count }} missed opportunities',
        'Hi {{ first_name }},

Over the last 4 days, your leads may have gone cold. Don''t worry — it happens to the best agencies.

I''d like to offer you a **free 15-minute Pipeline Audit**. We''ll look at:

1. How many leads fell through the cracks
2. Where your follow-up timing is off
3. One quick fix that could recover {{ estimated_revenue }} in pipeline value

No strings attached. Grab a time here:
{{ booking_link }}

Worth a shot,
{{ sender_name }}
',
        '{"first_name": {"type": "string", "required": true}, "missed_leads_count": {"type": "string", "required": false}, "estimated_revenue": {"type": "string", "required": false}, "booking_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_day_4;

    -- agency_booking_confirmed — Prep checklist for the call
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'agency_booking_confirmed',
        'email',
        'You''re confirmed — here''s what to expect',
        'Hi {{ first_name }},

Great, your discovery call is confirmed for {{ call_datetime }}.

**Quick prep checklist:**
✅ Think about your top 3 lead sources
✅ Jot down your current response time (ballpark)
✅ Any questions you want answered — bring them!

Meeting link: {{ call_link }}
Duration: 15 minutes

See you there,
{{ sender_name }}

P.S. If anything changes, just reply to reschedule.
',
        '{"first_name": {"type": "string", "required": true}, "call_datetime": {"type": "string", "required": true}, "call_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_booking_conf;

    -- agency_no_booking_nurture — Weekly B2B automation tips
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'agency_no_booking_nurture',
        'email',
        'B2B automation tip of the week: {{ tip_title }}',
        'Hi {{ first_name }},

Every week I share one actionable automation tip that agency owners use to close more deals with less effort.

**This week:** {{ tip_body }}

{{ tip_cta }}

Want to stop getting these? Hit reply and say "unsubscribe" — but you might miss the good stuff 😉

Cheers,
{{ sender_name }}

P.S. Still thinking about that discovery call? {{ booking_link }}
',
        '{"first_name": {"type": "string", "required": true}, "tip_title": {"type": "string", "required": true}, "tip_body": {"type": "string", "required": true}, "tip_cta": {"type": "string", "required": true}, "booking_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_no_booking;

    -- ======================================================================
    -- SEQUENCE A: AGENCY CLIENT ACQUISITION — Automation Rules
    -- ======================================================================

    -- Rule 1: On event type 'lead_captured' with source 'agency_lead_magnet'
    -- → queue followup with agency_minute_1 (hybrid channel) at T+1min
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'agency_lead_captured_instant',
        'On lead_captured from agency lead magnet → send immediate thank-you + booking CTA via email & SMS',
        'event',
        '{"event_type": "lead_captured", "source": "agency_lead_magnet", "unit": "agency"}'::jsonb,
        'queue_followup',
        jsonb_build_object(
            'template_name', 'agency_minute_1',
            'channel', 'hybrid',
            'delay_minutes', 1,
            'description', 'Send instant thank-you + booking link after lead magnet download'
        )
    )
    RETURNING id INTO v_rule_agency_capture;

    -- Rule 2: Same event → create delayed_action (no_event for call_booked, 48h) → queue agency_day_2
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'agency_lead_captured_day2_delayed',
        'On lead_captured → if no call booked in 48h, send case study email',
        'event',
        '{"event_type": "lead_captured", "source": "agency_lead_magnet", "unit": "agency"}'::jsonb,
        'create_delayed_action',
        jsonb_build_object(
            'condition_type', 'no_event',
            'condition_config', jsonb_build_object('event_type', 'call_booked', 'timeout_hours', 48),
            'timeout_action_template', 'agency_day_2',
            'timeout_action_channel', 'email',
            'description', 'Wait 48h for call booking, then send case study'
        )
    )
    RETURNING id INTO v_rule_agency_d2;

    -- Rule 3: Same event → create delayed_action (no_event, 96h) → queue agency_day_4
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'agency_lead_captured_day4_delayed',
        'On lead_captured → if no call booked in 96h, send free audit offer',
        'event',
        '{"event_type": "lead_captured", "source": "agency_lead_magnet", "unit": "agency"}'::jsonb,
        'create_delayed_action',
        jsonb_build_object(
            'condition_type', 'no_event',
            'condition_config', jsonb_build_object('event_type', 'call_booked', 'timeout_hours', 96),
            'timeout_action_template', 'agency_day_4',
            'timeout_action_channel', 'email',
            'description', 'Wait 96h for call booking, then send free audit offer'
        )
    )
    RETURNING id INTO v_rule_agency_d4;

    -- Rule 4: On event type 'call_booked' → queue followup with agency_booking_confirmed
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'agency_call_booked_confirmation',
        'On call_booked event → send prep checklist with call details',
        'event',
        '{"event_type": "call_booked", "unit": "agency"}'::jsonb,
        'queue_followup',
        jsonb_build_object(
            'template_name', 'agency_booking_confirmed',
            'channel', 'email',
            'delay_minutes', 0,
            'description', 'Send call prep checklist immediately after booking'
        )
    )
    RETURNING id INTO v_rule_agency_booked;

    -- Rule 5: On event type 'call_not_booked' or delayed_action timeout → enroll in weekly nurture
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'agency_no_booking_nurture',
        'On call_not_booked or delayed action timeout → enroll in weekly nurture campaign',
        'event',
        '{"event_type_in": ["call_not_booked", "delayed_action_timeout"], "unit": "agency"}'::jsonb,
        'enroll_nurture',
        jsonb_build_object(
            'template_name', 'agency_no_booking_nurture',
            'channel', 'email',
            'cadence_days', 7,
            'max_emails', 12,
            'description', 'Weekly nurture drip for leads who did not book a discovery call'
        )
    )
    RETURNING id INTO v_rule_agency_nurture;

    -- ======================================================================
    -- SEQUENCE B: DIRECTORY ONBOARDING — Message Templates
    -- ======================================================================

    -- dir_abandoned_15min
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_abandoned_15min',
        'sms',
        NULL,
        'Hey {{ first_name }}! Noticed you started your listing. Need help finishing? Reply or click here: {{ listing_link }}',
        '{"first_name": {"type": "string", "required": true}, "listing_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_abandoned;

    -- dir_stage1_logo
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_stage1_logo',
        'email',
        'Step 1 of 4: Upload your logo',
        'Hi {{ first_name }},

Your {{ business_name }} listing is almost ready! Let''s make it look great.

**Step 1 of 4: Upload Your Logo**

A professional logo makes your listing 3x more likely to get clicks.

➡️ Upload here: {{ onboarding_link }}

Takes 30 seconds. Let''s do this!
',
        '{"first_name": {"type": "string", "required": true}, "business_name": {"type": "string", "required": true}, "onboarding_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_logo;

    -- dir_stage2_hours
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_stage2_hours',
        'email',
        'Step 2 of 4: Set your business hours',
        'Hi {{ first_name }},

Great progress on {{ business_name }}! Now let customers know when you''re open.

**Step 2 of 4: Set Your Business Hours**

Accurate hours = happy customers. Let''s get this right.

➡️ Set hours here: {{ onboarding_link }}

Don''t forget to include weekend and holiday hours!
',
        '{"first_name": {"type": "string", "required": true}, "business_name": {"type": "string", "required": true}, "onboarding_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_hours;

    -- dir_stage3_keywords
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_stage3_keywords',
        'email',
        'Step 3 of 4: Add your keywords',
        'Hi {{ first_name }},

Almost there! Let''s help customers find {{ business_name }}.

**Step 3 of 4: Add Your Keywords**

Tell us what customers search for — we''ll optimize your listing for those terms.

➡️ Add keywords: {{ onboarding_link }}

Pro tip: Think of 5-10 terms your ideal customer would type into Google.
',
        '{"first_name": {"type": "string", "required": true}, "business_name": {"type": "string", "required": true}, "onboarding_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_keywords;

    -- dir_stage4_approve
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_stage4_approve',
        'email',
        'Step 4 of 4: Approve your first ad placement',
        'Hi {{ first_name }},

This is the final step — let''s get {{ business_name }} live!

**Step 4 of 4: Approve Your First Ad Placement**

Your listing is complete. Now approve your first ad to start getting visibility.

➡️ Review & approve: {{ onboarding_link }}

Once approved, your listing goes live immediately. 🚀
',
        '{"first_name": {"type": "string", "required": true}, "business_name": {"type": "string", "required": true}, "onboarding_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_approve;

    -- dir_prepopulated_preview
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'dir_prepopulated_preview',
        'email',
        'We built 90% of your listing — verify & go live',
        'Hi {{ first_name }},

Good news — we used publicly available data to pre-build most of your {{ business_name }} listing.

**✅ What''s already filled in:**
- Business name & address
- Phone number & website
- Category & description
- {{ prepopulated_count }} additional details

All you need to do is verify it''s correct and hit "Go Live."

➡️ Review your listing: {{ preview_link }}

Should take 2 minutes tops. Want to make changes? You can edit anything.
',
        '{"first_name": {"type": "string", "required": true}, "business_name": {"type": "string", "required": true}, "prepopulated_count": {"type": "string", "required": false}, "preview_link": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_dir_prepop;

    -- ======================================================================
    -- SEQUENCE B: DIRECTORY ONBOARDING — Checklist Template + Stages
    -- ======================================================================

    INSERT INTO checklist_templates (tenant_id, name, description, trigger_type, stage_count, days_per_stage)
    VALUES (
        v_tenant_id,
        'Directory Onboarding',
        '4-step guided onboarding for directory listings: logo, hours, keywords, approve',
        'profile_created',
        4,
        2
    )
    RETURNING id INTO v_checklist_dir;

    -- Stage 1: Logo
    INSERT INTO checklist_stages (template_id, stage_order, title, description, action_required, channel, message_template, delay_hours)
    VALUES (
        v_checklist_dir, 1,
        'Upload Logo',
        'Upload your business logo to make your listing stand out',
        'upload_logo',
        'email',
        'Step 1 of 4: Upload Your Logo

Hi {{ first_name }},

Your {{ business_name }} listing is almost ready! Let''s make it look great.

**Step 1 of 4: Upload Your Logo**

A professional logo makes your listing 3x more likely to get clicks.

➡️ Upload here: {{ onboarding_link }}

Takes 30 seconds. Let''s do this!',
        0
    );

    -- Stage 2: Hours
    INSERT INTO checklist_stages (template_id, stage_order, title, description, action_required, channel, message_template, delay_hours)
    VALUES (
        v_checklist_dir, 2,
        'Set Business Hours',
        'Set accurate business hours including weekends and holidays',
        'set_hours',
        'email',
        'Step 2 of 4: Set Your Business Hours

Hi {{ first_name }},

Great progress on {{ business_name }}! Now let customers know when you''re open.

**Step 2 of 4: Set Your Business Hours**

Accurate hours = happy customers. Let''s get this right.

➡️ Set hours here: {{ onboarding_link }}

Don''t forget to include weekend and holiday hours!',
        48
    );

    -- Stage 3: Keywords
    INSERT INTO checklist_stages (template_id, stage_order, title, description, action_required, channel, message_template, delay_hours)
    VALUES (
        v_checklist_dir, 3,
        'Add Keywords',
        'Add 5-10 keywords your ideal customers search for',
        'add_keywords',
        'email',
        'Step 3 of 4: Add Your Keywords

Hi {{ first_name }},

Almost there! Let''s help customers find {{ business_name }}.

**Step 3 of 4: Add Your Keywords**

Tell us what customers search for — we''ll optimize your listing for those terms.

➡️ Add keywords: {{ onboarding_link }}

Pro tip: Think of 5-10 terms your ideal customer would type into Google.',
        96
    );

    -- Stage 4: Approve
    INSERT INTO checklist_stages (template_id, stage_order, title, description, action_required, channel, message_template, delay_hours)
    VALUES (
        v_checklist_dir, 4,
        'Approve Ad Placement',
        'Review and approve your first ad placement to go live',
        'approve_ad',
        'email',
        'Step 4 of 4: Approve Your First Ad Placement

Hi {{ first_name }},

This is the final step — let''s get {{ business_name }} live!

**Step 4 of 4: Approve Your First Ad Placement**

Your listing is complete. Now approve your first ad to start getting visibility.

➡️ Review & approve: {{ onboarding_link }}

Once approved, your listing goes live immediately. 🚀',
        144
    );

    -- ======================================================================
    -- SEQUENCE B: DIRECTORY ONBOARDING — Automation Rules
    -- ======================================================================

    -- Rule: On event 'listing_abandoned' → queue followup with dir_abandoned_15min at T+15min
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'dir_listing_abandoned_recovery',
        'On listing_abandoned → send SMS recovery link within 15 minutes',
        'event',
        '{"event_type": "listing_abandoned", "unit": "directory"}'::jsonb,
        'queue_followup',
        jsonb_build_object(
            'template_name', 'dir_abandoned_15min',
            'channel', 'sms',
            'delay_minutes', 15,
            'description', 'Send SMS recovery link after listing abandonment'
        )
    )
    RETURNING id INTO v_rule_dir_abandoned;

    -- Rule: On event 'profile_created' with unit='directory' → delayed_action for prepopulated preview
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'dir_profile_created_prepopulated',
        'On profile_created with unit=directory → create delayed action to send prepopulated preview',
        'event',
        '{"event_type": "profile_created", "unit": "directory", "source": "onboarding"}'::jsonb,
        'create_delayed_action',
        jsonb_build_object(
            'condition_type', 'no_action',
            'condition_config', jsonb_build_object(
                'event_type_in', jsonb_build_array('listing_completed', 'listing_published'),
                'timeout_hours', 24
            ),
            'timeout_action_template', 'dir_prepopulated_preview',
            'timeout_action_channel', 'email',
            'description', 'Wait 24h for listing completion, then send prepopulated preview link'
        )
    )
    RETURNING id INTO v_rule_dir_profile_create;

    -- ======================================================================
    -- SEQUENCE C: SAAS TRIAL-TO-PAID — Message Templates
    -- ======================================================================

    -- saas_no_activity_24h
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'saas_no_activity_24h',
        'email',
        'Need help getting started? Watch this 3-minute tutorial',
        'Hi {{ first_name }},

We noticed you haven''t had a chance to explore {{ app_name }} yet. Totally normal — it''s got a lot of power under the hood.

Here''s a 3-minute screen share walking through the fastest way to set up your first automation:

🎥 {{ tutorial_link }}

Once you''ve watched it, you''ll be able to:
• Connect your first data source
• Set up a simple workflow in under 5 clicks
• See your first automation run

Need 1-on-1 help? Just reply.

Cheers,
{{ sender_name }}
',
        '{"first_name": {"type": "string", "required": true}, "app_name": {"type": "string", "required": true}, "tutorial_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_saas_no_activity;

    -- saas_first_milestone
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'saas_first_milestone',
        'email',
        '🎉 You saved {{ time_saved }} on your first automation!',
        'Hi {{ first_name }},

🎉 Congratulations! Your first automation just completed.

**Here''s what you achieved:**
• Automation: {{ automation_name }}
• Time saved: {{ time_saved }}
• {{ milestone_detail }}

This is just the beginning. The more automations you set up, the more time you reclaim.

**What''s next?**
Try inviting a teammate: {{ invite_link }}
Or set up your second automation: {{ new_automation_link }}

Keep going!
{{ sender_name }}
',
        '{"first_name": {"type": "string", "required": true}, "automation_name": {"type": "string", "required": true}, "time_saved": {"type": "string", "required": true}, "milestone_detail": {"type": "string", "required": false}, "invite_link": {"type": "string", "required": false}, "new_automation_link": {"type": "string", "required": false}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_saas_milestone;

    -- saas_3day_churn_risk
    INSERT INTO message_templates (tenant_id, name, channel, subject, body, variables)
    VALUES (
        v_tenant_id,
        'saas_3day_churn_risk',
        'email',
        'Your trial ends in 3 days — get a free concierge onboarding call',
        'Hi {{ first_name }},

Your {{ app_name }} trial ends in **3 days on {{ trial_end_date }}**.

Don''t let your automations go to waste! We''d love to give you a **free 1-on-1 concierge onboarding call** — we''ll set up your most impactful workflows together in 30 minutes.

🎯 What you''ll get:
• Your top 3 automations built for you
• Best practices from power users
• A custom roadmap for {{ business_name }}

➡️ Grab your spot: {{ booking_link }}

After the call, you''ll know exactly how {{ app_name }} pays for itself.

Talk soon,
{{ sender_name }}

P.S. If budget is the concern, reply and we''ll find a plan that works for you.
',
        '{"first_name": {"type": "string", "required": true}, "app_name": {"type": "string", "required": true}, "trial_end_date": {"type": "string", "required": true}, "business_name": {"type": "string", "required": false}, "booking_link": {"type": "string", "required": true}, "sender_name": {"type": "string", "required": true}}'::jsonb
    )
    RETURNING id INTO v_tpl_saas_churn;

    -- ======================================================================
    -- SEQUENCE C: SAAS TRIAL-TO-PAID — Automation Rules
    -- ======================================================================

    -- Rule: On event 'user_registered' with unit='saas' → delayed_action (no_action, 24h) → saas_no_activity_24h
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'saas_registered_no_activity_check',
        'On user_registered → create delayed action; if no activity in 24h, send tutorial',
        'event',
        '{"event_type": "user_registered", "unit": "saas"}'::jsonb,
        'create_delayed_action',
        jsonb_build_object(
            'condition_type', 'no_action',
            'condition_config', jsonb_build_object(
                'event_type_in', jsonb_build_array('data_source_connected', 'member_invited', 'automation_run'),
                'timeout_hours', 24
            ),
            'timeout_action_template', 'saas_no_activity_24h',
            'timeout_action_channel', 'email',
            'description', 'Wait 24h for key onboarding actions, then send tutorial link'
        )
    )
    RETURNING id INTO v_rule_saas_registered;

    -- Rule: On event 'automation_completed' → queue followup with saas_first_milestone
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'saas_automation_completed_milestone',
        'On automation_completed → send milestone celebration with dynamic data',
        'event',
        '{"event_type": "automation_completed", "unit": "saas"}'::jsonb,
        'queue_followup',
        jsonb_build_object(
            'template_name', 'saas_first_milestone',
            'channel', 'email',
            'delay_minutes', 5,
            'use_payload_data', true,
            'description', 'Send milestone email 5 min after first automation completes'
        )
    )
    RETURNING id INTO v_rule_saas_completed;

    -- Rule: On event 'trial_near_end' → queue followup with saas_3day_churn_risk
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'saas_trial_near_end_churn_prevention',
        'On trial_near_end event → send concierge onboarding offer',
        'event',
        '{"event_type": "trial_near_end", "unit": "saas"}'::jsonb,
        'queue_followup',
        jsonb_build_object(
            'template_name', 'saas_3day_churn_risk',
            'channel', 'email',
            'delay_minutes', 0,
            'description', 'Send concierge onboarding offer when trial nears end'
        )
    )
    RETURNING id INTO v_rule_saas_trial;

    -- Rule: On event 'account_health_dropping' → flag for concierge outreach
    INSERT INTO automation_rules (tenant_id, name, description, trigger_type, trigger_config, action_type, action_config)
    VALUES (
        v_tenant_id,
        'saas_health_drop_concierge_flag',
        'On account_health_dropping → flag for concierge outreach',
        'event',
        '{"event_type": "account_health_dropping", "unit": "saas"}'::jsonb,
        'flag_for_outreach',
        jsonb_build_object(
            'priority', 'high',
            'assignee_role', 'concierge',
            'description', 'Account health score dropping — concierge intervention recommended',
            'notification_slack_channel', '#saas-retention'
        )
    )
    RETURNING id INTO v_rule_saas_health;

    -- ======================================================================
    -- Summary
    -- ======================================================================

    RAISE NOTICE '✅ B2B Flawless Follow-Up seed complete for tenant %', v_tenant_id;
    RAISE NOTICE '   Templates created: agency=5, directory=6, saas=3 = total 14';
    RAISE NOTICE '   Rules created: agency=5, directory=2, saas=4 = total 11';
    RAISE NOTICE '   Checklist: Directory Onboarding (4 stages)';

END $$;

COMMIT;
