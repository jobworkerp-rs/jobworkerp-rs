-- Supports JobResult filtering, aggregation, bulk deletion, and per-worker recency queries.
CREATE INDEX IF NOT EXISTS idx_job_result_status ON job_result(status);
CREATE INDEX IF NOT EXISTS idx_job_result_start_time ON job_result(start_time);
CREATE INDEX IF NOT EXISTS idx_job_result_end_time ON job_result(end_time);
CREATE INDEX IF NOT EXISTS idx_job_result_end_status ON job_result(end_time, status);
CREATE INDEX IF NOT EXISTS idx_job_result_worker_end ON job_result(worker_id, end_time DESC);
