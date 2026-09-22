-- t_706da9df: make the DATABASE refuse a plaintext Google OAuth refresh token.
--
-- Same guard as 075 (provider_keys), 077 (integration_targets) and 078 (webhook_endpoints), for the
-- one credential that survives key rotation and is a standing grant on the tenant's Google account.
-- New writes must be sealed ('enc:v1:') or empty.
--
-- NOT VALID on purpose: rows written before the fix are exempt so this can be armed without a
-- rewrite, while every NEW write is checked immediately. No semicolon anywhere in these comments -
-- the in-app runner executes the whole file.
ALTER TABLE booking_calendars ADD CONSTRAINT booking_calendars_google_token_sealed CHECK (google_refresh_token IS NULL OR google_refresh_token = '' OR google_refresh_token LIKE 'enc:v1:%') NOT VALID;
