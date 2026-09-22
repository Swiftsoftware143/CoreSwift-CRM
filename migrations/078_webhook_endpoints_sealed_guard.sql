-- t_6718dc86: make the DATABASE refuse a plaintext outbound webhook signing secret.
--
-- Same guard as 075 (provider_keys) and 077 (integration_targets), for the one secret column that
-- was still bound raw on both the INSERT and the PATCH. New writes must be sealed ('enc:v1:') or
-- empty.
--
-- NOT VALID on purpose: rows written before the fix are exempt so this can be armed without a
-- rewrite, while every NEW write is checked immediately. No semicolon anywhere in these comments -
-- the in-app runner executes the whole file.
ALTER TABLE webhook_endpoints ADD CONSTRAINT webhook_endpoints_secret_sealed CHECK (secret IS NULL OR secret = '' OR secret LIKE 'enc:v1:%') NOT VALID;
