import { describe, expect, it } from "vitest";
import type { AskpassPrompt } from "../net/native";
import { askpassPresentation } from "./askpassPresentation";

const fingerprint = `SHA256:${"A".repeat(43)}`;
const prompt: AskpassPrompt = {
  id: 7,
  alias: "cluster",
  source: { type: "local" },
  prompt: "The authenticity of host synthetic.invalid can't be established.",
  kind: { type: "host_key", host: "synthetic.invalid", fingerprint },
};

describe("native first-host confirmation", () => {
  it("uses only explicit original-native metadata", () => {
    expect(askpassPresentation(prompt)).toEqual({
      type: "host_key", host: "synthetic.invalid", fingerprint,
    });
    expect(askpassPresentation({ ...prompt, kind: undefined })).toEqual({ type: "secret" });
    expect(askpassPresentation({ ...prompt, source: undefined })).toEqual({ type: "unsupported" });
    expect(askpassPresentation({
      ...prompt, source: { type: "keeper", host_id: "host", keeper_prompt_id: "mfa" },
    })).toEqual({ type: "unsupported" });
  });

  it("refuses damaged metadata instead of asking for a hidden yes", () => {
    for (const kind of [
      null,
      { type: "future", host: "synthetic.invalid", fingerprint },
      { type: "host_key", host: "host\nother", fingerprint },
      { type: "host_key", host: "A".repeat(1025), fingerprint },
      { type: "host_key", host: "host", fingerprint: "SHA256:damaged" },
    ]) {
      expect(askpassPresentation({ ...prompt, kind } as AskpassPrompt)).toEqual({ type: "unsupported" });
    }
  });
});
