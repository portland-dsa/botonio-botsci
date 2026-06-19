-- The cluster-wide group role the migrations grant to (see
-- crates/persistence/migrations/0001_init.sql, which does `GRANT ... TO botonio_app`).
-- On the production box this role is created by scripts/setup/db-bootstrap.py;
-- here it is created so a fresh throwaway cluster can apply the migrations cleanly.
-- NOLOGIN: it is only a privilege-bearing group, never a role anything connects as.
CREATE ROLE botonio_app NOLOGIN;
