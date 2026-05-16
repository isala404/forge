-- NOTIFY trigger for immediate worker wakeup on job enqueue.
-- Workers listen on `forge_jobs_available` and wake from poll sleep
-- when new jobs arrive, reducing dispatch latency from poll_interval to ~0.
-- Guards against no-op transitions (e.g. pending->pending re-enqueue)
-- by requiring either an INSERT or an actual status change.

CREATE OR REPLACE FUNCTION forge_notify_job_available() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.status = 'pending'
       AND (TG_OP = 'INSERT' OR OLD.status IS DISTINCT FROM NEW.status)
    THEN
        PERFORM pg_notify('forge_jobs_available', NEW.job_type);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER forge_job_enqueue_notify
    AFTER INSERT OR UPDATE OF status ON forge_jobs
    FOR EACH ROW EXECUTE FUNCTION forge_notify_job_available();
