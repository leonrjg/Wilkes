import type { MenuContributor } from "./types";
import { BookOpen } from "react-feather";

export const zoteroMenuContributor: MenuContributor = (ctx) => {
  if (!ctx.settings.integrations?.zotero.enabled) return [];
  if (ctx.target.kind === "directory") return [];

  return [
    {
      id: "zotero-add",
      label: "Add to Zotero",
      icon: BookOpen,
      run: async () => {
        try {
          const outcome = await ctx.api.zoteroAddItem(ctx.target.path);
          if (outcome.status === "already_present") {
            ctx.onToast("Already in Zotero", "success");
          } else if (outcome.status === "possible_duplicate") {
            ctx.onToast(outcome.message, "error");
          } else {
            ctx.onToast("Added to Zotero", "success");
          }
        } catch (error) {
          console.error("Failed to add file to Zotero:", error);
          ctx.onToast(errorMessage(error, "Failed to add to Zotero"), "error");
        }
      },
    },
  ];
};

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}
