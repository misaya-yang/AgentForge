/* eslint-disable react-refresh/only-export-components */
import { useEffect, useId, useRef, useState, type AriaAttributes } from "react";
import { createRoot } from "react-dom/client";
import type { ModalProps, ModalFuncProps } from "antd";
import { X } from "lucide-react";
import i18n from "@/i18n";

type Props = Omit<ModalProps, "panelRef"> & AriaAttributes & { panelRef?: (node: HTMLDialogElement | null) => void };

/** Compatibility at existing call sites; the browser owns modality and focus. */
export function NativeModal({ open = false, title, children, footer, onCancel, onOk, afterClose, afterOpenChange,
  closable = true, keyboard = true, maskClosable = true, mask = true, confirmLoading = false,
  okText, cancelText, okButtonProps, cancelButtonProps, width, className = "", style, styles, panelRef,
  destroyOnHidden, modalRender, ...aria }: Props) {
  const node = useRef<HTMLDialogElement>(null);
  const id = useId();
  const [mounted, setMounted] = useState(open);
  const callbacks = useRef({ afterClose, afterOpenChange, panelRef });
  callbacks.current = { afterClose, afterOpenChange, panelRef };
  const maskCanClose = mask !== false && (typeof mask === "object" ? mask.closable ?? maskClosable : maskClosable);
  const canClose = (typeof closable === "object" ? !closable.disabled : closable) !== false;
  const closedby = maskCanClose ? "any" : keyboard ? "closerequest" : "none";
  useEffect(() => {
    const dialog = node.current;
    if (!dialog) return;
    if (open && !dialog.open) { setMounted(true); dialog.showModal(); callbacks.current.afterOpenChange?.(true); }
    if (!open && dialog.open) dialog.close();
    callbacks.current.panelRef?.(dialog);
  }, [open]);
  function cancel(nativeEvent: Event) {
    onCancel?.({ nativeEvent, target: nativeEvent.target, preventDefault: () => nativeEvent.preventDefault() } as Parameters<NonNullable<ModalProps["onCancel"]>>[0]);
  }
  const defaultFooter = <>
    <button type="button" className="forge-button" disabled={cancelButtonProps?.disabled} onClick={event => cancel(event.nativeEvent)}>{cancelText ?? i18n.t("common.cancel", "Cancel")}</button>
    <button type="button" className="forge-button forge-button-primary" disabled={confirmLoading || okButtonProps?.disabled} aria-busy={confirmLoading} onClick={event => onOk?.(event)}>{okText ?? i18n.t("common.confirm", "Confirm")}</button>
  </>;
  const content = <>
    {title && <header className="forge-modal-header"><h2 id={`${id}-title`}>{title}</h2></header>}
    {canClose && <button type="button" className="forge-icon-button forge-modal-close" aria-label={i18n.t("common.close", "Close")} onClick={event => cancel(event.nativeEvent)}><X size={20} aria-hidden="true" /></button>}
    <div className="forge-modal-body" style={typeof styles === "object" ? styles.body : undefined}>{children}</div>
    {footer !== null && <footer className="forge-modal-footer">{typeof footer === "function" ? footer(defaultFooter, { OkBtn: () => <button type="button" className="forge-button forge-button-primary" disabled={confirmLoading || okButtonProps?.disabled} onClick={event => onOk?.(event)}>{okText ?? i18n.t("common.confirm", "Confirm")}</button>, CancelBtn: () => <button type="button" className="forge-button" disabled={cancelButtonProps?.disabled} onClick={event => cancel(event.nativeEvent)}>{cancelText ?? i18n.t("common.cancel", "Cancel")}</button> }) : footer ?? defaultFooter}</footer>}
  </>;
  const pressedOutside = useRef(false);
  const pointerDismissal = useRef<PointerEvent | null>(null);
  const outside = (event: { clientX: number; clientY: number }) => {
    const rect = node.current?.getBoundingClientRect();
    return rect && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom);
  };
  return <dialog ref={node} closedby={closedby} aria-labelledby={aria["aria-labelledby"] ?? (title ? `${id}-title` : undefined)} aria-label={aria["aria-label"] ?? (title ? undefined : i18n.t("common.confirm", "Confirm"))} aria-describedby={aria["aria-describedby"]}
    className={`forge-modal ${mask === false ? "forge-modal-clear-backdrop" : ""} ${className}`} style={{ ...style, width: typeof width === "number" || (typeof width === "string" && width !== "auto") ? width : undefined }}
    onCancel={event => {
      event.preventDefault();
      const pointer = pointerDismissal.current;
      pointerDismissal.current = null;
      if (pointer && maskCanClose && Math.abs(event.timeStamp - pointer.timeStamp) < 250) cancel(pointer);
      else if (keyboard) cancel(new KeyboardEvent("keydown", { key: "Escape", cancelable: true }));
    }}
    onClose={() => { callbacks.current.afterClose?.(); callbacks.current.afterOpenChange?.(false); if (destroyOnHidden) setMounted(false); }}
    onKeyDownCapture={event => { if (event.key === "Escape") pointerDismissal.current = null; }}
    onPointerDown={event => { pressedOutside.current = Boolean(outside(event)); }}
    onPointerUp={event => {
      // Native closedby handles current browsers. Geometry fallback avoids dismissing dialog padding or text-selection drags.
      if (pressedOutside.current && outside(event) && maskCanClose) {
        pointerDismissal.current = event.nativeEvent;
        if (!("closedBy" in HTMLDialogElement.prototype)) cancel(event.nativeEvent);
      }
      pressedOutside.current = false;
    }}>
    {(open || mounted || !destroyOnHidden) && (modalRender ? modalRender(content) : content)}
  </dialog>;
}

const instances = new Set<{ destroy: () => void }>();
function confirm(initial: ModalFuncProps) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  let config = initial, busy = false, destroyed = false;
  const close = () => {
    if (destroyed) return;
    destroyed = true; instances.delete(handle);
    container.querySelector("dialog")?.close();
    queueMicrotask(() => { root.unmount(); container.remove(); config.afterClose?.(); });
  };
  const handle = {
    destroy: close,
    update(next: Partial<ModalFuncProps> | ((previous: ModalFuncProps) => ModalFuncProps)) { config = typeof next === "function" ? next(config) : { ...config, ...next }; render(); },
  };
  function render() {
    if (destroyed) return;
    root.render(<NativeModal {...config} panelRef={undefined} open title={config.title} confirmLoading={busy} afterClose={undefined}
      onOk={async () => {
        if (busy) return;
        busy = true; render();
        try { await config.onOk?.(close); close(); }
        catch { /* The owner retains and presents its failed/uncertain operation. */ }
        finally { busy = false; render(); }
      }}
      onCancel={async () => { if (busy) return; try { await config.onCancel?.(close); close(); } catch { /* Keep protected work visible. */ } }}>
      {config.content}
    </NativeModal>);
  }
  instances.add(handle); render(); return handle;
}
function notice(config: ModalFuncProps) { return confirm({ ...config, footer: config.footer ?? ((_node, { OkBtn }) => <OkBtn />) }); }
export const Modal = Object.assign(NativeModal, { confirm, info: notice, success: notice, error: notice, warning: notice, warn: notice,
  destroyAll: () => { for (const instance of [...instances]) instance.destroy(); }, useModal: () => [{ confirm, info: notice, success: notice, error: notice, warning: notice }, null] as const });
