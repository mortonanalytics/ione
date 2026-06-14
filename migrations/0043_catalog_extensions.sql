-- Trigram fuzzy matching for the federated catalog search (migration 0044).
-- pg_trgm backs the `similarity()` / `%` operators used by CatalogRepo::search.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
