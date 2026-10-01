import { describe, expect, it } from "vitest";
import {
  draftWithInsert,
  skillBlocksForText,
  slashChoices,
  slashContextAt,
  type ComposerCommand,
} from "./composer";

const commands: ComposerCommand[] = [
  {
    name: "mode",
    description: "permission mode",
    options: [
      { value: "auto", label: "Auto" },
      { value: "auto-review", label: "Auto review" },
    ],
  },
  { name: "skill-creator", description: "create a skill", skill_path: "/skills/create.md" },
  {
    name: "SKILL-CREATOR",
    description: "duplicate scope",
    skill_path: "/skills/duplicate.md",
  },
];

describe("composer slash discovery", () => {
  it("finds commands after ordinary inline whitespace", () => {
    const draft = "hello — use /skill";
    const context = slashContextAt(draft, draft.length, commands);
    expect(context).toEqual({ kind: "command", start: 12, text: "/skill" });
    expect(slashChoices(context, commands).map((choice) => choice.label)).toEqual([
      "/skill-creator",
    ]);
  });

  it("does not treat a path fragment as a command", () => {
    const draft = "open src/skill";
    expect(slashContextAt(draft, draft.length, commands)).toBeNull();
  });

  it("completes native command arguments", () => {
    const draft = "please use /mode auto-r";
    const context = slashContextAt(draft, draft.length, commands);
    expect(context?.kind).toBe("argument");
    expect(slashChoices(context, commands).map((choice) => choice.label)).toEqual([
      "Auto review",
    ]);
  });
});

describe("Codex skill blocks", () => {
  it("promotes exact inline slash skills and ignores path-like text", () => {
    expect(skillBlocksForText("hello /skill-creator please", commands)).toEqual([
      { type: "skill", name: "skill-creator", path: "/skills/create.md" },
    ]);
    expect(skillBlocksForText("open src/skill-creator", commands)).toEqual([]);
  });

  it("de-duplicates repeated skill tokens", () => {
    expect(skillBlocksForText("/skill-creator then /skill-creator", commands)).toHaveLength(1);
  });

  it("de-duplicates overlapping catalog scopes", () => {
    const context = slashContextAt("/skill", 6, commands);
    expect(slashChoices(context, commands).map((choice) => choice.label)).toEqual([
      "/skill-creator",
    ]);
    expect(skillBlocksForText("/skill-creator", commands)).toEqual([
      { type: "skill", name: "skill-creator", path: "/skills/create.md" },
    ]);
  });
});

describe("draftWithInsert", () => {
  it("joins inline text after a space", () => {
    expect(draftWithInsert("", "@a.ts ", "inline")).toBe("@a.ts ");
    expect(draftWithInsert("see", "@a.ts ", "inline")).toBe("see @a.ts ");
    expect(draftWithInsert("see ", "@a.ts ", "inline")).toBe("see @a.ts ");
  });
  it("starts a block on its own paragraph", () => {
    const quote = "> q\n\n";
    expect(draftWithInsert("", quote, "block")).toBe(quote);
    expect(draftWithInsert("  \n", quote, "block")).toBe(quote);
    expect(draftWithInsert("why", quote, "block")).toBe(`why\n\n${quote}`);
    expect(draftWithInsert("why  ", quote, "block")).toBe(`why\n\n${quote}`);
    expect(draftWithInsert("why\n", quote, "block")).toBe(`why\n\n${quote}`);
    expect(draftWithInsert(`> p\n\n`, quote, "block")).toBe(`> p\n\n${quote}`);
  });
  it("puts a returned message above the draft in progress, as its own paragraph", () => {
    expect(draftWithInsert("", "it did not arrive", "above")).toBe("it did not arrive");
    expect(draftWithInsert("  \n", "it did not arrive", "above")).toBe("it did not arrive");
    // What is being written now stays last, exactly as typed, where the caret is.
    expect(draftWithInsert("and another thi", "it did not arrive", "above")).toBe("it did not arrive\n\nand another thi");
    expect(draftWithInsert("\nnext ", "first\n\nsecond\n", "above")).toBe("first\n\nsecond\n\nnext ");
  });
});
