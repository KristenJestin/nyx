import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

// Mock the toast helper so we can assert the section calls the right variant with the
// real reason — without mounting the whole toast manager/viewport.
const toastMock = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}));
vi.mock("@/components/ui/toast", () => ({ toast: toastMock }));

import { GlobalSection } from "./global-section";

/**
 * Coverage for the toast wiring of the project-settings Global pane: a successful
 * rename / resume toggle fires `toast.success`, and a backend refusal fires
 * `toast.error` with the REAL reason surfaced by `isBridgeError` (FEEDBACK #9/#10).
 */

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe("<GlobalSection> toast wiring — rename", () => {
  it("fires toast.success on a successful rename", async () => {
    const onRename = vi.fn().mockResolvedValue(undefined);
    render(
      <GlobalSection
        projectId="p1"
        projectName="Old"
        resumeAgentSessions={false}
        onRename={onRename}
        onResumeChange={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "New name" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onRename).toHaveBeenCalledWith("New name"));
    await waitFor(() => expect(toastMock.success).toHaveBeenCalledWith("Project renamed"));
    expect(toastMock.error).not.toHaveBeenCalled();
  });

  it("fires toast.error with the real backend reason on a refused rename", async () => {
    // A bridge `command` error: its `message` is the backend's real reason.
    const onRename = vi
      .fn()
      .mockRejectedValue({ kind: "command", message: "a project with that name already exists" });
    render(
      <GlobalSection
        projectId="p1"
        projectName="Old"
        resumeAgentSessions={false}
        onRename={onRename}
        onResumeChange={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByLabelText("Project name"), { target: { value: "Dup" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(toastMock.error).toHaveBeenCalledWith("a project with that name already exists"),
    );
    // The same reason is mirrored inline (proximity), not the generic fallback.
    expect(screen.getByRole("alert")).toHaveTextContent("a project with that name already exists");
    expect(toastMock.success).not.toHaveBeenCalled();
  });
});

describe("<GlobalSection> toast wiring — resume toggle", () => {
  it("fires toast.success when the resume toggle persists", async () => {
    const onResumeChange = vi.fn().mockResolvedValue(undefined);
    render(
      <GlobalSection
        projectId="p1"
        projectName="P"
        resumeAgentSessions={false}
        onRename={vi.fn().mockResolvedValue(undefined)}
        onResumeChange={onResumeChange}
      />,
    );

    fireEvent.click(screen.getByRole("switch"));

    await waitFor(() => expect(onResumeChange).toHaveBeenCalledWith(true));
    await waitFor(() =>
      expect(toastMock.success).toHaveBeenCalledWith("Agent-session resume enabled"),
    );
  });

  it("fires toast.error with the real reason when the toggle is refused", async () => {
    const onResumeChange = vi.fn().mockRejectedValue("backend said no");
    render(
      <GlobalSection
        projectId="p1"
        projectName="P"
        resumeAgentSessions={false}
        onRename={vi.fn().mockResolvedValue(undefined)}
        onResumeChange={onResumeChange}
      />,
    );

    fireEvent.click(screen.getByRole("switch"));

    await waitFor(() => expect(toastMock.error).toHaveBeenCalledWith("backend said no"));
  });
});
