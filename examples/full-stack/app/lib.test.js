import { test } from "node:test";
import assert from "node:assert/strict";
import { formatVisitResponse } from "./lib.js";

test("formats a visit response", () => {
  assert.deepEqual(formatVisitResponse(42, 100001, "cache"), {
    message: "Hello from Ratect!",
    visit_id: 42,
    total_visits: 100001,
    count_source: "cache",
  });
});
