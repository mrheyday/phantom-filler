-- Phantom Filler database initialization
-- This script runs on first container startup only.

-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Grant permissions
ALTER DATABASE phantom_filler SET timezone TO 'UTC';
