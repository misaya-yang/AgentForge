import type { FormEvent } from "react";

export function bindShell(node: HTMLElement | null) {
  node?.querySelectorAll("dialog[data-light]").forEach((dialog) => dialog.setAttribute("closedby", "any"));
  node?.querySelectorAll("[data-focusgroup]").forEach((group) => group.setAttribute("focusgroup", "menu nomemory"));
  node?.querySelectorAll("[data-command]").forEach((button) => {
    const [command, id] = (button.getAttribute("data-command") ?? "").split(":");
    if (!command || !id) return;
    button.setAttribute("command", command);
    button.setAttribute("commandfor", id);
  });
}

export function dialogForm(event: FormEvent<HTMLFormElement>) {
  event.preventDefault();
  const dialog = event.currentTarget.closest("dialog");
  const submitter = (event.nativeEvent as SubmitEvent).submitter;
  return { dialog, save: submitter?.getAttribute("value") === "save" };
}

export function closeDialog(dialog: HTMLDialogElement | null) {
  dialog?.close();
  if (dialog) dialog.returnValue = "";
}

export function openDialog(id: string) {
  const dialog = document.getElementById(id);
  if (dialog instanceof HTMLDialogElement && !dialog.open) dialog.showModal();
}

/** Feature-detect invoker commands and focusgroup. Native behavior stays untouched. */
export function installOverlayFallback(root: ParentNode) {
  if (!("commandForElement" in HTMLButtonElement.prototype)) {
    root.querySelectorAll<HTMLButtonElement>("button[command][commandfor]").forEach((button) => {
      if (button.dataset.spaceCommand === "1") return;
      button.dataset.spaceCommand = "1";
      button.addEventListener("click", () => {
        const target = document.getElementById(button.getAttribute("commandfor") ?? "");
        if (!target) return;
        const command = button.getAttribute("command");
        if (command === "show-modal" && target instanceof HTMLDialogElement && !target.open) target.showModal();
        if (command === "close" && target instanceof HTMLDialogElement) target.close();
        if (command === "toggle-popover" && target instanceof HTMLElement && "togglePopover" in target) target.togglePopover();
      });
    });
  }
  if ("focusGroup" in HTMLElement.prototype) return;
  root.querySelectorAll<HTMLElement>("[focusgroup]").forEach((group) => {
    if (group.dataset.spaceMenu === "1") return;
    group.dataset.spaceMenu = "1";
    group.setAttribute("role", "menu");
    const items = () => [...group.querySelectorAll<HTMLButtonElement>(":scope > button")];
    for (const item of items()) if (!item.getAttribute("role")) item.setAttribute("role", "menuitem");
    group.addEventListener("keydown", (event) => {
      const buttons = items();
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
      if (index < 0) return;
      const next = event.key === "ArrowDown" ? (index + 1) % buttons.length
        : event.key === "ArrowUp" ? (index - 1 + buttons.length) % buttons.length
          : event.key === "Home" ? 0
            : event.key === "End" ? buttons.length - 1
              : -1;
      if (next < 0) return;
      event.preventDefault();
      buttons[next]?.focus();
    });
  });
}
