import { describe, expect, it } from "vitest";
import { browserRequestUrl, formErrors, formValues, type ElicitationField } from "./elicitation";
const field = (name: string, kind: string, extra: Partial<ElicitationField> = {}): ElicitationField => ({name, kind, title: name, description: "", required: true, options: [], fields: [], default: null, minimum: null, maximum: null, min_length: null, max_length: null, min_items: null, max_items: null, format: null, ...extra});
describe("MCP form input", () => {
  it("keeps false and zero distinct from missing and validates nested fields", () => {
    const fields = [field("enabled", "boolean", {default: false}), field("count", "integer", {default: 0}), field("profile", "object", {fields: [field("owner", "string", {min_length: 1})]})];
    const values = formValues(fields, {profile: {owner: "Ada"}});
    expect(values).toEqual({enabled: false, count: 0, profile: {owner: "Ada"}});
    expect(formErrors(fields, values)).toEqual({});
    expect(formErrors(fields, formValues(fields, {}))).toHaveProperty("profile.owner");
  });
  it("omits untouched optional fields without inventing consent", () => {
    expect(formValues([field("enabled", "boolean", {required: false}), field("n", "number", {required: false})], {})).toEqual({});
    expect(formErrors([field("n", "integer", {minimum: 0})], {n: -1})).toHaveProperty("n");
  });
  it("refuses integer text that would change when serialized as a browser number", () => {
    const fields = [field("n", "integer")];
    for (const n of ["9007199254740993", "-9007199254740993", "9007199254740992", "1.5"]) {
      expect(formErrors(fields, formValues(fields, {n}))).toHaveProperty("n");
    }
    for (const n of ["9007199254740991", "-9007199254740991", "0"]) {
      expect(formErrors(fields, formValues(fields, {n}))).toEqual({});
    }
  });
  it("does not create an untouched optional object from child defaults", () => {
    const fields = [field("profile", "object", {required: false, fields: [field("owner", "string"), field("enabled", "boolean", {default: false})]})];
    expect(formValues(fields, {})).toEqual({});
    expect(formErrors(fields, formValues(fields, {}))).toEqual({});
    expect(formErrors(fields, formValues(fields, {profile: {enabled: true}}))).toHaveProperty("profile.owner");
  });
  it("never opens executable or credential-bearing URLs", () => {
    for (const url of ["javascript:alert(1)", "file:///tmp/test", "https://user:secret@example.com", "https://example.com/\nsecret"]) expect(browserRequestUrl(url)).toBeNull();
    expect(browserRequestUrl("https://example.com/auth")?.hostname).toBe("example.com");
  });
});
