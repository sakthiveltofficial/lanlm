import { describe, expect, it } from "vitest";
import { lendableMb, parseWorkerInput } from "./cluster";

describe("parseWorkerInput", () => {
  it("reads host:port and takes the RAM from the form field", () => {
    expect(parseWorkerInput("192.168.1.20:50052", 4)).toEqual({ endpoint: "192.168.1.20:50052", ramMb: 4096 });
  });
  it("reads a pairing code host:port@ramMb and ignores the form field", () => {
    expect(parseWorkerInput("192.168.1.5:50052@5120", 1)).toEqual({ endpoint: "192.168.1.5:50052", ramMb: 5120 });
  });
  it("accepts host names and trims whitespace", () => {
    expect(parseWorkerInput("  my-pc.local:50052  ", 2)).toEqual({ endpoint: "my-pc.local:50052", ramMb: 2048 });
  });
  it("rounds fractional gigabytes", () => {
    expect(parseWorkerInput("10.0.0.2:50052", 1.5)).toEqual({ endpoint: "10.0.0.2:50052", ramMb: 1536 });
  });
  it("rejects input without a port", () => {
    expect(parseWorkerInput("192.168.1.20", 4)).toHaveProperty("error");
  });
  it("rejects garbage and shell-like input", () => {
    expect(parseWorkerInput("192.168.1.20:50052; rm -rf /", 4)).toHaveProperty("error");
    expect(parseWorkerInput("", 4)).toHaveProperty("error");
    expect(parseWorkerInput("http://host:80", 4)).toHaveProperty("error");
  });
  it("rejects ports above 65535", () => {
    expect(parseWorkerInput("10.0.0.2:99999", 4)).toHaveProperty("error");
  });
  it("needs a positive amount of RAM", () => {
    expect(parseWorkerInput("10.0.0.2:50052", 0)).toHaveProperty("error");
    expect(parseWorkerInput("10.0.0.2:50052@0", 4)).toHaveProperty("error");
  });
});

describe("lendableMb", () => {
  it("lends three quarters of a worker's free RAM", () => {
    expect(lendableMb(4096)).toBe(3072);
  });
});
