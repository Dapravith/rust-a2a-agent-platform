-- Phase 6B B7: `quality_assessment` is its own state (QA in `testing`, the
-- Quality Lead in `quality_assessment`), so supervision shows who is running
-- and recovery fails exactly the in-flight stage. Additive: every existing
-- status stays valid.

ALTER TABLE eng_workflows DROP CONSTRAINT eng_workflows_status_check;
ALTER TABLE eng_workflows ADD CONSTRAINT eng_workflows_status_check CHECK (status IN (
    'draft', 'awaiting_plan_approval', 'approved_for_execution', 'executing', 'reviewing',
    'testing', 'quality_assessment', 'awaiting_delivery_approval', 'changes_requested',
    'approved_for_delivery', 'rejected', 'failed', 'cancelled'));
