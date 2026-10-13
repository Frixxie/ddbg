import { describe, expect, test } from "vitest";
import { add, addPoints } from "../src/math.ts";

test("adds numbers", () => {
  expect(add(2, 3)).toBe(5);
});

describe("points", () => {
  test("adds points", () => {
    const p = addPoints({ x: 1, y: 2 }, { x: 3, y: 4 });
    expect(p).toEqual({ x: 4, y: 6 });
  });

  test("fails on purpose", () => {
    const sum = add(2, 2);
    expect(sum).toBe(5); // ddbg:vitest-failing-assert
  });
});
