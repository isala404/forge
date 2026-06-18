-- v010: terminal 'dead' status (P1-4).
--
-- A job that exhausts its attempts inside a *.dlq queue must not re-home into an
-- unwatched .dlq.dlq. Instead it parks as 'dead' — a terminal status that depth()
-- already excludes (it counts only available/leased) and reclaim never touches.
ALTER TABLE forge_jobs DROP CONSTRAINT IF EXISTS forge_jobs_status_ck;
ALTER TABLE forge_jobs
    ADD CONSTRAINT forge_jobs_status_ck
    CHECK (status IN ('available', 'leased', 'done', 'dead'));
