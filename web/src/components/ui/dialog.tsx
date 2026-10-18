import * as React from "react";
import { Modal } from "antd";
import { cn } from "@/lib/utils";
import { childWithProps } from "./composition";

const DialogContext = React.createContext({ open: false, setOpen: (open: boolean) => { void open; }, titleId: "", descriptionId: "", alert: false });
function Dialog({ children, open: controlledOpen, defaultOpen = false, onOpenChange, alert = false }: {
  children?: React.ReactNode; open?: boolean; defaultOpen?: boolean; onOpenChange?: (open: boolean) => void; modal?: boolean; alert?: boolean;
}) {
  const [internalOpen, setInternalOpen] = React.useState(defaultOpen);
  const id = React.useId();
  const open = controlledOpen ?? internalOpen;
  const setOpen = React.useCallback((next: boolean) => { if (controlledOpen === undefined) setInternalOpen(next); onOpenChange?.(next); }, [controlledOpen, onOpenChange]);
  return <DialogContext.Provider value={{ open, setOpen, titleId: `${id}-title`, descriptionId: `${id}-description`, alert }}>{children}</DialogContext.Provider>;
}
const DialogTrigger = React.forwardRef<HTMLButtonElement, React.ButtonHTMLAttributes<HTMLButtonElement> & { asChild?: boolean }>(
  ({ asChild, children, onClick, ...props }, ref) => {
    const { open, setOpen } = React.useContext(DialogContext);
    const triggerProps = { ...props, ref, "aria-haspopup": "dialog" as const, "aria-expanded": open, onClick: (event: React.MouseEvent<HTMLButtonElement>) => { onClick?.(event); if (!event.defaultPrevented) setOpen(true); } };
    return asChild ? childWithProps(children, triggerProps) : <button type="button" {...triggerProps}>{children}</button>;
  });
DialogTrigger.displayName = "DialogTrigger";
const DialogClose = React.forwardRef<HTMLButtonElement, React.ButtonHTMLAttributes<HTMLButtonElement> & { asChild?: boolean }>(
  ({ asChild, children, onClick, ...props }, ref) => {
    const { setOpen } = React.useContext(DialogContext);
    const closeProps = { ...props, ref, onClick: (event: React.MouseEvent<HTMLButtonElement>) => { onClick?.(event); if (!event.defaultPrevented) setOpen(false); } };
    return asChild ? childWithProps(children, closeProps) : <button type="button" {...closeProps}>{children}</button>;
  });
DialogClose.displayName = "DialogClose";
function DialogPortal({ children }: { children?: React.ReactNode }) { return <>{children}</>; }
function DialogOverlay(_props: React.HTMLAttributes<HTMLDivElement>) { void _props; return null; }
interface DialogContentProps extends React.HTMLAttributes<HTMLDivElement> {
  showCloseButton?: boolean;
  onEscapeKeyDown?: (event: KeyboardEvent) => void;
  onInteractOutside?: (event: Event) => void;
  onPointerDownOutside?: (event: Event) => void;
  onOpenAutoFocus?: (event: Event) => void;
  onCloseAutoFocus?: (event: Event) => void;
}
const DialogContent = React.forwardRef<HTMLDivElement, DialogContentProps>(
  ({ children, className, showCloseButton = true, onEscapeKeyDown, onInteractOutside, onPointerDownOutside, onOpenAutoFocus, onCloseAutoFocus, ...props }, ref) => {
    const { open, setOpen, titleId, descriptionId, alert } = React.useContext(DialogContext);
    const returnFocus = React.useRef<HTMLElement | null>(null);
    const callbacks = React.useRef({ onOpenAutoFocus, onCloseAutoFocus });
    React.useLayoutEffect(() => { callbacks.current = { onOpenAutoFocus, onCloseAutoFocus }; });
    React.useLayoutEffect(() => {
      if (!open) return;
      returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      callbacks.current.onOpenAutoFocus?.(new Event("openAutoFocus", { cancelable: true }));
      return () => {
        const event = new Event("closeAutoFocus", { cancelable: true });
        callbacks.current.onCloseAutoFocus?.(event);
        if (!event.defaultPrevented) returnFocus.current?.focus();
      };
    }, [open]);
    return <Modal open={open} footer={null} title={null} centered width="auto" zIndex={120}
      closable={showCloseButton} keyboard={!alert} mask={{ closable: !alert }} destroyOnHidden
      focusable={{ focusTriggerAfterClose: false }}
      onCancel={event => {
        const native = event.nativeEvent;
        if (native instanceof KeyboardEvent) onEscapeKeyDown?.(native);
        else if (!(event.target as HTMLElement).closest(".ant-modal-close")) { onPointerDownOutside?.(native); onInteractOutside?.(native); }
        if (!native.defaultPrevented) setOpen(false);
      }}
      styles={{ container: { padding: 0, background: "transparent", boxShadow: "none" } }}
      panelRef={node => {
        node?.setAttribute("role", alert ? "alertdialog" : "dialog");
        node?.setAttribute("aria-labelledby", titleId);
        node?.setAttribute("aria-describedby", descriptionId);
      }}
    >
      <div {...props} ref={ref} className={cn("ui-dialog-slot-1", className)}>{children}</div>
    </Modal>;
  });
DialogContent.displayName = "DialogContent";
function DialogHeader({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) { return <div className={cn("ui-dialog-slot-2", className)} {...props} />; }
function DialogFooter({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) { return <div className={cn("ui-dialog-slot-3", className)} {...props} />; }
const DialogTitle = React.forwardRef<HTMLHeadingElement, React.HTMLAttributes<HTMLHeadingElement>>(({ className, ...props }, ref) => {
  const { titleId } = React.useContext(DialogContext);
  return <h2 ref={ref} id={titleId} className={cn("ui-dialog-slot-4", className)} {...props} />;
});
DialogTitle.displayName = "DialogTitle";
const DialogDescription = React.forwardRef<HTMLParagraphElement, React.HTMLAttributes<HTMLParagraphElement>>(({ className, ...props }, ref) => {
  const { descriptionId } = React.useContext(DialogContext);
  return <p ref={ref} id={descriptionId} className={cn("ui-dialog-slot-5", className)} {...props} />;
});
DialogDescription.displayName = "DialogDescription";
export { Dialog, DialogTrigger, DialogPortal, DialogClose, DialogOverlay, DialogContent, DialogHeader, DialogFooter, DialogTitle, DialogDescription };
