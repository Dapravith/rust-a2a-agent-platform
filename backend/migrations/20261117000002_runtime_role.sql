-- Phase 3: least-privilege runtime access.
--
-- `a2a_app` is a NOLOGIN group role holding exactly what the running API
-- needs: read/insert/update on the three tables, read on the migration
-- history (to verify the schema at startup). No DELETE, TRUNCATE, DDL or
-- ownership. Operators create the login role themselves, so no password is
-- ever committed (see docs/PHASE3.md):
--
--   CREATE ROLE a2a_api LOGIN PASSWORD '<generated>' IN ROLE a2a_app;
--
-- Migrations keep running as the owner (`api migrate`, DATABASE_MIGRATION_URL).
-- Roles are cluster-wide, so creation is idempotent and race-safe (parallel
-- test databases run this concurrently).

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'a2a_app') THEN
        CREATE ROLE a2a_app NOLOGIN;
    END IF;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END
$$;

GRANT USAGE ON SCHEMA public TO a2a_app;
GRANT SELECT, INSERT, UPDATE ON workflows, agent_runs, artifacts TO a2a_app;
GRANT SELECT ON _sqlx_migrations TO a2a_app;

-- RLS is enabled with no policies (initial migration), which denies every
-- role except the table owner. The runtime role is not the owner, so it
-- needs explicit policies; no other role gets any. (DELETE is not granted,
-- so FOR ALL here cannot delete either.)
CREATE POLICY a2a_app_workflows  ON workflows  FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_agent_runs ON agent_runs FOR ALL TO a2a_app USING (true) WITH CHECK (true);
CREATE POLICY a2a_app_artifacts  ON artifacts  FOR ALL TO a2a_app USING (true) WITH CHECK (true);

-- Supabase grants its Data API roles broad default privileges on `public`.
-- RLS (enabled, no policies) already denies them; revoking the table
-- privileges as well means a future RLS mistake still exposes nothing.
-- These roles do not exist on plain Postgres, hence the guard.
DO $$
DECLARE
    r text;
BEGIN
    FOREACH r IN ARRAY ARRAY['anon', 'authenticated'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('REVOKE ALL ON workflows, agent_runs, artifacts FROM %I', r);
        END IF;
    END LOOP;
END
$$;
