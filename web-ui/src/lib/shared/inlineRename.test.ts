import { describe, expect, it, vi } from "vitest";
import { InlineRename, renameFocus, type InlineRenameSpec } from "./inlineRename.svelte";

type Target = { name: string };

function make(over: Partial<InlineRenameSpec<Target>> = {}) {
  const submit = vi.fn<InlineRenameSpec<Target>["submit"]>(async () => {});
  const onClose = vi.fn();
  const r = new InlineRename<Target>({
    check: (t, name) => (name === t.name ? "unchanged" : name.includes("/") ? { invalid: "name cannot contain /" } : "submit"),
    submit,
    errorText: (e) => (e instanceof Error ? e.message : "failed"),
    onClose,
    ...over,
  });
  return { r, submit, onClose };
}

const key = (k: string) => ({ key: k, preventDefault: vi.fn(), stopPropagation: vi.fn() }) as unknown as KeyboardEvent;

describe("InlineRename", () => {
  it("begins with the draft and no error", () => {
    const { r } = make();
    r.begin({ name: "a.txt" }, "a.txt");
    expect(r.target).toEqual({ name: "a.txt" });
    expect(r.draft).toBe("a.txt");
    expect(r.error).toBeNull();
  });

  it("submits the trimmed name and closes on success", async () => {
    const { r, submit, onClose } = make();
    const t = { name: "a.txt" };
    r.begin(t, "a.txt");
    r.draft = "  b.txt ";
    await r.commit();
    expect(submit).toHaveBeenCalledWith(t, "b.txt", expect.any(Function));
    expect(r.target).toBeNull();
    expect(onClose).toHaveBeenCalledWith(t);
  });

  it("a blank or unchanged name closes without submitting", async () => {
    const { r, submit } = make();
    r.begin({ name: "a" }, "   ");
    await r.commit();
    expect(r.target).toBeNull();
    r.begin({ name: "a" }, "a");
    await r.commit();
    expect(r.target).toBeNull();
    expect(submit).not.toHaveBeenCalled();
  });

  it("an invalid name shows the error on Enter but cancels on blur", async () => {
    const { r, submit } = make();
    r.begin({ name: "a" }, "x/y");
    await r.commit();
    expect(r.error).toBe("name cannot contain /");
    expect(r.target).not.toBeNull();
    r.blur();
    await Promise.resolve();
    expect(r.target).toBeNull();
    expect(submit).not.toHaveBeenCalled();
  });

  it("a failed submit keeps the edit open with the error, and may retry", async () => {
    const { r, submit } = make();
    submit.mockRejectedValueOnce(new Error("exists"));
    r.begin({ name: "a" }, "b");
    await r.commit();
    expect(r.error).toBe("exists");
    expect(r.target).not.toBeNull();
    await r.commit();
    expect(submit).toHaveBeenCalledTimes(2);
    expect(r.target).toBeNull();
  });

  it("ignores a second commit while the first is in flight (Enter, then blur)", async () => {
    let resolve!: () => void;
    const { r, submit } = make({});
    submit.mockImplementation(() => new Promise<void>((done) => (resolve = done)));
    r.begin({ name: "a" }, "b");
    r.keydown(key("Enter"));
    r.blur();
    r.keydown(key("Enter"));
    expect(submit).toHaveBeenCalledTimes(1);
    resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(r.target).toBeNull();
  });

  it("Escape cancels first, so the following blur commits nothing", async () => {
    const { r, submit, onClose } = make();
    r.begin({ name: "a" }, "b");
    const e = key("Escape");
    r.keydown(e);
    expect(e.preventDefault).toHaveBeenCalled();
    expect(e.stopPropagation).toHaveBeenCalled();
    r.blur();
    await Promise.resolve();
    expect(submit).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("other keys only stop propagation", () => {
    const { r } = make();
    r.begin({ name: "a" }, "b");
    const e = key("x");
    r.keydown(e);
    expect(e.stopPropagation).toHaveBeenCalled();
    expect(e.preventDefault).not.toHaveBeenCalled();
    expect(r.target).not.toBeNull();
  });

  it("an early close() lets the site keep working, and a newer edit survives the old submit", async () => {
    let resolve!: () => void;
    const { r, submit } = make();
    submit.mockImplementation((_t, _n, close) => {
      close();
      return new Promise<void>((done) => (resolve = done));
    });
    r.begin({ name: "a" }, "b");
    const pending = r.commit();
    expect(r.target).toBeNull();
    r.begin({ name: "c" }, "c2");
    resolve();
    await pending;
    expect(r.target).toEqual({ name: "c" });
    // The newer edit is not blocked by the older in-flight guard.
    submit.mockImplementation(async () => {});
    await r.commit();
    expect(submit).toHaveBeenCalledTimes(2);
  });

  it("a failure after the edit was cancelled shows nothing", async () => {
    let reject!: (e: unknown) => void;
    const { r, submit } = make();
    submit.mockImplementation(() => new Promise<void>((_, no) => (reject = no)));
    r.begin({ name: "a" }, "b");
    const pending = r.commit();
    r.cancel();
    reject(new Error("late"));
    await pending;
    expect(r.error).toBeNull();
  });
});

describe("renameFocus", () => {
  it("focuses and selects the stem only when asked", () => {
    const node = { value: "report.final.pdf", focus: vi.fn(), setSelectionRange: vi.fn() };
    renameFocus(node as unknown as HTMLInputElement, true);
    expect(node.focus).toHaveBeenCalled();
    expect(node.setSelectionRange).toHaveBeenCalledWith(0, 12);
    const plain = { value: "x", focus: vi.fn(), setSelectionRange: vi.fn() };
    renameFocus(plain as unknown as HTMLInputElement, false);
    expect(plain.setSelectionRange).not.toHaveBeenCalled();
  });
});
