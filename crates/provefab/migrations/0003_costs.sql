-- Cost per stage (cost-aware routing, D74).
ALTER TABLE stage_runs ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stage_runs ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stage_runs ADD COLUMN actual_model TEXT;
ALTER TABLE stage_runs ADD COLUMN cost_usd REAL;
ALTER TABLE stage_runs ADD COLUMN quota_units REAL;
