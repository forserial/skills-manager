import { describe, expect, it } from "vitest";
import { getErrorKind, getErrorMessage, isAppError } from "./error";

describe("agent error kind", () => {
  // Serialized by the Rust side as ErrorKind::Agent.
  const agentError = { kind: "agent", message: "Could not find the `claude` CLI." };

  it("is recognised as a structured AppError", () => {
    expect(isAppError(agentError)).toBe(true);
    expect(getErrorKind(agentError)).toBe("agent");
    expect(getErrorMessage(agentError, "fallback")).toBe("Could not find the `claude` CLI.");
  });

  it("still rejects kinds the backend does not send", () => {
    expect(isAppError({ kind: "agent_failure", message: "x" })).toBe(false);
  });
});
