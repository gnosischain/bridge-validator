-- Add stage column to track processing phase for foreign chain execution retry.
-- 'home' = needs home-chain submitSignature (default, normal flow)
-- 'foreign' = home-chain submitSignature succeeded, only foreign execution pending
ALTER TABLE event_logs ADD COLUMN stage TEXT DEFAULT 'home';
