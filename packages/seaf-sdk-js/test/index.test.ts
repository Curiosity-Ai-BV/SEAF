import { describe, expect, it } from "vitest";
import { frameworkName } from "../src/index";

describe("@seaf/sdk", () => {
  it("exports the framework name", () => {
    expect(frameworkName).toBe("Self-Evolving Application Framework");
  });
});
