import { expect, test } from "vitest";
import { greet } from "./index.js";

test("greets", () => {
  expect(greet()).toBe("Hello, world!");
});
