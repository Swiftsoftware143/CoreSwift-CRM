-- t_477d46c2: make the DATABASE refuse a plaintext outbound webhook credential.
--
-- The create path seals the value in the application since 2026-09-22, but application sealing is
-- only half the guard - one manual UPDATE, or the next handler that forgets, puts a live key back
-- in the clear. New writes must be sealed ('enc:v1:') or empty.
--
-- NOT VALID on purpose: rows written before the fix are exempt so this can be armed without a
-- rewrite, while every NEW write is checked immediately. No semicolon anywhere in these comments -
-- the in-app runner executes the whole file.
ALTER TABLE integration_targets ADD CONSTRAINT integration_targets_api_key_sealed CHECK (api_key IS NULL OR api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;
