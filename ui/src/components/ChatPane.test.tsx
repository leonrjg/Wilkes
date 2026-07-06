import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageBubble } from "./ChatPane";
import type { ChatMessage } from "../stores/useChatStore";

function message(overrides: Partial<ChatMessage>): ChatMessage {
  return {
    id: "message-1",
    role: "assistant",
    text: "",
    thought: "",
    streaming: false,
    error: null,
    tools: [],
    permissions: [],
    startedAtMs: null,
    endedAtMs: null,
    ...overrides,
  };
}

describe("MessageBubble", () => {
  it("renders assistant replies as GitHub-flavored Markdown", () => {
    render(
      <MessageBubble
        message={message({
          text: [
            "**Result**",
            "",
            "| Threshold | Precision |",
            "| --- | --- |",
            "| 50 | 100% |",
          ].join("\n"),
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    expect(screen.getByText("Result").tagName).toBe("STRONG");
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Threshold" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "100%" })).toBeInTheDocument();
  });

  it("keeps user messages as literal plain text", () => {
    render(
      <MessageBubble
        message={message({
          role: "user",
          text: "**literal**\n| not | a table |",
        })}
        nowMs={0}
        onNavigate={vi.fn()}
      />,
    );

    expect(screen.getByText(/\*\*literal\*\*/)).toBeInTheDocument();
    expect(screen.queryByText("literal")).not.toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });
});
