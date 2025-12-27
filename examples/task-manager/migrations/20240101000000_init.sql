-- Task Manager Schema
-- This migration creates all tables for the Task Manager example app.

-- Enums
CREATE TYPE task_status AS ENUM ('backlog', 'todo', 'in_progress', 'in_review', 'done', 'cancelled');
CREATE TYPE task_priority AS ENUM ('low', 'medium', 'high', 'urgent');
CREATE TYPE team_role AS ENUM ('owner', 'admin', 'member', 'guest');
CREATE TYPE project_status AS ENUM ('active', 'on_hold', 'completed', 'archived');

-- Teams
CREATE TABLE teams (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(255) NOT NULL UNIQUE,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_teams_slug ON teams(slug);

-- Users
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    avatar_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users(email);

-- Team Members (many-to-many)
CREATE TABLE team_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role team_role NOT NULL DEFAULT 'member',
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(team_id, user_id)
);

CREATE INDEX idx_team_members_team ON team_members(team_id);
CREATE INDEX idx_team_members_user ON team_members(user_id);

-- Projects
CREATE TABLE projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    status project_status NOT NULL DEFAULT 'active',
    color VARCHAR(7),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_projects_team ON projects(team_id);
CREATE INDEX idx_projects_status ON projects(status);

-- Tasks
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title VARCHAR(500) NOT NULL,
    description TEXT,
    status task_status NOT NULL DEFAULT 'backlog',
    priority task_priority NOT NULL DEFAULT 'medium',
    assignee_id UUID REFERENCES users(id) ON DELETE SET NULL,
    due_date TIMESTAMPTZ,
    position INTEGER NOT NULL DEFAULT 0,
    created_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tasks_project ON tasks(project_id);
CREATE INDEX idx_tasks_status ON tasks(status);
CREATE INDEX idx_tasks_assignee ON tasks(assignee_id);
CREATE INDEX idx_tasks_due_date ON tasks(due_date) WHERE due_date IS NOT NULL;
CREATE INDEX idx_tasks_position ON tasks(project_id, position);

-- Comments
CREATE TABLE comments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES users(id),
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_comments_task ON comments(task_id);

-- Attachments
CREATE TABLE attachments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    uploaded_by UUID NOT NULL REFERENCES users(id),
    filename VARCHAR(255) NOT NULL,
    file_size BIGINT NOT NULL,
    content_type VARCHAR(100) NOT NULL,
    storage_key VARCHAR(500) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_attachments_task ON attachments(task_id);

-- Labels
CREATE TABLE labels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name VARCHAR(50) NOT NULL,
    color VARCHAR(7) NOT NULL,
    UNIQUE(project_id, name)
);

CREATE INDEX idx_labels_project ON labels(project_id);

-- Task Labels (many-to-many)
CREATE TABLE task_labels (
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    label_id UUID NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, label_id)
);

-- Invitations
CREATE TABLE invitations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    email VARCHAR(255) NOT NULL,
    role team_role NOT NULL DEFAULT 'member',
    inviter_id UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    UNIQUE(team_id, email)
);

CREATE INDEX idx_invitations_email ON invitations(email);
CREATE INDEX idx_invitations_expires ON invitations(expires_at) WHERE accepted_at IS NULL;

-- Enable reactivity for all tables
SELECT forge_enable_reactivity('teams');
SELECT forge_enable_reactivity('users');
SELECT forge_enable_reactivity('team_members');
SELECT forge_enable_reactivity('projects');
SELECT forge_enable_reactivity('tasks');
SELECT forge_enable_reactivity('comments');
SELECT forge_enable_reactivity('attachments');
SELECT forge_enable_reactivity('labels');
SELECT forge_enable_reactivity('invitations');

-- Seed data for testing
INSERT INTO users (id, email, name) VALUES
    ('00000000-0000-0000-0000-000000000001', 'alice@example.com', 'Alice Developer'),
    ('00000000-0000-0000-0000-000000000002', 'bob@example.com', 'Bob Manager'),
    ('00000000-0000-0000-0000-000000000003', 'carol@example.com', 'Carol Designer');

INSERT INTO teams (id, name, slug, description) VALUES
    ('10000000-0000-0000-0000-000000000001', 'Acme Corp', 'acme-corp', 'Building the future');

INSERT INTO team_members (team_id, user_id, role) VALUES
    ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'owner'),
    ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'admin'),
    ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000003', 'member');

INSERT INTO projects (id, team_id, name, description) VALUES
    ('20000000-0000-0000-0000-000000000001', '10000000-0000-0000-0000-000000000001', 'Website Redesign', 'Complete website overhaul');

INSERT INTO tasks (id, project_id, title, status, priority, position, created_by) VALUES
    ('30000000-0000-0000-0000-000000000001', '20000000-0000-0000-0000-000000000001', 'Design new homepage', 'in_progress', 'high', 1, '00000000-0000-0000-0000-000000000001'),
    ('30000000-0000-0000-0000-000000000002', '20000000-0000-0000-0000-000000000001', 'Implement navigation', 'todo', 'medium', 2, '00000000-0000-0000-0000-000000000001'),
    ('30000000-0000-0000-0000-000000000003', '20000000-0000-0000-0000-000000000001', 'Add contact form', 'backlog', 'low', 3, '00000000-0000-0000-0000-000000000002');
