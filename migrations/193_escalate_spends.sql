-- Escalation spends money, so the money rule applies to it.
--
-- The catalogue shipped escalate as a non-spending kind, which was wrong: escalating a ticket is what a
-- workspace does when it wants a person alerted, and on this platform that means a notification and — when
-- a workspace has Telnyx connected — an SMS, which is billed per message. A kind that can incur a cost must
-- be marked as spending one, or the limits that exist to govern spending would never see it.

UPDATE agent_action_kinds
   SET spends = true,
       description = 'Marks the ticket urgent, tells the account owner through the escalation ladder, and '
                     'may send an SMS when the workspace has Telnyx connected — which is billed, so the '
                     'money limits apply.'
 WHERE kind = 'escalate';
