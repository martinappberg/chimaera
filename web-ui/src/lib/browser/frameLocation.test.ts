import { describe, expect, it } from "vitest";
import { inAppPath, onRootForm, pageAddress, proxySessionOf } from "./frameLocation";

const at = (href: string) => new URL(href, "http://127.0.0.1:7777");
const base = "/proxy/abc";

describe("inAppPath", () => {
  it("strips the proxy prefix", () => {
    expect(inAppPath(at("/proxy/abc/lab/tree?x=1#h"), base)).toBe("/lab/tree?x=1#h");
    expect(inAppPath(at("/proxy/abc"), base)).toBe("/");
  });
  it("keeps a rescued root-form path as the app's own", () => {
    expect(inAppPath(at("/lab/tree/a.ipynb"), base)).toBe("/lab/tree/a.ipynb");
  });
  it("refuses about:blank and another session's page", () => {
    expect(inAppPath(at("about:blank"), base)).toBeNull();
    expect(inAppPath(at("/proxy/old/lab"), base)).toBeNull();
  });
});

describe("onRootForm", () => {
  it("tells a rescued path from a prefixed one", () => {
    expect(onRootForm(at("/lab"), base)).toBe(true);
    expect(onRootForm(at("/proxy/abc/lab"), base)).toBe(false);
    expect(onRootForm(at("about:blank"), base)).toBe(false);
  });
});

describe("pageAddress", () => {
  it("names the app, not the proxy, and drops the query", () => {
    expect(pageAddress("localhost", 8888, "/lab/tree?token=secret")).toBe("http://localhost:8888/lab/tree");
    expect(pageAddress("node12", 8501, "/")).toBe("http://node12:8501/");
  });
  it("keeps a client-side route", () => {
    expect(pageAddress("localhost", 3000, "/app?x=1#/settings")).toBe("http://localhost:3000/app#/settings");
  });
  it("brackets an IPv6 host and roots a bare path", () => {
    expect(pageAddress("::1", 8000, "docs")).toBe("http://[::1]:8000/docs");
  });
});

describe("proxySessionOf", () => {
  const at = (href: string) => new URL(href, "http://127.0.0.1:7000");
  it("reads the session id and the app's own path", () => {
    expect(proxySessionOf(at("/proxy/p-2/lab/tree?x=1#h"))).toEqual({ id: "p-2", path: "/lab/tree?x=1#h" });
    expect(proxySessionOf(at("/proxy/p-2"))).toEqual({ id: "p-2", path: "/" });
  });
  it("is null off the proxy", () => {
    expect(proxySessionOf(at("/raw/t/a.html"))).toBeNull();
    expect(proxySessionOf(at("/lab"))).toBeNull();
  });
});
