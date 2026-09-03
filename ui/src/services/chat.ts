import { tauriChatTransport } from "@leonrjg/wilkes-chat/tauri";

/** How the chat pane reaches its sessions.
 *
 *  The command names and the event channels are `@leonrjg/wilkes-chat`'s — it
 *  is what the Rust side registers them from — so this file names none of
 *  them. Wilkes used to hand-write the `invoke` calls beside a Rust crate that
 *  hand-wrote the handlers, which is how a renamed command becomes a runtime
 *  "command not found" that only shows up on one screen.
 *
 *  What Wilkes still says is *what the chat is about*, and that does not
 *  travel through here: it rides every call as the host blob the store
 *  supplies (see `useChatStore`), and the shell hands it to `WilkesChatHost`.
 */
export const chatTransport = tauriChatTransport();
