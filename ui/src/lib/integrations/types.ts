import type { ContextMenuItem } from "../../components/ContextMenu";
import type { SearchApi } from "../../services/api";
import type { ContextMenuTarget } from "../fileActions";
import type { Settings } from "../types";

export interface MenuContributorCtx {
  target: ContextMenuTarget;
  api: SearchApi;
  settings: Settings;
  onToast: (message: string, type: "success" | "error") => void;
}

export type MenuContributor = (ctx: MenuContributorCtx) => ContextMenuItem[];
