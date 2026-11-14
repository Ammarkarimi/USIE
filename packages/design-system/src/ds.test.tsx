// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { ConfidenceBadge, ErrorBanner, Proportions, StatusBadge, formatBytes, formatDuration, formatRelative } from "./index";

describe("format", () => {
  it("formats bytes, durations and relative times", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatDuration(250)).toBe("250 ms");
    expect(formatDuration(61_000)).toBe("1 min 1 s");
    const now = Date.parse("2026-01-01T12:00:00Z");
    expect(formatRelative("2026-01-01T11:00:00Z", now)).toBe("1 h ago");
    expect(formatRelative(null, now)).toBe("—");
  });
});

describe("components", () => {
  it("labels confidence with text and an explanation", () => {
    render(<ConfidenceBadge level="inferred" />);
    const badge = screen.getByText("inferred");
    expect(badge.getAttribute("title")).toMatch(/heuristic/);
  });

  it("renders status text without underscores", () => {
    render(<StatusBadge status="parsed_with_errors" />);
    expect(screen.getByText("parsed with errors")).toBeTruthy();
  });

  it("shows API error details and request id", () => {
    render(<ErrorBanner error={{ message: "boom", requestId: "abc" }} />);
    expect(screen.getByRole("alert").textContent).toContain("Request id: abc");
  });

  it("states proportions as text", () => {
    render(<Proportions items={[{ label: "a", value: 1, className: "" }, { label: "b", value: 3, className: "" }]} />);
    expect(screen.getByText(/b:/).textContent).toContain("75.0%");
  });
});
