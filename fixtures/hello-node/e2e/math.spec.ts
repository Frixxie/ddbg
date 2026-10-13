import { expect, test } from "@playwright/test";
import { add } from "../src/math.ts";

test("adds in playwright", () => {
  expect(add(1, 1)).toBe(2);
});

test.describe("suite", () => {
  test("fails in playwright", () => {
    const sum = add(2, 2);
    expect(sum).toBe(5); // ddbg:playwright-failing-assert
  });
});
