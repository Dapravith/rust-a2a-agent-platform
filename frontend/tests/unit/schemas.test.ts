import assert from "node:assert/strict";
import { test } from "node:test";

import { draftSchema } from "../../src/features/workflows/schemas.ts";

const base = { title: "t", summary: "s", content: "c", assumptions: [] };

test("drafts without a mock flag (real model, older rows) are not labelled mock", () => {
  assert.equal(draftSchema.parse(base).mock, false);
});

test("the Worker's mock flag survives parsing", () => {
  assert.equal(draftSchema.parse({ ...base, mock: true }).mock, true);
});

test("a non-boolean mock flag is rejected, not coerced", () => {
  assert.equal(draftSchema.safeParse({ ...base, mock: "yes" }).success, false);
});
